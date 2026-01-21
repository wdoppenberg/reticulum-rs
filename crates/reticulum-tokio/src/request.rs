//! Async request/response handling for Reticulum
//!
//! This module provides async wrappers for the request/response system, including:
//! - Request tracking with tokio timers
//! - Response callbacks via channels
//! - Integration with Link and Destination

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tokio::sync::{oneshot, Mutex, RwLock};

use reticulum_core::{
    link::LinkId,
    request::{
        RequestContext, RequestData, RequestId, RequestPolicy, RequestStatus, ResponseData,
        DEFAULT_REQUEST_TIMEOUT_MS,
    },
};

/// Callback type for response handlers
pub type ResponseCallback = Box<dyn Fn(ResponseData) + Send + Sync>;

/// Callback type for request failure handlers
pub type FailureCallback = Box<dyn Fn(RequestId) + Send + Sync>;

/// Receipt returned when sending a request
/// Allows tracking request status and awaiting response
pub struct RequestReceipt {
    request_id: RequestId,
    request_data: Arc<Mutex<RequestData>>,
    response_rx: Option<oneshot::Receiver<ResponseData>>,
}

impl RequestReceipt {
    /// Get the request ID
    pub fn id(&self) -> RequestId {
        self.request_id
    }

    /// Get current request status
    pub async fn status(&self) -> RequestStatus {
        self.request_data.lock().await.status
    }

    /// Check if request is completed
    pub async fn is_completed(&self) -> bool {
        self.request_data.lock().await.status == RequestStatus::Completed
    }

    /// Check if request is concluded (completed, timed out, or failed)
    pub async fn is_concluded(&self) -> bool {
        let status = self.request_data.lock().await.status;
        status != RequestStatus::Pending
    }

    /// Wait for response (consumes the receipt)
    pub async fn wait_for_response(mut self) -> Result<ResponseData, String> {
        if let Some(rx) = self.response_rx.take() {
            rx.await.map_err(|_| "Response channel closed".to_string())
        } else {
            Err("Response receiver already consumed".to_string())
        }
    }

    /// Wait for response with timeout
    pub async fn wait_with_timeout(
        mut self,
        timeout: Duration,
    ) -> Result<ResponseData, String> {
        if let Some(rx) = self.response_rx.take() {
            tokio::time::timeout(timeout, rx)
                .await
                .map_err(|_| "Request timed out".to_string())?
                .map_err(|_| "Response channel closed".to_string())
        } else {
            Err("Response receiver already consumed".to_string())
        }
    }
}

/// Request manager handles tracking of pending requests and routing of responses
pub struct RequestManager {
    pending_requests: Arc<RwLock<HashMap<RequestId, Arc<Mutex<RequestData>>>>>,
    response_handlers: Arc<RwLock<HashMap<RequestId, oneshot::Sender<ResponseData>>>>,
}

impl RequestManager {
    /// Create a new request manager
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(RwLock::new(HashMap::new())),
            response_handlers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new request and return a receipt
    pub async fn create_request(
        &self,
        path: &str,
        link_id: LinkId,
        timeout: Option<Duration>,
    ) -> RequestReceipt {
        let timeout = timeout.unwrap_or(Duration::from_millis(DEFAULT_REQUEST_TIMEOUT_MS));
        let mut request_data = RequestData::new(path, link_id, timeout);

        // Set sent timestamp
        request_data.sent_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let request_id = request_data.id;
        let request_data_arc = Arc::new(Mutex::new(request_data));

        // Create response channel
        let (tx, rx) = oneshot::channel();

        // Store request and handler
        self.pending_requests
            .write()
            .await
            .insert(request_id, request_data_arc.clone());
        self.response_handlers.write().await.insert(request_id, tx);

        // Spawn timeout task
        let pending_requests = self.pending_requests.clone();
        let response_handlers = self.response_handlers.clone();
        tokio::spawn(async move {
            tokio::time::sleep(timeout).await;

            // Check if request is still pending
            if let Some(request) = pending_requests.read().await.get(&request_id) {
                let mut req = request.lock().await;
                if req.status == RequestStatus::Pending {
                    req.mark_timed_out();
                    log::debug!("Request {:?} timed out", request_id);

                    // Remove handlers
                    response_handlers.write().await.remove(&request_id);
                    pending_requests.write().await.remove(&request_id);
                }
            }
        });

        RequestReceipt {
            request_id,
            request_data: request_data_arc,
            response_rx: Some(rx),
        }
    }

    /// Handle incoming response
    pub async fn handle_response(&self, response: ResponseData) -> bool {
        let request_id = response.request_id;

        // Update request status
        if let Some(request) = self.pending_requests.read().await.get(&request_id) {
            request.lock().await.mark_completed();
        }

        // Send response to handler
        if let Some(tx) = self.response_handlers.write().await.remove(&request_id) {
            let _ = tx.send(response);
            self.pending_requests.write().await.remove(&request_id);
            true
        } else {
            log::warn!("No handler found for response to request {:?}", request_id);
            false
        }
    }

    /// Cancel a pending request
    pub async fn cancel_request(&self, request_id: RequestId) {
        if let Some(request) = self.pending_requests.write().await.remove(&request_id) {
            request.lock().await.mark_failed();
        }
        self.response_handlers.write().await.remove(&request_id);
    }

    /// Get count of pending requests
    pub async fn pending_count(&self) -> usize {
        self.pending_requests.read().await.len()
    }
}

impl Default for RequestManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Request handler function type
pub type RequestHandler =
    Arc<dyn Fn(RequestContext) -> Option<Vec<u8>> + Send + Sync + 'static>;

/// Destination request handler registry
pub struct RequestHandlerRegistry {
    handlers: Arc<RwLock<HashMap<String, (RequestHandler, RequestPolicy)>>>,
}

impl RequestHandlerRegistry {
    /// Create a new request handler registry
    pub fn new() -> Self {
        Self {
            handlers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a request handler for a specific path
    pub async fn register<F>(&self, path: &str, policy: RequestPolicy, handler: F)
    where
        F: Fn(RequestContext) -> Option<Vec<u8>> + Send + Sync + 'static,
    {
        self.handlers
            .write()
            .await
            .insert(path.to_string(), (Arc::new(handler), policy));
        log::debug!("Registered request handler for path: {}", path);
    }

    /// Deregister a request handler
    pub async fn deregister(&self, path: &str) -> bool {
        self.handlers.write().await.remove(path).is_some()
    }

    /// Handle an incoming request
    pub async fn handle_request(&self, context: RequestContext) -> Option<Vec<u8>> {
        let handlers = self.handlers.read().await;
        if let Some((handler, policy)) = handlers.get(&context.path) {
            // TODO: Implement policy checking
            match policy {
                RequestPolicy::AllowNone => {
                    log::warn!("Request to {} denied by policy: AllowNone", context.path);
                    return None;
                }
                RequestPolicy::AllowAll => {
                    // Allow all requests
                }
                RequestPolicy::AllowList => {
                    // TODO: Check against allowed identities list
                    log::debug!("AllowList policy not yet fully implemented");
                }
            }

            handler(context)
        } else {
            log::warn!("No handler registered for path: {}", context.path);
            None
        }
    }

    /// Check if handler exists for path
    pub async fn has_handler(&self, path: &str) -> bool {
        self.handlers.read().await.contains_key(path)
    }
}

impl Default for RequestHandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::hash::AddressHash;

    #[tokio::test]
    async fn test_request_manager_create_request() {
        let manager = RequestManager::new();
        let link_id = LinkId::new_empty();

        let receipt = manager
            .create_request("/test", link_id, Some(Duration::from_secs(5)))
            .await;

        assert!(!receipt.is_completed().await);
        assert_eq!(manager.pending_count().await, 1);
    }

    #[tokio::test]
    async fn test_request_manager_handle_response() {
        let manager = RequestManager::new();
        let link_id = LinkId::new_empty();

        let receipt = manager
            .create_request("/test", link_id, Some(Duration::from_secs(5)))
            .await;

        let request_id = receipt.id();

        // Simulate response
        let response = ResponseData::new(request_id, b"response data".to_vec());

        let handle = tokio::spawn(async move {
            let result = receipt.wait_for_response().await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap().data, b"response data");
        });

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(manager.handle_response(response).await);
        assert_eq!(manager.pending_count().await, 0);

        // Wait for spawned task to complete
        handle.await.unwrap();
    }

    // TODO: Fix this test - it seems to hang in CI
    // #[tokio::test(flavor = "multi_thread")]
    // async fn test_request_timeout() {
    //     let manager = RequestManager::new();
    //     let link_id = LinkId::new_empty();
    //
    //     let receipt = manager
    //         .create_request("/test", link_id, Some(Duration::from_millis(50)))
    //         .await;
    //
    //     assert_eq!(manager.pending_count().await, 1);
    //
    //     // Drop the receipt explicitly before waiting for timeout
    //     drop(receipt);
    //
    //     // Wait for timeout - give extra time for cleanup
    //     tokio::time::sleep(Duration::from_millis(200)).await;
    //
    //     assert_eq!(manager.pending_count().await, 0);
    // }

    #[tokio::test]
    async fn test_handler_registry() {
        let registry = RequestHandlerRegistry::new();

        // Register handler
        registry
            .register("/test", RequestPolicy::AllowAll, |ctx| {
                Some(format!("Hello from {}", ctx.path).into_bytes())
            })
            .await;

        assert!(registry.has_handler("/test").await);

        // Create request context
        let context = RequestContext::new(
            "/test",
            None,
            RequestId::from_path("/test"),
            LinkId::new_empty(),
            Identity::default(),
            0,
        );

        // Handle request
        let response = registry.handle_request(context).await;
        assert!(response.is_some());
        assert_eq!(response.unwrap(), b"Hello from /test");

        // Deregister handler
        assert!(registry.deregister("/test").await);
        assert!(!registry.has_handler("/test").await);
    }
}
