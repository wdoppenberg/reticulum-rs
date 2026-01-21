use ed25519_dalek::{Signature, SIGNATURE_LENGTH};

use crate::{
    error::RnsError,
    identity::Identity,
    packet::{PacketDataBuffer, PUBLIC_KEY_LENGTH},
};

use super::LinkId;

/// Validates a proof packet for link establishment
pub fn validate_proof_packet(
    link_id: &LinkId,
    peer_identity: &Identity,
    proof_data: &[u8],
) -> Result<(), RnsError> {
    if proof_data.len() < SIGNATURE_LENGTH + PUBLIC_KEY_LENGTH {
        return Err(RnsError::InvalidArgument);
    }

    let signature_bytes = &proof_data[..SIGNATURE_LENGTH];
    let public_key = &proof_data[SIGNATURE_LENGTH..SIGNATURE_LENGTH + PUBLIC_KEY_LENGTH];

    if public_key != peer_identity.public_key.as_bytes() {
        return Err(RnsError::InvalidArgument);
    }

    let signature = Signature::from_bytes(signature_bytes.try_into().unwrap());

    let mut signed_data = PacketDataBuffer::new();
    signed_data.safe_write(link_id.as_slice());
    signed_data.safe_write(peer_identity.public_key.as_bytes());
    signed_data.safe_write(peer_identity.verifying_key.as_bytes());

    peer_identity
        .verifying_key
        .verify_strict(signed_data.as_slice(), &signature)
        .map_err(|_| RnsError::CryptoError)?;

    Ok(())
}
