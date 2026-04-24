use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use getrandom::SysRng;
use rand_core::TryRng;
use x25519_dalek::StaticSecret;

use reticulum_core::{
    destination::DestinationDesc,
    error::RnsError,
    hash::AddressHash,
    identity::{
        DecryptIdentity, DerivedKey, EncryptIdentity, Identity, PrivateIdentity, PUBLIC_KEY_LENGTH,
    },
    packet::{
        DestinationType, Header, Packet, PacketContext, PacketDataBuffer, PacketType, PACKET_MDU,
    },
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

// ─── Link state machine ───────────────────────────────────────────────────────

/// Internal state of a [`Link`].
///
/// Replaces the previous `established: Option<EstablishedLink>` + redundant
/// `status: LinkStatus` pair with a single source of truth.  Moving between
/// variants is the only way to change the link's cryptographic state.
///
/// `PrivateIdentity` is intentionally **not** `Clone` (key material must not
/// be silently duplicated), so transitions use [`std::mem::replace`] to move
/// the key out of the old variant and into the new one.
///
/// The enum lives inside `Arc<Mutex<Link>>`, so the stack-size difference
/// between variants is not observable to callers.
#[allow(clippy::large_enum_variant)]
enum LinkState {
    /// DH exchange not yet complete; proof not yet received.
    Pending { priv_identity: PrivateIdentity },

    /// Proof validated; shared key material is available for encryption.
    Active {
        priv_identity: PrivateIdentity,
        /// The remote peer's public identity (carried here so callers can read
        /// it without a separate look-up).
        #[allow(dead_code)]
        peer_identity: Identity,
        derived_key: DerivedKey,
    },

    /// Link has been closed; no further crypto operations are valid.
    Closed,
}

impl LinkState {
    fn status(&self) -> LinkStatus {
        match self {
            LinkState::Pending { .. } => LinkStatus::Pending,
            LinkState::Active { .. } => LinkStatus::Active,
            LinkState::Closed => LinkStatus::Closed,
        }
    }

    fn priv_identity(&self) -> Option<&PrivateIdentity> {
        match self {
            LinkState::Pending { priv_identity } | LinkState::Active { priv_identity, .. } => {
                Some(priv_identity)
            }
            LinkState::Closed => None,
        }
    }
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
    id: LinkId,
    destination: DestinationDesc,
    /// Single source of truth for the link's cryptographic lifecycle.
    state: LinkState,
    request_time: Instant,
    rtt: Duration,
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
            id: AddressHash::new_empty(),
            destination,
            state: LinkState::Pending {
                priv_identity: PrivateIdentity::try_new_from_rand(SysRng)?,
            },
            request_time: Instant::now(),
            rtt: Duration::from_secs(0),
            event_tx,
            data_tx,
        })
    }

    pub fn new_from_request(
        packet: &Packet,
        signing_key: SigningKey,
        destination: DestinationDesc,
        event_tx: tokio::sync::broadcast::Sender<LinkEventData>,
        data_tx: tokio::sync::broadcast::Sender<Arc<LinkDataEventData>>,
    ) -> Result<Self, RnsError> {
        if packet.data.len() < PUBLIC_KEY_LENGTH * 2 {
            return Err(RnsError::InvalidArgument);
        }

        let peer_identity = Identity::new_from_slices(
            &packet.data.as_slice()[..PUBLIC_KEY_LENGTH],
            &packet.data.as_slice()[PUBLIC_KEY_LENGTH..PUBLIC_KEY_LENGTH * 2],
        )
        .map_err(|_| RnsError::CryptoError)?;

        let link_id = LinkId::from(packet);
        log::debug!("link: create from request {}", link_id);

        let mut private_key_bytes = [0u8; PUBLIC_KEY_LENGTH];
        SysRng
            .try_fill_bytes(&mut private_key_bytes)
            .map_err(|_| RnsError::Randomness)?;
        let priv_identity =
            PrivateIdentity::new(StaticSecret::from(private_key_bytes), signing_key);
        let derived_key =
            priv_identity.derive_key(&peer_identity.public_key, Some(link_id.as_slice()));

        Ok(Self {
            id: link_id,
            destination,
            state: LinkState::Active {
                priv_identity,
                peer_identity,
                derived_key,
            },
            request_time: Instant::now(),
            rtt: Duration::from_secs(0),
            event_tx,
            data_tx,
        })
    }

    pub fn request(&mut self) -> Packet {
        let priv_identity = self
            .state
            .priv_identity()
            .expect("request() called on closed link");

        let mut packet_data = PacketDataBuffer::new();
        packet_data.safe_write(priv_identity.as_identity().public_key.as_bytes());
        packet_data.safe_write(priv_identity.as_identity().verifying_key.as_bytes());

        let packet = Packet {
            header: Header {
                packet_type: PacketType::LinkRequest,
                ..Default::default()
            },
            ifac: None,
            destination: self.destination.address_hash,
            transport: None,
            context: PacketContext::None,
            data: packet_data,
        };

        self.id = LinkId::from(&packet);
        self.request_time = Instant::now();

        packet
    }

    pub fn prove(&mut self) -> Result<Packet, RnsError> {
        log::debug!("link({}): prove", self.id);

        self.post_event(LinkEvent::Activated);

        let priv_identity = self
            .state
            .priv_identity()
            .expect("prove() called on closed link");

        let mut packet_data = PacketDataBuffer::new();

        packet_data.safe_write(self.id.as_slice());
        packet_data.safe_write(priv_identity.as_identity().public_key.as_bytes());
        packet_data.safe_write(priv_identity.as_identity().verifying_key.as_bytes());

        let signature = priv_identity.sign(packet_data.as_slice())?;

        packet_data.reset();
        packet_data.safe_write(&signature.to_bytes()[..]);
        packet_data.safe_write(priv_identity.as_identity().public_key.as_bytes());

        Ok(Packet {
            header: Header {
                packet_type: PacketType::Proof,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context: PacketContext::LinkRequestProof,
            data: packet_data,
        })
    }

    /// Complete the DH key exchange for outgoing links (proof received from
    /// remote).  Transitions `Pending → Active` atomically.
    fn activate(&mut self, peer_identity: Identity) {
        let old = std::mem::replace(&mut self.state, LinkState::Closed);
        let priv_identity = match old {
            LinkState::Pending { priv_identity } => priv_identity,
            LinkState::Active { priv_identity, .. } => {
                // Re-activation should not happen, but handle gracefully.
                log::warn!(
                    "link({}): activate() called on already-active link",
                    self.id
                );
                priv_identity
            }
            LinkState::Closed => {
                log::error!("link({}): activate() called on closed link", self.id);
                return;
            }
        };
        let derived_key =
            priv_identity.derive_key(&peer_identity.public_key, Some(self.id.as_slice()));
        self.rtt = self.request_time.elapsed();
        self.state = LinkState::Active {
            priv_identity,
            peer_identity,
            derived_key,
        };
    }

    fn handle_data_packet(&mut self, packet: &Packet) -> LinkHandleResult {
        match packet.context {
            PacketContext::None => {
                let mut buffer = [0u8; PACKET_MDU];
                if let Ok(plain_text) = self.decrypt(packet.data.as_slice(), &mut buffer[..]) {
                    log::trace!("link({}): data {}B", self.id, plain_text.len());
                    self.request_time = Instant::now();
                    self.post_data(DataKind::Data, PacketContext::None, plain_text);
                } else {
                    log::error!("link({}): can't decrypt packet", self.id);
                }
            }
            PacketContext::Channel => {
                let mut buffer = [0u8; PACKET_MDU];
                if let Ok(plain_text) = self.decrypt(packet.data.as_slice(), &mut buffer[..]) {
                    log::trace!("link({}): channel data {}B", self.id, plain_text.len());
                    self.request_time = Instant::now();
                    self.post_data(DataKind::ChannelData, PacketContext::Channel, plain_text);
                } else {
                    log::error!("link({}): can't decrypt channel packet", self.id);
                }
            }
            ctx @ (PacketContext::Resource
            | PacketContext::ResourceAdvrtisement
            | PacketContext::ResourceRequest
            | PacketContext::ResourceHashUpdate
            | PacketContext::ResourceProof
            | PacketContext::ResourceInitiatorCancel
            | PacketContext::ResourceReceiverCancel) => {
                let mut buffer = [0u8; PACKET_MDU];
                if let Ok(plain_text) = self.decrypt(packet.data.as_slice(), &mut buffer[..]) {
                    log::trace!(
                        "link({}): resource data {}B ctx={:?}",
                        self.id,
                        plain_text.len(),
                        ctx
                    );
                    self.request_time = Instant::now();
                    self.post_data(DataKind::ResourceData, ctx, plain_text);
                } else {
                    log::error!("link({}): can't decrypt resource packet", self.id);
                }
            }
            PacketContext::KeepAlive => {
                if !packet.data.is_empty() && packet.data.as_slice()[0] == 0xFF {
                    self.request_time = Instant::now();
                    log::trace!("link({}): keep-alive request", self.id);
                    return LinkHandleResult::KeepAlive;
                }
                if !packet.data.is_empty() && packet.data.as_slice()[0] == 0xFE {
                    log::trace!("link({}): keep-alive response", self.id);
                    self.request_time = Instant::now();
                    return LinkHandleResult::None;
                }
            }
            _ => {}
        }

        LinkHandleResult::None
    }

    pub fn handle_packet(&mut self, packet: &Packet) -> LinkHandleResult {
        if packet.destination != self.id {
            return LinkHandleResult::None;
        }

        match packet.header.packet_type {
            PacketType::Data => return self.handle_data_packet(packet),
            PacketType::Proof => {
                if self.state.status() == LinkStatus::Pending
                    && packet.context == PacketContext::LinkRequestProof
                {
                    match LinkHandshake::new(self.id).validate_proof(
                        packet.data.as_slice(),
                        &self.destination.identity.verifying_key,
                    ) {
                        Ok(proved) => {
                            log::debug!("link({}): has been proved", self.id);
                            self.activate(proved.into_peer_identity());
                            log::debug!("link({}): activated", self.id);
                            self.post_event(LinkEvent::Activated);
                            return LinkHandleResult::Activated;
                        }
                        Err(_) => {
                            log::warn!("link({}): proof is not valid", self.id);
                        }
                    }
                }
            }
            _ => {}
        }

        LinkHandleResult::None
    }

    /// Build a data packet, encrypting `data` with the link's derived key.
    /// Returns `Err(InvalidArgument)` if the link is not yet active.
    pub fn data_packet(&self, data: &[u8]) -> Result<Packet, RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };

        let mut packet_data = PacketDataBuffer::new();
        let cipher_text_len = {
            let cipher_text =
                priv_identity.encrypt(SysRng, data, derived_key, packet_data.acquire_buf_max())?;
            cipher_text.len()
        };
        packet_data.resize(cipher_text_len);

        Ok(Packet {
            header: Header {
                destination_type: DestinationType::Link,
                packet_type: PacketType::Data,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context: PacketContext::None,
            data: packet_data,
        })
    }

    pub fn channel_packet(&self, data: &[u8]) -> Result<Packet, RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };

        let mut packet_data = PacketDataBuffer::new();
        let cipher_text_len = {
            let cipher_text =
                priv_identity.encrypt(SysRng, data, derived_key, packet_data.acquire_buf_max())?;
            cipher_text.len()
        };
        packet_data.resize(cipher_text_len);

        Ok(Packet {
            header: Header {
                destination_type: DestinationType::Link,
                packet_type: PacketType::Data,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context: PacketContext::Channel,
            data: packet_data,
        })
    }

    pub fn resource_packet(&self, data: &[u8], context: PacketContext) -> Result<Packet, RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };

        let mut packet_data = PacketDataBuffer::new();
        let cipher_text_len = {
            let cipher_text =
                priv_identity.encrypt(SysRng, data, derived_key, packet_data.acquire_buf_max())?;
            cipher_text.len()
        };
        packet_data.resize(cipher_text_len);

        Ok(Packet {
            header: Header {
                destination_type: DestinationType::Link,
                packet_type: PacketType::Data,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context,
            data: packet_data,
        })
    }

    pub fn keep_alive_packet(&self, data: u8) -> Packet {
        log::trace!("link({}): create keep alive {}", self.id, data);

        let mut packet_data = PacketDataBuffer::new();
        packet_data.safe_write(&[data]);

        Packet {
            header: Header {
                destination_type: DestinationType::Link,
                packet_type: PacketType::Data,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context: PacketContext::KeepAlive,
            data: packet_data,
        }
    }

    pub fn encrypt<'a>(&self, text: &[u8], out_buf: &'a mut [u8]) -> Result<&'a [u8], RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };
        priv_identity.encrypt(SysRng, text, derived_key, out_buf)
    }

    pub fn decrypt<'a>(&self, text: &[u8], out_buf: &'a mut [u8]) -> Result<&'a [u8], RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };
        priv_identity.decrypt(SysRng, text, derived_key, out_buf)
    }

    pub fn destination(&self) -> &DestinationDesc {
        &self.destination
    }

    pub fn create_rtt(&self) -> Result<Packet, RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };

        let rtt = self.rtt.as_secs_f32();
        let mut buf = Vec::with_capacity(4);
        rmp::encode::write_f32(&mut buf, rtt).unwrap();

        let mut packet_data = PacketDataBuffer::new();
        let token_len = {
            let token = priv_identity.encrypt(
                SysRng,
                buf.as_slice(),
                derived_key,
                packet_data.acquire_buf_max(),
            )?;
            token.len()
        };
        packet_data.resize(token_len);

        log::trace!("link: {} create rtt packet = {} sec", self.id, rtt);

        Ok(Packet {
            header: Header {
                destination_type: DestinationType::Link,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context: PacketContext::LinkRTT,
            data: packet_data,
        })
    }

    fn post_event(&self, event: LinkEvent) {
        let _ = self.event_tx.send(LinkEventData {
            id: self.id,
            address_hash: self.destination.address_hash,
            event,
        });
    }

    fn post_data(&self, kind: DataKind, context: PacketContext, plain_text: &[u8]) {
        let _ = self.data_tx.send(Arc::new(LinkDataEventData {
            id: self.id,
            address_hash: self.destination.address_hash,
            frame: LinkDataFrame {
                kind,
                context,
                payload: LinkPayload::new_from_slice(plain_text),
            },
        }));
    }

    pub fn close(&mut self) {
        self.state = LinkState::Closed;
        self.post_event(LinkEvent::Closed);
        log::warn!("link: close {}", self.id);
    }

    /// Restart the link: drop the derived key material and revert to Pending,
    /// keeping the same private identity so the link ID can be reused.
    pub fn restart(&mut self) {
        log::warn!(
            "link({}): restart after {}s",
            self.id,
            self.request_time.elapsed().as_secs()
        );

        let old = std::mem::replace(&mut self.state, LinkState::Closed);
        let priv_identity = match old {
            LinkState::Active { priv_identity, .. } | LinkState::Pending { priv_identity } => {
                priv_identity
            }
            LinkState::Closed => match PrivateIdentity::try_new_from_rand(SysRng) {
                Ok(identity) => identity,
                Err(_) => {
                    log::error!("link({}): restart failed: RNG unavailable", self.id);
                    return;
                }
            },
        };
        self.state = LinkState::Pending { priv_identity };
    }

    pub fn elapsed(&self) -> Duration {
        self.request_time.elapsed()
    }

    pub fn status(&self) -> LinkStatus {
        self.state.status()
    }

    pub fn id(&self) -> &LinkId {
        &self.id
    }

    pub fn rtt_ms(&self) -> u32 {
        (self.rtt.as_millis() as u32).max(25)
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reticulum_core::destination::DestinationName;
    use reticulum_core::destination::SingleInputDestination;
    use reticulum_core::hash::AddressHash;
    use reticulum_core::identity::PrivateIdentity;
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
        // restart from Closed uses a freshly generated key
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn new_from_request_is_active() {
        let (event_tx, _) = broadcast::channel(4);
        let (data_tx, _) = broadcast::channel(4);

        // Build a fake link-request packet (two x25519 public keys).
        let requester_id = PrivateIdentity::try_new_from_rand(SysRng).expect("system RNG");
        let responder_id = PrivateIdentity::try_new_from_rand(SysRng).expect("system RNG");
        let responder_dest =
            SingleInputDestination::new(responder_id, DestinationName::new("test", "link"));

        // Build a packet that looks like a link request.
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
