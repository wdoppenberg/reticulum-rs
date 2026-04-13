use ed25519_dalek::{Signature, SIGNATURE_LENGTH, VerifyingKey};

use crate::{
    buffer::OutputBuffer,
    error::RnsError,
    hash::ADDRESS_HASH_SIZE,
    identity::{Identity, PUBLIC_KEY_LENGTH},
};

use super::{LinkId, LINK_MTU_SIZE};

// ─── Sealed trait ─────────────────────────────────────────────────────────────

mod sealed {
    pub trait Sealed {}
}

// ─── State markers ───────────────────────────────────────────────────────────

/// The handshake is waiting for a proof packet from the remote peer.
pub struct AwaitingProof {
    _private: (),
}

/// The remote peer's proof packet was received and the signature verified.
/// The validated peer `Identity` lives in this state.
pub struct Proved {
    peer_identity: Identity,
}

impl sealed::Sealed for AwaitingProof {}
impl sealed::Sealed for Proved {}

pub trait HandshakeState: sealed::Sealed {}
impl HandshakeState for AwaitingProof {}
impl HandshakeState for Proved {}

// ─── Type-state handshake ─────────────────────────────────────────────────────

/// A link proof handshake parameterised over its cryptographic state `S`.
///
/// Compile-time guarantees:
/// - `validate_proof` is callable exactly once — `AwaitingProof` is consumed on call
/// - `peer_identity` is inaccessible before proof validates — method only exists on `Proved`
/// - The `Identity` inside `Proved` is always the one whose signature passed verification
#[must_use]
pub struct LinkHandshake<S: HandshakeState> {
    link_id: LinkId,
    state: S,
}

impl LinkHandshake<AwaitingProof> {
    pub fn new(link_id: LinkId) -> Self {
        Self {
            link_id,
            state: AwaitingProof { _private: () },
        }
    }

    /// Validate the remote peer's proof packet.
    ///
    /// Consumes `self` so that proof validation can happen at most once.
    /// `proof_data` is the raw payload of the received proof packet.
    /// `peer_verifying_key` is the Ed25519 key from the known destination identity.
    ///
    /// On success returns `LinkHandshake<Proved>`, the capability token proving
    /// the peer is authentic. The validated `Identity` is accessible from it.
    pub fn validate_proof(
        self,
        proof_data: &[u8],
        peer_verifying_key: &VerifyingKey,
    ) -> Result<LinkHandshake<Proved>, RnsError> {
        const MIN_PROOF_LEN: usize = SIGNATURE_LENGTH + PUBLIC_KEY_LENGTH;
        const MTU_PROOF_LEN: usize = SIGNATURE_LENGTH + PUBLIC_KEY_LENGTH + LINK_MTU_SIZE;
        const SIGN_DATA_LEN: usize = ADDRESS_HASH_SIZE + PUBLIC_KEY_LENGTH * 2 + LINK_MTU_SIZE;

        if proof_data.len() < MIN_PROOF_LEN {
            return Err(RnsError::PacketError);
        }

        // Build the buffer that the peer signed:
        //   [link_id | pub_key_from_proof | verifying_key_from_dest | optional_mtu]
        let mut sign_buf = [0u8; SIGN_DATA_LEN];
        let sign_data_len = {
            let mut out = OutputBuffer::new(&mut sign_buf);
            out.write(self.link_id.as_slice())?;
            out.write(&proof_data[SIGNATURE_LENGTH..SIGNATURE_LENGTH + PUBLIC_KEY_LENGTH])?;
            out.write(peer_verifying_key.as_bytes())?;
            if proof_data.len() >= MTU_PROOF_LEN {
                out.write(&proof_data[SIGNATURE_LENGTH + PUBLIC_KEY_LENGTH..])?;
            }
            out.offset()
        };

        // The peer identity combines the X25519 public key from the proof with
        // the Ed25519 verifying key already known from the destination.
        let peer_identity = Identity::new_from_slices(
            &sign_buf[ADDRESS_HASH_SIZE..ADDRESS_HASH_SIZE + PUBLIC_KEY_LENGTH],
            peer_verifying_key.as_bytes(),
        )
        .map_err(|_| RnsError::CryptoError)?;

        let signature = Signature::from_slice(&proof_data[..SIGNATURE_LENGTH])
            .map_err(|_| RnsError::CryptoError)?;

        peer_identity
            .verify(&sign_buf[..sign_data_len], &signature)
            .map_err(|_| RnsError::IncorrectSignature)?;

        Ok(LinkHandshake {
            link_id: self.link_id,
            state: Proved { peer_identity },
        })
    }
}

impl LinkHandshake<Proved> {
    /// Borrow the validated peer identity.
    ///
    /// Only reachable after a successful `validate_proof` transition.
    pub fn peer_identity(&self) -> &Identity {
        &self.state.peer_identity
    }

    /// Consume the handshake capability and yield the peer identity.
    pub fn into_peer_identity(self) -> Identity {
        self.state.peer_identity
    }
}
