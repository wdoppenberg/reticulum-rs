use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::tcp_client::TcpClient;
use crate::iface::InterfaceManager;

/// Listens for inbound TCP connections and registers each accepted client as an
/// independent interface with the `InterfaceManager`.
///
/// Unlike the data interfaces, `TcpServer` is not itself a
/// `reticulum_core::Interface` — it is a *listener* that dynamically creates
/// `TcpClient` interfaces.  Spawn it with [`TcpServer::run`].
pub struct TcpServer {
    addr: String,
}

impl TcpServer {
    pub fn new<T: Into<String>>(addr: T) -> Self {
        Self { addr: addr.into() }
    }

    /// Bind to the configured address and accept connections until the
    /// cancellation token fires.
    ///
    /// Each accepted connection is wrapped in a [`TcpClient`] and registered
    /// with `iface_manager` via `spawn_interface`.
    pub async fn run(self, iface_manager: Arc<Mutex<InterfaceManager>>, cancel: CancellationToken) {
        loop {
            if cancel.is_cancelled() {
                break;
            }

            let listener = match TcpListener::bind(&self.addr).await {
                Ok(l) => {
                    log::info!("tcp_server: listening on <{}>", self.addr);
                    l
                }
                Err(e) => {
                    log::warn!("tcp_server: couldn't bind to <{}>: {e}", self.addr);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                log::info!(
                                    "tcp_server: new client <{peer_addr}> on <{}>",
                                    self.addr
                                );
                                let client = TcpClient::from_stream(
                                    peer_addr.to_string(),
                                    stream,
                                );
                                iface_manager.lock().await.spawn_interface(client);
                            }
                            Err(e) => {
                                log::warn!("tcp_server: accept error: {e}");
                                break; // re-bind
                            }
                        }
                    }
                }
            }
        }
    }
}
