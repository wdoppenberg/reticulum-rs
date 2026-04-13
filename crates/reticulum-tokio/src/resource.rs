//! Async Resource transfer runtime for sending and receiving large data over Links.
//!
//! Wraps the `reticulum-core` Resource state machine in a tokio-compatible API
//! that integrates with the Transport's link event bus.
//!
//! # Transfer overview
//!
//! **Sender** side – call [`send_resource`]:
//! ```no_run
//! // tokio::spawn(send_resource(data, link, transport, event_rx, cancel));
//! ```
//!
//! **Receiver** side – listen for `LinkEvent::ResourceData(_, ResourceAdvrtisement)`,
//! then create and drive a [`ResourceReceiver`]:
//! ```no_run
//! // let rx = ResourceReceiver::new(adv_bytes, link, transport, cancel).await?;
//! // let data = rx.wait(event_rx).await?;
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, Mutex};
use tokio_util::sync::CancellationToken;

use reticulum_core::packet::PacketContext;
use reticulum_core::resource::{
    Resource, ResourceAdvertisement, ResourcePart, ResourceStatus, MAPHASH_LEN,
};

use crate::link::{DataKind, Link, LinkDataEventData, LinkEvent, LinkEventData, LinkId};
use crate::transport::Transport;

/// Timeout for waiting on a hash-update or proof from the receiver.
const RECEIVER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Timeout for waiting on the next resource part from the sender.
const PART_RECEIVE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait in each poll loop iteration.
const POLL_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub enum ResourceError {
    Advertisement(String),
    Transfer(String),
    Timeout,
    LinkClosed,
    Cancelled,
}

impl std::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResourceError::Advertisement(s) => write!(f, "advertisement error: {}", s),
            ResourceError::Transfer(s) => write!(f, "transfer error: {}", s),
            ResourceError::Timeout => write!(f, "transfer timed out"),
            ResourceError::LinkClosed => write!(f, "link closed during transfer"),
            ResourceError::Cancelled => write!(f, "transfer cancelled"),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Sender
// ──────────────────────────────────────────────────────────────────────────────

/// Send `data` over an established link as a Reticulum Resource transfer.
///
/// This function runs the entire sender protocol and returns when the transfer
/// has completed (proof received), failed, or been cancelled. Callers should
/// `tokio::spawn` it for concurrent use.
pub async fn send_resource(
    data: Vec<u8>,
    link: Arc<Mutex<Link>>,
    transport: Arc<Transport>,
    mut event_rx: broadcast::Receiver<LinkEventData>,
    mut data_rx: broadcast::Receiver<Arc<LinkDataEventData>>,
    cancel: CancellationToken,
) -> Result<(), ResourceError> {
    let link_id = *link.lock().await.id();
    let sdu = compute_sdu();

    let mut resource = Resource::new_outgoing(&data, sdu, false)
        .map_err(|e| ResourceError::Advertisement(e.to_string()))?;

    let parts: Vec<ResourcePart> = resource.segment_data(&data);

    // Build advertisement hashmap (up to HASHMAP_MAX_LEN bytes worth of hashes).
    let max_hashes = ResourceAdvertisement::HASHMAP_MAX_LEN / MAPHASH_LEN;
    let hashmap_bytes: Vec<u8> = parts
        .iter()
        .take(max_hashes)
        .flat_map(|p| p.map_hash.iter().copied())
        .collect();

    let adv = ResourceAdvertisement {
        flags: resource.flags,
        size: resource.size,
        total_size: resource.total_size,
        uncompressed_size: resource.uncompressed_size,
        hash: resource.hash,
        original_hash: resource.original_hash,
        random_hash: resource.random_hash,
        hashmap: hashmap_bytes,
        segment_index: resource.segment_index,
        total_segments: resource.total_segments,
        has_metadata: resource.has_metadata,
    };

    let adv_bytes = adv.pack();
    let adv_packet = link
        .lock()
        .await
        .resource_packet(&adv_bytes, PacketContext::ResourceAdvrtisement)
        .map_err(|e| ResourceError::Advertisement(format!("{:?}", e)))?;

    transport.send_packet(adv_packet).await;
    resource.set_status(ResourceStatus::Advertised);

    log::debug!(
        "resource_sender({}): advertised {} bytes, {} parts",
        link_id,
        resource.size,
        parts.len()
    );

    // Send the initial window of parts.
    resource.set_status(ResourceStatus::Transferring);
    send_window(&mut resource, &parts, &link, &transport, link_id).await?;

    let mut last_activity = Instant::now();

    loop {
        if cancel.is_cancelled() {
            send_resource_packet(
                &link,
                &transport,
                &[],
                PacketContext::ResourceInitiatorCancel,
            )
            .await;
            resource.set_status(ResourceStatus::Failed);
            return Err(ResourceError::Cancelled);
        }

        if last_activity.elapsed() > RECEIVER_RESPONSE_TIMEOUT {
            log::warn!("resource_sender({}): receiver response timeout", link_id);
            resource.set_status(ResourceStatus::Failed);
            return Err(ResourceError::Timeout);
        }

        tokio::select! {
            _ = tokio::time::sleep(POLL_TIMEOUT) => continue, // re-check cancel/global timeout
            result = event_rx.recv() => {
                match result {
                    Ok(ev) if ev.id == link_id => {
                        if let LinkEvent::Closed = ev.event {
                            resource.set_status(ResourceStatus::Failed);
                            return Err(ResourceError::LinkClosed);
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        resource.set_status(ResourceStatus::Failed);
                        return Err(ResourceError::LinkClosed);
                    }
                }
            }
            result = data_rx.recv() => {
                match result {
                    Ok(fd) if fd.id == link_id && fd.frame.kind == DataKind::ResourceData => {
                        match fd.frame.context {
                            PacketContext::ResourceHashUpdate => {
                                last_activity = Instant::now();
                                handle_hash_update_sender(
                                    &mut resource,
                                    &parts,
                                    fd.frame.payload.as_slice(),
                                    &link,
                                    &transport,
                                    link_id,
                                )
                                .await;
                                if resource.is_complete() {
                                    break;
                                }
                            }
                            PacketContext::ResourceProof => {
                                log::debug!(
                                    "resource_sender({}): proof received, transfer complete",
                                    link_id
                                );
                                resource.set_status(ResourceStatus::Complete);
                                return Ok(());
                            }
                            PacketContext::ResourceReceiverCancel => {
                                log::info!("resource_sender({}): receiver cancelled", link_id);
                                resource.set_status(ResourceStatus::Failed);
                                return Err(ResourceError::Cancelled);
                            }
                            _ => {}
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {
                        resource.set_status(ResourceStatus::Failed);
                        return Err(ResourceError::LinkClosed);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn send_window(
    resource: &mut Resource,
    parts: &[ResourcePart],
    link: &Arc<Mutex<Link>>,
    transport: &Arc<Transport>,
    link_id: LinkId,
) -> Result<(), ResourceError> {
    let indices = resource.request_next_window();
    for idx in indices {
        if idx >= parts.len() {
            continue;
        }
        let data = &parts[idx].data;
        let packet = link
            .lock()
            .await
            .resource_packet(data, PacketContext::Resource)
            .map_err(|e| ResourceError::Transfer(format!("{:?}", e)))?;
        transport.send_packet(packet).await;
        log::trace!(
            "resource_sender({}): sent part {} ({} bytes)",
            link_id,
            idx,
            data.len()
        );
    }
    Ok(())
}

async fn handle_hash_update_sender(
    resource: &mut Resource,
    parts: &[ResourcePart],
    data: &[u8],
    link: &Arc<Mutex<Link>>,
    transport: &Arc<Transport>,
    link_id: LinkId,
) {
    if data.len() < 2 {
        return;
    }
    let consecutive_height = u16::from_be_bytes([data[0], data[1]]) as usize;
    let missing_hashes = &data[2..];
    let num_missing = missing_hashes.len() / MAPHASH_LEN;

    log::debug!(
        "resource_sender({}): hash update - consecutive={}, missing={}",
        link_id,
        consecutive_height,
        num_missing
    );

    resource.adjust_window(num_missing == 0);

    // Retransmit parts identified by their map hashes.
    for i in 0..num_missing {
        let start = i * MAPHASH_LEN;
        let end = start + MAPHASH_LEN;
        if end > missing_hashes.len() {
            break;
        }
        let mut target = [0u8; MAPHASH_LEN];
        target.copy_from_slice(&missing_hashes[start..end]);

        for part in parts {
            if part.map_hash == target {
                if let Ok(packet) = link
                    .lock()
                    .await
                    .resource_packet(&part.data, PacketContext::Resource)
                {
                    transport.send_packet(packet).await;
                    log::trace!(
                        "resource_sender({}): retransmitted part {}",
                        link_id,
                        part.index
                    );
                }
                break;
            }
        }
    }
}

async fn send_resource_packet(
    link: &Arc<Mutex<Link>>,
    transport: &Arc<Transport>,
    payload: &[u8],
    context: PacketContext,
) {
    if let Ok(packet) = link.lock().await.resource_packet(payload, context) {
        transport.send_packet(packet).await;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Receiver
// ──────────────────────────────────────────────────────────────────────────────

/// Drives a resource transfer from the receiving side.
///
/// Create from an incoming advertisement payload obtained via
/// `LinkEvent::ResourceData(payload, PacketContext::ResourceAdvrtisement)`,
/// then call [`wait`](ResourceReceiver::wait) to drive the protocol to
/// completion and obtain the assembled data.
pub struct ResourceReceiver {
    link_id: LinkId,
    link: Arc<Mutex<Link>>,
    transport: Arc<Transport>,
    resource: Resource,
    cancel: CancellationToken,
}

impl ResourceReceiver {
    /// Create a receiver from a raw advertisement payload.
    pub async fn new(
        adv_bytes: &[u8],
        link: Arc<Mutex<Link>>,
        transport: Arc<Transport>,
        cancel: CancellationToken,
    ) -> Result<Self, ResourceError> {
        let adv = ResourceAdvertisement::unpack(adv_bytes)
            .map_err(|e| ResourceError::Advertisement(e.to_string()))?;

        let link_id = *link.lock().await.id();
        let sdu = compute_sdu();

        let mut resource = Resource::new_incoming(&adv, sdu);
        resource.update_hashmap(0, &adv.hashmap);

        log::debug!(
            "resource_receiver({}): incoming {} bytes, {} parts",
            link_id,
            resource.size,
            resource.total_parts
        );

        Ok(Self {
            link_id,
            link,
            transport,
            resource,
            cancel,
        })
    }

    /// Drive the receive protocol to completion, returning the assembled data.
    ///
    /// `event_rx` carries control events (Activated, Closed).
    /// `data_rx` carries data frames (Arc-wrapped for zero payload copies).
    pub async fn wait(
        mut self,
        mut event_rx: broadcast::Receiver<LinkEventData>,
        mut data_rx: broadcast::Receiver<Arc<LinkDataEventData>>,
    ) -> Result<Vec<u8>, ResourceError> {
        let mut last_activity = Instant::now();

        loop {
            if self.cancel.is_cancelled() {
                self.send_cancel().await;
                return Err(ResourceError::Cancelled);
            }

            if last_activity.elapsed() > PART_RECEIVE_TIMEOUT {
                self.send_cancel().await;
                return Err(ResourceError::Timeout);
            }

            tokio::select! {
                _ = tokio::time::sleep(POLL_TIMEOUT) => continue, // re-check cancel/global timeout
                result = event_rx.recv() => {
                    match result {
                        Ok(ev) if ev.id == self.link_id => {
                            if let LinkEvent::Closed = ev.event {
                                return Err(ResourceError::LinkClosed);
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return Err(ResourceError::LinkClosed),
                    }
                }
                result = data_rx.recv() => {
                    match result {
                        Ok(fd) if fd.id == self.link_id && fd.frame.kind == DataKind::ResourceData => {
                            match fd.frame.context {
                                PacketContext::Resource => {
                                    last_activity = Instant::now();
                                    match self.resource.receive_part(fd.frame.payload.as_slice().to_vec()) {
                                        Ok(complete) => {
                                            if complete {
                                                return self.finish().await;
                                            }
                                            if self.resource.outstanding_parts == 0 {
                                                self.send_hash_update().await;
                                            }
                                        }
                                        Err(e) => {
                                            log::warn!(
                                                "resource_receiver({}): receive_part: {}",
                                                self.link_id,
                                                e
                                            );
                                            self.send_hash_update().await;
                                        }
                                    }
                                }
                                PacketContext::ResourceInitiatorCancel => {
                                    log::info!("resource_receiver({}): initiator cancelled", self.link_id);
                                    return Err(ResourceError::Cancelled);
                                }
                                _ => {}
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return Err(ResourceError::LinkClosed),
                    }
                }
            }
        }
    }

    async fn finish(self) -> Result<Vec<u8>, ResourceError> {
        let data = self
            .resource
            .assemble()
            .map_err(|e| ResourceError::Transfer(e.to_string()))?;

        // Send proof: hash of the assembled resource.
        let proof_payload = self.resource.hash.as_bytes().to_vec();
        if let Ok(packet) = self
            .link
            .lock()
            .await
            .resource_packet(&proof_payload, PacketContext::ResourceProof)
        {
            self.transport.send_packet(packet).await;
        }

        log::debug!(
            "resource_receiver({}): complete ({} bytes)",
            self.link_id,
            data.len()
        );

        Ok(data)
    }

    async fn send_hash_update(&self) {
        let missing = self.resource.get_missing_parts();
        let consecutive_height = self
            .resource
            .consecutive_completed_height
            .map(|h| h as u16)
            .unwrap_or(0u16);

        let mut payload = Vec::with_capacity(2 + missing.len() * MAPHASH_LEN);
        payload.extend_from_slice(&consecutive_height.to_be_bytes());

        for idx in &missing {
            if *idx < self.resource.hashmap.len() {
                if let Some(hash) = self.resource.hashmap[*idx] {
                    payload.extend_from_slice(&hash);
                }
            }
        }

        log::trace!(
            "resource_receiver({}): hash update, {} missing",
            self.link_id,
            missing.len()
        );

        send_resource_packet(
            &self.link,
            &self.transport,
            &payload,
            PacketContext::ResourceHashUpdate,
        )
        .await;
    }

    async fn send_cancel(&self) {
        send_resource_packet(
            &self.link,
            &self.transport,
            &[],
            PacketContext::ResourceReceiverCancel,
        )
        .await;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn compute_sdu() -> usize {
    // PACKET_MDU minus Fernet encryption overhead (~16 bytes).
    const ENCRYPTION_OVERHEAD: usize = 16;
    const MIN_SDU: usize = 64;
    reticulum_core::packet::PACKET_MDU
        .saturating_sub(ENCRYPTION_OVERHEAD)
        .max(MIN_SDU)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::hash::Hash;
    use reticulum_core::resource::{Resource, ResourceAdvertisement, ResourceFlags};

    #[test]
    fn advertisement_round_trip() {
        let data = vec![0xABu8; 512];
        let sdu = 128;
        let resource = Resource::new_outgoing(&data, sdu, false).unwrap();
        let parts = resource.segment_data(&data);

        let hashmap_bytes: Vec<u8> = parts
            .iter()
            .flat_map(|p| p.map_hash.iter().copied())
            .collect();

        let adv = ResourceAdvertisement {
            flags: ResourceFlags::default(),
            size: data.len(),
            total_size: data.len(),
            uncompressed_size: data.len(),
            hash: Hash::new_from_slice(&data),
            original_hash: Hash::new_from_slice(&data),
            random_hash: [0u8; 4],
            hashmap: hashmap_bytes,
            segment_index: 1,
            total_segments: 1,
            has_metadata: false,
        };

        let packed = adv.pack();
        let unpacked = ResourceAdvertisement::unpack(&packed).unwrap();
        assert_eq!(unpacked.size, data.len());
        assert_eq!(unpacked.segment_index, 1);
        assert_eq!(unpacked.total_segments, 1);
    }

    #[test]
    fn sdu_is_sensible() {
        let sdu = compute_sdu();
        assert!(sdu >= 64);
        assert!(sdu <= reticulum_core::packet::PACKET_MDU);
    }
}
