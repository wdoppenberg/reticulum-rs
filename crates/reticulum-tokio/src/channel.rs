//! Async Channel runtime for reliable bidirectional messaging over Links.
//!
//! Wraps the `reticulum-core` sliding-window Channel state machine in a
//! tokio-compatible API that integrates with the Transport's link event bus.
//!
//! # Usage
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use tokio::sync::Mutex;
//! // After a Link is established:
//! // let (channel, mut receiver) = Channel::new(link, transport, event_rx, LinkSpeed::Medium);
//! // channel.send(MessageType::new(1), b"hello").await?;
//! // if let Some(msg) = receiver.recv().await {
//! //     println!("got msg type={}", msg.msg_type.as_u16());
//! // }
//! ```

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

use reticulum_core::buffer::StaticBuffer;
use reticulum_core::channel::state::{RxRing, TxRing};
use reticulum_core::channel::types::{
    Envelope, LinkSpeed, MessageType, SequenceNumber, MAX_ENVELOPE_SIZE,
};
use reticulum_core::error::RnsError;

use crate::link::{Link, LinkEvent, LinkEventData, LinkId};
use crate::transport::Transport;

/// Message type reserved for channel ACKs (matching Python implementation).
pub const MSG_TYPE_CHANNEL_ACK: u16 = 0xFFFF;

/// How often the retransmit task wakes to check for timed-out messages.
const RETRANSMIT_INTERVAL: Duration = Duration::from_millis(50);

/// Capacity of the inbound message queue.
const INBOUND_QUEUE_CAP: usize = 64;

#[derive(Debug)]
pub enum ChannelError {
    LinkClosed,
    WindowFull,
    Packet(RnsError),
    Closed,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::LinkClosed => write!(f, "link is closed"),
            ChannelError::WindowFull => write!(f, "TX window full"),
            ChannelError::Packet(e) => write!(f, "packet error: {:?}", e),
            ChannelError::Closed => write!(f, "channel is closed"),
        }
    }
}

/// A delivered inbound channel message.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub msg_type: MessageType,
    pub payload: Vec<u8>,
}

/// Receiving end of a [`Channel`]. Call [`recv`](ChannelReceiver::recv) to
/// get the next in-order message delivered by the sliding-window protocol.
pub struct ChannelReceiver {
    rx: mpsc::Receiver<InboundMessage>,
}

impl ChannelReceiver {
    pub async fn recv(&mut self) -> Option<InboundMessage> {
        self.rx.recv().await
    }
}

/// Async Channel over an established Reticulum Link.
///
/// Created via [`Channel::new`]. The channel manages its own background tasks
/// for receiving, ACKing, and retransmitting messages.
pub struct Channel {
    link_id: LinkId,
    link: Arc<Mutex<Link>>,
    transport: Arc<Transport>,
    tx_ring: Arc<Mutex<TxRing>>,
    rtt_ms: Arc<Mutex<u32>>,
    cancel: CancellationToken,
}

impl Channel {
    /// Attach a Channel to an already-established link.
    ///
    /// `event_rx` should be a subscription to the transport's `link_in_event_tx`
    /// (or the out-link equivalent). The caller obtains this from
    /// `transport.subscribe_link_events()`.
    pub async fn new(
        link: Arc<Mutex<Link>>,
        transport: Arc<Transport>,
        event_rx: broadcast::Receiver<LinkEventData>,
        link_speed: LinkSpeed,
    ) -> (Channel, ChannelReceiver) {
        let link_id = *link.lock().await.id();

        let tx_ring = Arc::new(Mutex::new(TxRing::new(link_speed)));
        let rx_ring = Arc::new(Mutex::new(RxRing::new()));
        let rtt_ms = Arc::new(Mutex::new(link.lock().await.rtt_ms()));

        let (inbound_tx, inbound_rx) = mpsc::channel(INBOUND_QUEUE_CAP);
        let cancel = CancellationToken::new();

        // Receiver task: process incoming channel data events.
        tokio::spawn({
            let link = link.clone();
            let transport = transport.clone();
            let tx_ring = tx_ring.clone();
            let rx_ring = rx_ring.clone();
            let rtt_ms = rtt_ms.clone();
            let cancel = cancel.clone();
            async move {
                run_receiver(
                    link_id,
                    link,
                    transport,
                    tx_ring,
                    rx_ring,
                    rtt_ms,
                    inbound_tx,
                    event_rx,
                    cancel,
                )
                .await;
            }
        });

        // Retransmit task: periodically flush pending/timed-out TX messages.
        tokio::spawn({
            let link = link.clone();
            let transport = transport.clone();
            let tx_ring = tx_ring.clone();
            let rtt_ms = rtt_ms.clone();
            let cancel = cancel.clone();
            async move {
                run_retransmit(link_id, link, transport, tx_ring, rtt_ms, cancel).await;
            }
        });

        let channel = Channel {
            link_id,
            link,
            transport,
            tx_ring,
            rtt_ms,
            cancel,
        };

        let receiver = ChannelReceiver { rx: inbound_rx };
        (channel, receiver)
    }

    /// Send a message over the channel.
    ///
    /// Pushes the message into the TX ring and immediately attempts to send
    /// any ready messages (new or timed-out retransmits).
    pub async fn send(
        &self,
        msg_type: MessageType,
        payload: &[u8],
    ) -> Result<SequenceNumber, ChannelError> {
        if self.cancel.is_cancelled() {
            return Err(ChannelError::Closed);
        }

        let seq = self
            .tx_ring
            .lock()
            .await
            .push(msg_type, payload)
            .map_err(|_| ChannelError::WindowFull)?;

        // Eagerly flush new messages without waiting for the retransmit tick.
        flush_tx(
            self.link_id,
            &self.link,
            &self.transport,
            &self.tx_ring,
            &self.rtt_ms,
        )
        .await
        .map_err(ChannelError::Packet)?;

        Ok(seq)
    }

    /// Shut down background tasks for this channel.
    pub fn close(&self) {
        self.cancel.cancel();
    }

    pub fn link_id(&self) -> LinkId {
        self.link_id
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────────────────────────────────────

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Flush ready messages from `tx_ring` through the link.
///
/// Returns `Err` only on a hard packet-construction failure.
async fn flush_tx(
    link_id: LinkId,
    link: &Arc<Mutex<Link>>,
    transport: &Arc<Transport>,
    tx_ring: &Arc<Mutex<TxRing>>,
    rtt_ms: &Arc<Mutex<u32>>,
) -> Result<(), RnsError> {
    let now = now_ms();
    let rtt = *rtt_ms.lock().await;

    // Collect envelopes to send while holding the TxRing lock, then release it
    // before touching the link/transport locks.
    let to_send: Vec<Vec<u8>> = {
        let mut ring = tx_ring.lock().await;
        let ring_len = ring.len();
        let seqs = ring.messages_to_send(now);
        let mut out = Vec::with_capacity(seqs.len());
        for seq in seqs {
            if let Some(entry) = ring.get_mut(seq) {
                let packed: StaticBuffer<MAX_ENVELOPE_SIZE> = entry.envelope.pack();
                let data = packed.as_slice().to_vec();
                entry.mark_sent(now, rtt, ring_len);
                out.push(data);
            }
        }
        out
    };

    for data in to_send {
        let packet = link
            .lock()
            .await
            .channel_packet(&data)?;
        transport.send_packet(packet).await;
        log::trace!("channel({}): tx {} bytes", link_id, data.len());
    }

    Ok(())
}

/// Send a channel ACK for `acked_seq` back through the link.
async fn send_ack(
    link_id: LinkId,
    link: &Arc<Mutex<Link>>,
    transport: &Arc<Transport>,
    acked_seq: SequenceNumber,
) {
    // ACK envelope: msg_type=0xFFFF, seq=acked_seq, empty payload.
    let ack_type = MessageType::new(MSG_TYPE_CHANNEL_ACK);
    match Envelope::<MAX_ENVELOPE_SIZE>::new(ack_type, acked_seq, &[]) {
        Ok(env) => {
            let packed = env.pack();
            match link.lock().await.channel_packet(packed.as_slice()) {
                Ok(pkt) => {
                    transport.send_packet(pkt).await;
                    log::trace!("channel({}): sent ACK for seq={}", link_id, acked_seq);
                }
                Err(e) => {
                    log::warn!("channel({}): ACK packet error: {:?}", link_id, e);
                }
            }
        }
        Err(e) => {
            log::warn!("channel({}): ACK envelope error: {:?}", link_id, e);
        }
    }
}

async fn run_receiver(
    link_id: LinkId,
    link: Arc<Mutex<Link>>,
    transport: Arc<Transport>,
    tx_ring: Arc<Mutex<TxRing>>,
    rx_ring: Arc<Mutex<RxRing>>,
    rtt_ms: Arc<Mutex<u32>>,
    inbound_tx: mpsc::Sender<InboundMessage>,
    mut event_rx: broadcast::Receiver<LinkEventData>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = event_rx.recv() => {
                match result {
                    Ok(event_data) => {
                        if event_data.id != link_id {
                            continue;
                        }
                        match event_data.event {
                            LinkEvent::ChannelData(payload) => {
                                handle_channel_data(
                                    link_id,
                                    payload.as_slice(),
                                    &link,
                                    &transport,
                                    &tx_ring,
                                    &rx_ring,
                                    &rtt_ms,
                                    &inbound_tx,
                                ).await;
                            }
                            LinkEvent::Closed => {
                                log::debug!("channel({}): link closed", link_id);
                                break;
                            }
                            _ => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        log::warn!("channel({}): event receiver lagged by {}", link_id, n);
                    }
                }
            }
        }
    }
    log::debug!("channel({}): receiver task exited", link_id);
}

async fn handle_channel_data(
    link_id: LinkId,
    data: &[u8],
    link: &Arc<Mutex<Link>>,
    transport: &Arc<Transport>,
    tx_ring: &Arc<Mutex<TxRing>>,
    rx_ring: &Arc<Mutex<RxRing>>,
    rtt_ms: &Arc<Mutex<u32>>,
    inbound_tx: &mpsc::Sender<InboundMessage>,
) {
    let envelope = match Envelope::<MAX_ENVELOPE_SIZE>::unpack(data) {
        Ok(e) => e,
        Err(_) => {
            log::warn!("channel({}): failed to unpack envelope ({} bytes)", link_id, data.len());
            return;
        }
    };

    let msg_type = envelope.msg_type;
    let seq = envelope.sequence;

    // ACK from remote: advance our TX ring.
    if msg_type.as_u16() == MSG_TYPE_CHANNEL_ACK {
        let acked = tx_ring.lock().await.acknowledge(seq);
        if acked {
            log::trace!("channel({}): remote ACKed seq={}", link_id, seq);
            // Opportunistically flush any new messages now that the window has grown.
            let _ = flush_tx(link_id, link, transport, tx_ring, rtt_ms).await;
        }
        return;
    }

    // Normal message: feed into RX ring for in-order delivery.
    let ready = match rx_ring.lock().await.receive(envelope, now_ms()) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("channel({}): rx ring error: {:?}", link_id, e);
            return;
        }
    };

    for env in ready {
        let acked_seq = env.sequence;
        let payload = env.payload_slice().to_vec();
        let msg = InboundMessage {
            msg_type: env.msg_type,
            payload,
        };
        if inbound_tx.send(msg).await.is_err() {
            // Receiver was dropped; shut down.
            log::debug!("channel({}): inbound queue closed, stopping receiver", link_id);
            return;
        }
        // Send ACK for each delivered message.
        send_ack(link_id, link, transport, acked_seq).await;
    }
}

async fn run_retransmit(
    link_id: LinkId,
    link: Arc<Mutex<Link>>,
    transport: Arc<Transport>,
    tx_ring: Arc<Mutex<TxRing>>,
    rtt_ms: Arc<Mutex<u32>>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = sleep(RETRANSMIT_INTERVAL) => {
                // Mark failures first, then flush (which retransmits timed-out messages).
                {
                    let mut ring = tx_ring.lock().await;
                    ring.check_failures(now_ms());
                }
                let _ = flush_tx(link_id, &link, &transport, &tx_ring, &rtt_ms).await;
            }
        }
    }
    log::debug!("channel({}): retransmit task exited", link_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::channel::types::{Envelope, MessageType, SequenceNumber, MAX_ENVELOPE_SIZE};

    #[test]
    fn ack_envelope_round_trips() {
        let ack_type = MessageType::new(MSG_TYPE_CHANNEL_ACK);
        let seq = SequenceNumber::new(42);
        let env: Envelope<MAX_ENVELOPE_SIZE> =
            Envelope::new(ack_type, seq, &[]).expect("ack envelope");
        let packed = env.pack();
        let unpacked: Envelope<MAX_ENVELOPE_SIZE> =
            Envelope::unpack(packed.as_slice()).expect("unpack");
        assert_eq!(unpacked.msg_type.as_u16(), MSG_TYPE_CHANNEL_ACK);
        assert_eq!(unpacked.sequence, seq);
        assert!(unpacked.payload_slice().is_empty());
    }
}
