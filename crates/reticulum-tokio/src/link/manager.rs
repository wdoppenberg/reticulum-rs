use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use rand_core::OsRng;
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

// ─── Established link state ───────────────────────────────────────────────────

/// Key material that only exists once a link proof has been validated.
///
/// Keeping this in its own struct makes it impossible to use `derived_key`
/// before the DH exchange completes: callers that need crypto go through
/// `Link::established()`, which returns `None` on `Pending` links.
struct EstablishedLink {
    peer_identity: Identity,
    derived_key: DerivedKey,
}

// ─── Link ─────────────────────────────────────────────────────────────────────

pub struct Link {
    id: LinkId,
    destination: DestinationDesc,
    priv_identity: PrivateIdentity,
    /// `None` while the link is `Pending` (proof not yet validated).
    /// `Some` once the DH exchange has completed — the only path to
    /// constructing this is through `Link::handshake`, which calls
    /// `LinkHandshake<AwaitingProof>::validate_proof`.
    established: Option<EstablishedLink>,
    status: LinkStatus,
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
    ) -> Self {
        Self {
            id: AddressHash::new_empty(),
            destination,
            priv_identity: PrivateIdentity::new_from_rand(OsRng),
            established: None,
            status: LinkStatus::Pending,
            request_time: Instant::now(),
            rtt: Duration::from_secs(0),
            event_tx,
            data_tx,
        }
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

        let mut link = Self {
            id: link_id,
            destination,
            priv_identity: PrivateIdentity::new(StaticSecret::random_from_rng(OsRng), signing_key),
            established: None,
            status: LinkStatus::Pending,
            request_time: Instant::now(),
            rtt: Duration::from_secs(0),
            event_tx,
            data_tx,
        };

        link.handshake(peer_identity);

        Ok(link)
    }

    pub fn request(&mut self) -> Packet {
        let mut packet_data = PacketDataBuffer::new();

        packet_data.safe_write(self.priv_identity.as_identity().public_key.as_bytes());
        packet_data.safe_write(self.priv_identity.as_identity().verifying_key.as_bytes());

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

        self.status = LinkStatus::Pending;
        self.id = LinkId::from(&packet);
        self.request_time = Instant::now();

        packet
    }

    pub fn prove(&mut self) -> Packet {
        log::debug!("link({}): prove", self.id);

        if self.status != LinkStatus::Active {
            self.status = LinkStatus::Active;
            self.post_event(LinkEvent::Activated);
        }

        let mut packet_data = PacketDataBuffer::new();

        packet_data.safe_write(self.id.as_slice());
        packet_data.safe_write(self.priv_identity.as_identity().public_key.as_bytes());
        packet_data.safe_write(self.priv_identity.as_identity().verifying_key.as_bytes());

        let signature = self.priv_identity.sign(packet_data.as_slice());

        packet_data.reset();
        packet_data.safe_write(&signature.to_bytes()[..]);
        packet_data.safe_write(self.priv_identity.as_identity().public_key.as_bytes());

        Packet {
            header: Header {
                packet_type: PacketType::Proof,
                ..Default::default()
            },
            ifac: None,
            destination: self.id,
            transport: None,
            context: PacketContext::LinkRequestProof,
            data: packet_data,
        }
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
                if packet.data.len() >= 1 && packet.data.as_slice()[0] == 0xFF {
                    self.request_time = Instant::now();
                    log::trace!("link({}): keep-alive request", self.id);
                    return LinkHandleResult::KeepAlive;
                }
                if packet.data.len() >= 1 && packet.data.as_slice()[0] == 0xFE {
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
                if self.status == LinkStatus::Pending
                    && packet.context == PacketContext::LinkRequestProof
                {
                    match LinkHandshake::new(self.id).validate_proof(
                        packet.data.as_slice(),
                        &self.destination.identity.verifying_key,
                    ) {
                        Ok(proved) => {
                            log::debug!("link({}): has been proved", self.id);

                            self.handshake(proved.into_peer_identity());

                            self.status = LinkStatus::Active;
                            self.rtt = self.request_time.elapsed();

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

    /// Build a data packet. Returns `Err(InvalidArgument)` if the link is not
    /// yet active — callers must not attempt to encrypt before `Activated`.
    pub fn data_packet(&self, data: &[u8]) -> Result<Packet, RnsError> {
        let established = self.established.as_ref().ok_or(RnsError::InvalidArgument)?;

        let mut packet_data = PacketDataBuffer::new();
        let cipher_text_len = {
            let cipher_text = self.priv_identity.encrypt(
                OsRng,
                data,
                &established.derived_key,
                packet_data.acquire_buf_max(),
            )?;
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
        let established = self.established.as_ref().ok_or(RnsError::InvalidArgument)?;

        let mut packet_data = PacketDataBuffer::new();
        let cipher_text_len = {
            let cipher_text = self.priv_identity.encrypt(
                OsRng,
                data,
                &established.derived_key,
                packet_data.acquire_buf_max(),
            )?;
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
        let established = self.established.as_ref().ok_or(RnsError::InvalidArgument)?;

        let mut packet_data = PacketDataBuffer::new();
        let cipher_text_len = {
            let cipher_text = self.priv_identity.encrypt(
                OsRng,
                data,
                &established.derived_key,
                packet_data.acquire_buf_max(),
            )?;
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
        let established = self.established.as_ref().ok_or(RnsError::InvalidArgument)?;
        self.priv_identity
            .encrypt(OsRng, text, &established.derived_key, out_buf)
    }

    pub fn decrypt<'a>(&self, text: &[u8], out_buf: &'a mut [u8]) -> Result<&'a [u8], RnsError> {
        let established = self.established.as_ref().ok_or(RnsError::InvalidArgument)?;
        self.priv_identity
            .decrypt(OsRng, text, &established.derived_key, out_buf)
    }

    pub fn destination(&self) -> &DestinationDesc {
        &self.destination
    }

    pub fn create_rtt(&self) -> Result<Packet, RnsError> {
        let established = self.established.as_ref().ok_or(RnsError::InvalidArgument)?;

        let rtt = self.rtt.as_secs_f32();
        let mut buf = Vec::new();
        {
            buf.reserve(4);
            rmp::encode::write_f32(&mut buf, rtt).unwrap();
        }

        let mut packet_data = PacketDataBuffer::new();
        let token_len = {
            let token = self.priv_identity.encrypt(
                OsRng,
                buf.as_slice(),
                &established.derived_key,
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

    /// Complete the DH key exchange.
    ///
    /// `peer_identity` must come from a validated proof — callers inside this
    /// crate use `LinkHandshake<Proved>::into_peer_identity()`, so the only
    /// path to populating `established` is through signature verification.
    fn handshake(&mut self, peer_identity: Identity) {
        log::debug!("link({}): handshake", self.id);

        self.status = LinkStatus::Handshake;

        let derived_key = self
            .priv_identity
            .derive_key(&peer_identity.public_key, Some(self.id.as_slice()));

        self.established = Some(EstablishedLink {
            peer_identity,
            derived_key,
        });
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
        self.status = LinkStatus::Closed;
        self.post_event(LinkEvent::Closed);
        log::warn!("link: close {}", self.id);
    }

    pub fn restart(&mut self) {
        log::warn!(
            "link({}): restart after {}s",
            self.id,
            self.request_time.elapsed().as_secs()
        );

        // Drop the key material; a new handshake will repopulate it.
        self.established = None;
        self.status = LinkStatus::Pending;
    }

    pub fn elapsed(&self) -> Duration {
        self.request_time.elapsed()
    }

    pub fn status(&self) -> LinkStatus {
        self.status
    }

    pub fn id(&self) -> &LinkId {
        &self.id
    }

    pub fn rtt_ms(&self) -> u32 {
        (self.rtt.as_millis() as u32).max(25)
    }
}
