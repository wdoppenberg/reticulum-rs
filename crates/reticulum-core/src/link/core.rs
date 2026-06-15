use core::mem;

use ed25519_dalek::SigningKey;
use rand_core::TryCryptoRng;
use x25519_dalek::StaticSecret;

use crate::{
    destination::DestinationDesc,
    error::RnsError,
    hash::AddressHash,
    identity::{DecryptIdentity, DerivedKey, EncryptIdentity, Identity, PrivateIdentity, PUBLIC_KEY_LENGTH},
    packet::{DestinationType, Header, Packet, PacketContext, PacketDataBuffer, PacketType, PACKET_MDU},
};

use super::{DataKind, LinkDataFrame, LinkHandshake, LinkId, LinkPayload, LinkStatus};

// ─── Link state ───────────────────────────────────────────────────────────────

#[allow(clippy::large_enum_variant)]
enum LinkState {
    Pending { priv_identity: PrivateIdentity },
    Active {
        priv_identity: PrivateIdentity,
        #[allow(dead_code)]
        peer_identity: Identity,
        derived_key: DerivedKey,
    },
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

// ─── LinkOutcome ─────────────────────────────────────────────────────────────

/// The result of processing an incoming packet on a [`LinkCore`].
///
/// Runtime wrappers map these variants to executor-specific event dispatch.
/// Core intentionally has no channels, so all side-effects are deferred to the
/// caller.
pub enum LinkOutcome {
    /// Packet was ignored (wrong destination, unknown type, or decryption failed).
    None,
    /// DH handshake completed; the link transitioned to `Active`.
    Activated,
    /// A keep-alive request arrived; the caller should send a keep-alive reply.
    KeepAlive,
    /// A successfully decrypted data frame.
    DataReceived(LinkDataFrame),
}

// ─── LinkCore ─────────────────────────────────────────────────────────────────

/// Runtime-agnostic link state machine.
///
/// Holds all cryptographic state and implements every packet-level operation:
/// DH handshake, packet creation, encryption, decryption, and keep-alive
/// handling.  Has no async executor dependency, no channels, and no wall-clock
/// timer — timing concerns belong to the runtime wrapper.
///
/// RTT is stored as a `u32` millisecond value.  The runtime wrapper measures
/// elapsed time and calls [`set_rtt_ms`](Self::set_rtt_ms) when the handshake
/// proof is validated.
#[allow(clippy::large_enum_variant)]
pub struct LinkCore {
    id: LinkId,
    destination: DestinationDesc,
    state: LinkState,
    rtt_ms: u32,
}

impl LinkCore {
    /// Create an outgoing link to `destination`.
    ///
    /// Generates an ephemeral `PrivateIdentity` with `rng`.  The link starts
    /// in the `Pending` state; call [`request_packet`](Self::request_packet)
    /// to build the wire packet that initiates the handshake.
    pub fn new_outgoing<R: TryCryptoRng>(
        destination: DestinationDesc,
        rng: R,
    ) -> Result<Self, RnsError> {
        Ok(Self {
            id: AddressHash::new_empty(),
            destination,
            state: LinkState::Pending {
                priv_identity: PrivateIdentity::try_new_from_rand(rng)?,
            },
            rtt_ms: 0,
        })
    }

    /// Create an incoming link from a received link-request `packet`.
    ///
    /// `signing_key` belongs to the responding destination; `destination` is
    /// the local destination that received the request.  The link starts in
    /// the `Active` state (the responder's DH half is computed here); call
    /// [`prove_packet`](Self::prove_packet) to generate the proof reply.
    pub fn new_incoming<R: TryCryptoRng>(
        packet: &Packet,
        signing_key: SigningKey,
        destination: DestinationDesc,
        mut rng: R,
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
        rng.try_fill_bytes(&mut private_key_bytes)
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
            rtt_ms: 0,
        })
    }

    /// Build the link-request packet (initiator side).
    ///
    /// Sets the link ID from the packet hash.  The runtime wrapper should
    /// update its activity timestamp after calling this.
    pub fn request_packet(&mut self) -> Packet {
        let priv_identity = self
            .state
            .priv_identity()
            .expect("request_packet() called on closed link");

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
        packet
    }

    /// Build the proof packet (responder side).
    ///
    /// The caller is responsible for emitting any `LinkEvent::Activated`
    /// notification after this returns successfully.
    pub fn prove_packet(&mut self) -> Result<Packet, RnsError> {
        log::debug!("link({}): prove", self.id);

        let priv_identity = self
            .state
            .priv_identity()
            .expect("prove_packet() called on closed link");

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

    /// Process an incoming packet.
    ///
    /// `rng` is needed for the decryption path (the `Fernet` token verifier
    /// stores an RNG reference even though decryption itself is deterministic).
    ///
    /// Returns a [`LinkOutcome`] describing what happened.  The runtime wrapper
    /// translates the outcome into executor-specific event dispatch (channels,
    /// callbacks, etc.) and updates timing state.
    pub fn handle_packet<R: TryCryptoRng + Copy>(
        &mut self,
        rng: R,
        packet: &Packet,
    ) -> LinkOutcome {
        if packet.destination != self.id {
            return LinkOutcome::None;
        }

        match packet.header.packet_type {
            PacketType::Data => self.handle_data_packet(rng, packet),
            PacketType::Proof
                if self.state.status() == LinkStatus::Pending
                    && packet.context == PacketContext::LinkRequestProof =>
            {
                match LinkHandshake::new(self.id).validate_proof(
                    packet.data.as_slice(),
                    &self.destination.identity.verifying_key,
                ) {
                    Ok(proved) => {
                        self.activate(proved.into_peer_identity());
                        log::debug!("link({}): proved and activated", self.id);
                        LinkOutcome::Activated
                    }
                    Err(_) => {
                        log::warn!("link({}): proof is not valid", self.id);
                        LinkOutcome::None
                    }
                }
            }
            _ => LinkOutcome::None,
        }
    }

    fn handle_data_packet<R: TryCryptoRng + Copy>(
        &mut self,
        rng: R,
        packet: &Packet,
    ) -> LinkOutcome {
        match packet.context {
            PacketContext::None => {
                let mut buffer = [0u8; PACKET_MDU];
                match self.decrypt(rng, packet.data.as_slice(), &mut buffer[..]) {
                    Ok(plain_text) => {
                        log::trace!("link({}): data {}B", self.id, plain_text.len());
                        LinkOutcome::DataReceived(LinkDataFrame {
                            kind: DataKind::Data,
                            context: PacketContext::None,
                            payload: LinkPayload::new_from_slice(plain_text),
                        })
                    }
                    Err(_) => {
                        log::error!("link({}): can't decrypt packet", self.id);
                        LinkOutcome::None
                    }
                }
            }
            PacketContext::Channel => {
                let mut buffer = [0u8; PACKET_MDU];
                match self.decrypt(rng, packet.data.as_slice(), &mut buffer[..]) {
                    Ok(plain_text) => {
                        log::trace!("link({}): channel data {}B", self.id, plain_text.len());
                        LinkOutcome::DataReceived(LinkDataFrame {
                            kind: DataKind::ChannelData,
                            context: PacketContext::Channel,
                            payload: LinkPayload::new_from_slice(plain_text),
                        })
                    }
                    Err(_) => {
                        log::error!("link({}): can't decrypt channel packet", self.id);
                        LinkOutcome::None
                    }
                }
            }
            ctx @ (PacketContext::Resource
            | PacketContext::ResourceAdvertisement
            | PacketContext::ResourceRequest
            | PacketContext::ResourceHashUpdate
            | PacketContext::ResourceProof
            | PacketContext::ResourceInitiatorCancel
            | PacketContext::ResourceReceiverCancel) => {
                let mut buffer = [0u8; PACKET_MDU];
                match self.decrypt(rng, packet.data.as_slice(), &mut buffer[..]) {
                    Ok(plain_text) => {
                        log::trace!(
                            "link({}): resource data {}B ctx={:?}",
                            self.id,
                            plain_text.len(),
                            ctx
                        );
                        LinkOutcome::DataReceived(LinkDataFrame {
                            kind: DataKind::ResourceData,
                            context: ctx,
                            payload: LinkPayload::new_from_slice(plain_text),
                        })
                    }
                    Err(_) => {
                        log::error!("link({}): can't decrypt resource packet", self.id);
                        LinkOutcome::None
                    }
                }
            }
            PacketContext::KeepAlive => {
                if !packet.data.is_empty() && packet.data.as_slice()[0] == 0xFF {
                    log::trace!("link({}): keep-alive request", self.id);
                    return LinkOutcome::KeepAlive;
                }
                if !packet.data.is_empty() && packet.data.as_slice()[0] == 0xFE {
                    log::trace!("link({}): keep-alive response", self.id);
                }
                LinkOutcome::None
            }
            _ => LinkOutcome::None,
        }
    }

    /// Transition `Pending → Active` after validating the peer's proof.
    fn activate(&mut self, peer_identity: Identity) {
        let old = mem::replace(&mut self.state, LinkState::Closed);
        let priv_identity = match old {
            LinkState::Pending { priv_identity } => priv_identity,
            LinkState::Active { priv_identity, .. } => {
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
        self.state = LinkState::Active {
            priv_identity,
            peer_identity,
            derived_key,
        };
    }

    /// Build an encrypted data packet.  Returns `Err` if the link is not active.
    pub fn data_packet<R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        data: &[u8],
    ) -> Result<Packet, RnsError> {
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
                priv_identity.encrypt(rng, data, derived_key, packet_data.acquire_buf_max())?;
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

    /// Build an encrypted channel-data packet.  Returns `Err` if not active.
    pub fn channel_packet<R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        data: &[u8],
    ) -> Result<Packet, RnsError> {
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
                priv_identity.encrypt(rng, data, derived_key, packet_data.acquire_buf_max())?;
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

    /// Build an encrypted resource packet.  Returns `Err` if not active.
    pub fn resource_packet<R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        data: &[u8],
        context: PacketContext,
    ) -> Result<Packet, RnsError> {
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
                priv_identity.encrypt(rng, data, derived_key, packet_data.acquire_buf_max())?;
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

    /// Encrypt `text` with the link's derived key.
    ///
    /// Returns `Err(InvalidArgument)` if the link is not active.
    pub fn encrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        text: &[u8],
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };
        priv_identity.encrypt(rng, text, derived_key, out_buf)
    }

    /// Decrypt `text` with the link's derived key.
    ///
    /// Returns `Err(InvalidArgument)` if the link is not active.
    pub fn decrypt<'a, R: TryCryptoRng + Copy>(
        &self,
        rng: R,
        text: &[u8],
        out_buf: &'a mut [u8],
    ) -> Result<&'a [u8], RnsError> {
        let LinkState::Active {
            priv_identity,
            derived_key,
            ..
        } = &self.state
        else {
            return Err(RnsError::InvalidArgument);
        };
        priv_identity.decrypt(rng, text, derived_key, out_buf)
    }

    /// Transition the link to `Closed`.
    ///
    /// The caller is responsible for emitting any `LinkEvent::Closed`
    /// notification.
    pub fn close(&mut self) {
        self.state = LinkState::Closed;
        log::warn!("link: close {}", self.id);
    }

    /// Revert the link to `Pending`, retaining the private identity if available.
    ///
    /// If the link is already `Closed`, a fresh identity is generated with
    /// `rng`.  The caller is responsible for logging elapsed time before
    /// calling this.
    pub fn restart<R: TryCryptoRng>(&mut self, rng: R) {
        let old = mem::replace(&mut self.state, LinkState::Closed);
        let priv_identity = match old {
            LinkState::Active { priv_identity, .. } | LinkState::Pending { priv_identity } => {
                priv_identity
            }
            LinkState::Closed => match PrivateIdentity::try_new_from_rand(rng) {
                Ok(identity) => identity,
                Err(_) => {
                    log::error!("link({}): restart failed: RNG unavailable", self.id);
                    return;
                }
            },
        };
        self.state = LinkState::Pending { priv_identity };
    }

    pub fn destination(&self) -> &DestinationDesc {
        &self.destination
    }

    pub fn status(&self) -> LinkStatus {
        self.state.status()
    }

    pub fn id(&self) -> &LinkId {
        &self.id
    }

    /// RTT in milliseconds, clamped to a minimum of 25 ms.
    pub fn rtt_ms(&self) -> u32 {
        self.rtt_ms.max(25)
    }

    /// Set the RTT in milliseconds.  Called by the runtime wrapper when the
    /// handshake proof is validated.
    pub fn set_rtt_ms(&mut self, rtt_ms: u32) {
        self.rtt_ms = rtt_ms;
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::{DestinationName, SingleInputDestination};

    // Minimal deterministic RNG for tests (LCG; not cryptographically secure).
    #[derive(Copy, Clone)]
    struct TestRng(u64);

    impl TestRng {
        fn seeded() -> Self {
            Self(0xDEAD_BEEF_CAFE_BABE)
        }
    }

    #[derive(Debug)]
    struct TestRngError;

    impl core::fmt::Display for TestRngError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("TestRng error")
        }
    }

    impl core::error::Error for TestRngError {}

    impl rand_core::TryRng for TestRng {
        type Error = TestRngError;

        fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            Ok((self.0 >> 32) as u32)
        }

        fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
            let lo = self.try_next_u32()? as u64;
            let hi = self.try_next_u32()? as u64;
            Ok((hi << 32) | lo)
        }

        fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
            for chunk in dst.chunks_mut(8) {
                let n = self.try_next_u64()?;
                for (i, byte) in chunk.iter_mut().enumerate() {
                    *byte = (n >> (i * 8)) as u8;
                }
            }
            Ok(())
        }
    }

    impl rand_core::TryCryptoRng for TestRng {}

    fn make_outgoing() -> (LinkCore, SingleInputDestination) {
        let dest = SingleInputDestination::new(
            PrivateIdentity::try_new_from_rand(TestRng::seeded()).expect("rng"),
            DestinationName::new("test", "link"),
        );
        let link =
            LinkCore::new_outgoing(dest.desc, TestRng::seeded()).expect("new_outgoing");
        (link, dest)
    }

    #[test]
    fn new_outgoing_is_pending() {
        let (link, _) = make_outgoing();
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn close_transitions_to_closed() {
        let (mut link, _) = make_outgoing();
        link.close();
        assert_eq!(link.status(), LinkStatus::Closed);
    }

    #[test]
    fn channel_packet_on_pending_returns_error() {
        let (link, _) = make_outgoing();
        assert!(link.channel_packet(TestRng::seeded(), b"hello").is_err());
    }

    #[test]
    fn data_packet_on_pending_returns_error() {
        let (link, _) = make_outgoing();
        assert!(link.data_packet(TestRng::seeded(), b"hello").is_err());
    }

    #[test]
    fn restart_from_pending_stays_pending() {
        let (mut link, _) = make_outgoing();
        link.restart(TestRng::seeded());
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn restart_from_closed_becomes_pending() {
        let (mut link, _) = make_outgoing();
        link.close();
        link.restart(TestRng::seeded());
        assert_eq!(link.status(), LinkStatus::Pending);
    }

    #[test]
    fn new_incoming_is_active() {
        let responder_dest = SingleInputDestination::new(
            PrivateIdentity::try_new_from_rand(TestRng::seeded()).expect("rng"),
            DestinationName::new("test", "link"),
        );

        let requester_id =
            PrivateIdentity::try_new_from_rand(TestRng::seeded()).expect("rng");

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
        let link = LinkCore::new_incoming(
            &packet,
            signing_key,
            responder_dest.desc,
            TestRng::seeded(),
        )
        .expect("new_incoming");

        assert_eq!(link.status(), LinkStatus::Active);
    }
}
