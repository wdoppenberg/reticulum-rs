use std::sync::Arc;
use std::time::{Duration, Instant};

use getrandom::SysRng;

use reticulum_core::{
    destination::DestinationDesc,
    error::RnsError,
    hash::AddressHash,
    link::{LinkCore, LinkOutcome},
    packet::{Packet, PacketContext, PacketDataBuffer},
};

// Re-export core link types so downstream crates only need to import from `crate::link`.
pub use reticulum_core::link::{
    DataKind, LinkDataFrame, LinkEvent, LinkHandleResult, LinkHandshake, LinkId, LinkPayload,
    LinkStatus,
};

// ─── Event envelope (control plane) ──────────────────────────────────────────

/// Control-plane event broadcast to all subscribers of a link bus.
/// Contains only cheap-to-clone state (no payload bytes).
#[derive(Clone)]
pub struct LinkEventData {
    pub id: LinkId,
    pub address_hash: AddressHash,
    pub event: LinkEvent,
}

// ─── Data envelope (data plane) ──────────────────────────────────────────────

/// Data-plane frame envelope.  Wrapped in `Arc` so that the
/// `broadcast::Sender` only copies an 8-byte pointer per subscriber —
/// not the full payload.
pub struct LinkDataEventData {
    pub id: LinkId,
    pub address_hash: AddressHash,
    pub frame: LinkDataFrame,
}

// ─── ActiveLink capability token ─────────────────────────────────────────────

/// Proof that a [`Link`] has completed its DH handshake and is ready for
/// encrypted data exchange.
///
/// Constructable only inside this crate (the transport issues one when it
/// validates a link-request proof).  [`Channel::new`](crate::channel::Channel)
/// accepts this type instead of a raw `Arc<Mutex<Link>>`, making it a
/// **compile-time** guarantee that channels cannot be opened on pending links.
#[derive(Clone)]
pub struct ActiveLink {
    /// Cached at construction so callers never need to lock the mutex just to
    /// read the ID.
    id: LinkId,
    inner: Arc<tokio::sync::Mutex<Link>>,
}

impl ActiveLink {
    /// Only constructable from within this crate.
    pub(crate) fn new(inner: Arc<tokio::sync::Mutex<Link>>, id: LinkId) -> Self {
        Self { id, inner }
    }

    pub fn id(&self) -> LinkId {
        self.id
    }

    pub async fn rtt_ms(&self) -> u32 {
        self.inner.lock().await.rtt_ms()
    }

    /// Build a channel-data packet from `data` (already-serialised envelope).
    pub async fn channel_packet(&self, data: &[u8]) -> Result<Packet, RnsError> {
        self.inner.lock().await.channel_packet(data)
    }

    pub async fn destination(&self) -> DestinationDesc {
        *self.inner.lock().await.destination()
    }
}

// ─── Link ─────────────────────────────────────────────────────────────────────

pub struct Link {
    /// Protocol state machine: handshake, encryption, packet construction.
    core: LinkCore,
    /// Tracks last activity time for both RTT measurement and staleness detection.
    request_time: Instant,
    /// Control-plane sender: Activated / Closed.
    event_tx: tokio::sync::broadcast::Sender<LinkEventData>,
    /// Data-plane sender: Arc-wrapped so broadcast clones only a pointer.
    data_tx: tokio::sync::broadcast::Sender<Arc<LinkDataEventData>>,
}

impl Link {
    pub fn new(
        destination: DestinationDesc,
        event_tx: tokio::sync::broadcast::Sender<LinkEventData>,
        data_tx: tokio::sync::broadcast::Sender<Arc<LinkDataEventData>>,
    ) -> Result<Self, RnsError> {
        Ok(Self {
            core: LinkCore::new_outgoing(destination, SysRng)?,
            request_time: Instant::now(),
            event_tx,
            data_tx,
        })
    }

    pub fn new_from_request(
        packet: &Packet,
        signing_key: ed25519_dalek::SigningKey,
        destination: DestinationDesc,
        event_tx: tokio::sync::broadcast::Sender<LinkEventData>,
        data_tx: tokio::sync::broadcast::Sender<Arc<LinkDataEventData>>,
    ) -> Result<Self, RnsError> {
        Ok(Self {
            core: LinkCore::new_incoming(packet, signing_key, destination, SysRng)?,
            request_time: Instant::now(),
            event_tx,
            data_tx,
        })
    }

    pub fn request(&mut self) -> Packet {
        self.request_time = Instant::now();
        self.core.request_packet()
    }

    pub fn prove(&mut self) -> Result<Packet, RnsError> {
        let packet = self.core.prove_packet()?;
        self.post_event(LinkEvent::Activated);
        Ok(packet)
    }

    pub fn handle_packet(&mut self, packet: &Packet) -> LinkHandleResult {
        let elapsed_ms = self.request_time.elapsed().as_millis() as u32;

        match self.core.handle_packet(SysRng, packet) {
            LinkOutcome::Activated => {
                self.core.set_rtt_ms(elapsed_ms.max(25));
                self.request_time = Instant::now();
                log::debug!("link({}): proved and activated", self.core.id());
                self.post_event(LinkEvent::Activated);
                LinkHandleResult::Activated
            }
            LinkOutcome::DataReceived(frame) => {
                self.request_time = Instant::now();
                self.post_data_frame(frame);
                LinkHandleResult::None
            }
            LinkOutcome::KeepAlive => {
                self.request_time = Instant::now();
                LinkHandleResult::KeepAlive
            }
            LinkOutcome::None => LinkHandleResult::None,
        }
    }

    pub fn data_packet(&self, data: &[u8]) -> Result<Packet, RnsError> {
        self.core.data_packet(SysRng, data)
    }

    pub fn channel_packet(&self, data: &[u8]) -> Result<Packet, RnsError> {
        self.core.channel_packet(SysRng, data)
    }

    pub fn resource_packet(&self, data: &[u8], context: PacketContext) -> Result<Packet, RnsError> {
        self.core.resource_packet(SysRng, data, context)
    }

    pub fn keep_alive_packet(&self, data: u8) -> Packet {
        self.core.keep_alive_packet(data)
    }

    pub fn encrypt<'a>(&self, text: &[u8], out_buf: &'a mut [u8]) -> Result<&'a [u8], RnsError> {
        self.core.encrypt(SysRng, text, out_buf)
    }

    pub fn decrypt<'a>(&self, text: &[u8], out_buf: &'a mut [u8]) -> Result<&'a [u8], RnsError> {
        self.core.decrypt(SysRng, text, out_buf)
    }

    pub fn create_rtt(&self) -> Result<Packet, RnsError> {
        let rtt = self.core.rtt_ms() as f32 / 1000.0;
        let mut buf = Vec::with_capacity(4);
        rmp::encode::write_f32(&mut buf, rtt).unwrap();

        let mut packet_data = PacketDataBuffer::new();
        let token_len = {
            let token = self
                .core
                .encrypt(SysRng, buf.as_slice(), packet_data.acquire_buf_max())?;
            token.len()
        };
        packet_data.resize(token_len);

        log::trace!("link: {} create rtt packet = {} sec", self.core.id(), rtt);

        use reticulum_core::packet::{DestinationType, Header};
        Ok(Packet {
            header: Header {
                destination_type: DestinationType::Link,
                ..Default::default()
            },
            ifac: None,
            destination: *self.core.id(),
            transport: None,
            context: PacketContext::LinkRTT,
            data: packet_data,
        })
    }

    fn post_event(&self, event: LinkEvent) {
        let _ = self.event_tx.send(LinkEventData {
            id: *self.core.id(),
            address_hash: self.core.destination().address_hash,
            event,
        });
    }

    fn post_data_frame(&self, frame: LinkDataFrame) {
        let _ = self.data_tx.send(Arc::new(LinkDataEventData {
            id: *self.core.id(),
            address_hash: self.core.destination().address_hash,
            frame,
        }));
    }

    pub fn close(&mut self) {
        self.core.close();
        self.post_event(LinkEvent::Closed);
    }

    pub fn restart(&mut self) {
        log::warn!(
            "link({}): restart after {}s",
            self.core.id(),
            self.request_time.elapsed().as_secs()
        );
        self.core.restart(SysRng);
    }

    pub fn elapsed(&self) -> Duration {
        self.request_time.elapsed()
    }

    pub fn status(&self) -> LinkStatus {
        self.core.status()
    }

    pub fn id(&self) -> &LinkId {
        self.core.id()
    }

    pub fn rtt_ms(&self) -> u32 {
        self.core.rtt_ms()
    }

    pub fn destination(&self) -> &DestinationDesc {
        self.core.destination()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::destination::DestinationName;
    use reticulum_core::destination::SingleInputDestination;
    use reticulum_core::identity::PrivateIdentity;
    use reticulum_core::packet::{Header, PacketType};
    use tokio::sync::broadcast;

    fn make_link() -> Link {
        let (event_tx, _) = broadcast::channel(4);
        let (data_tx, _) = broadcast::channel(4);
        let identity = PrivateIdentity::try_new_from_rand(SysRng).expect("system RNG");
        let dest = SingleInputDestination::new(identity, DestinationName::new("test", "link"));
        Link::new(dest.desc, event_tx, data_tx).expect("link")
    }

    #[test]
    fn new_link_is_pending() {
        let link = make_link();
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn close_transitions_to_closed() {
        let mut link = make_link();
        link.close();
        assert_eq!(link.status(), LinkStatus::Closed);
    }

    #[test]
    fn channel_packet_on_pending_returns_error() {
        let link = make_link();
        assert!(link.channel_packet(b"hello").is_err());
    }

    #[test]
    fn data_packet_on_pending_returns_error() {
        let link = make_link();
        assert!(link.data_packet(b"hello").is_err());
    }

    #[test]
    fn restart_from_pending_stays_pending() {
        let mut link = make_link();
        link.restart();
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn restart_from_closed_becomes_pending() {
        let mut link = make_link();
        link.close();
        link.restart();
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn new_from_request_is_active() {
        let (event_tx, _) = broadcast::channel(4);
        let (data_tx, _) = broadcast::channel(4);

        let requester_id = PrivateIdentity::try_new_from_rand(SysRng).expect("system RNG");
        let responder_id = PrivateIdentity::try_new_from_rand(SysRng).expect("system RNG");
        let responder_dest =
            SingleInputDestination::new(responder_id, DestinationName::new("test", "link"));

        let mut packet_data = PacketDataBuffer::new();
        packet_data.safe_write(requester_id.as_identity().public_key.as_bytes());
        packet_data.safe_write(requester_id.as_identity().verifying_key.as_bytes());

        let packet = Packet {
            header: Header {
                packet_type: PacketType::LinkRequest,
                ..Default::default()
            },
            ifac: None,
            destination: responder_dest.desc.address_hash,
            transport: None,
            context: PacketContext::None,
            data: packet_data,
        };

        let signing_key = responder_dest.sign_key().clone();
        let link =
            Link::new_from_request(&packet, signing_key, responder_dest.desc, event_tx, data_tx)
                .expect("new_from_request");

        assert_eq!(link.status(), LinkStatus::Active);
    }
}
