//! Identity persistence over [`embedded-storage`] NOR flash.
//!
//! A [`PrivateIdentity`] is serialised as 64 raw bytes (two concatenated
//! 32-byte keys: X25519 private key followed by Ed25519 signing key) and
//! stored at a fixed flash offset, preceded by a 4-byte magic marker so that
//! blank or corrupt flash can be detected.
//!
//! No heap allocation is required — all serialisation is done on the stack via
//! [`PrivateIdentity::to_raw_bytes`] / [`PrivateIdentity::new_from_raw_bytes`].
//!
//! # Flash layout
//!
//! ```text
//! offset + 0  : [u8; 4]  magic = 0x52_4E_53_49  ("RNSI")
//! offset + 4  : [u8; 32] X25519 private key bytes
//! offset + 36 : [u8; 32] Ed25519 signing key bytes
//! ─────────────────────────────────────────────────
//! total       : 68 bytes
//! ```
//!
//! The offset is chosen by the caller so the same helpers work with any flash
//! region (internal NVMC, QSPI, …).
//!
//! # Example
//!
//! ```rust,ignore
//! use reticulum_node::storage::load_or_generate;
//!
//! // On first boot: generates a new identity and writes it to flash.
//! // On subsequent boots: loads the stored identity.
//! let identity = load_or_generate(&mut nvmc, 0x000F_F000, &mut rng)
//!     .expect("identity init");
//! ```

use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use rand_core::TryCryptoRng;
use thiserror::Error;

use reticulum_core::identity::{PrivateIdentity, PUBLIC_KEY_LENGTH};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Magic bytes written before the identity payload to detect valid storage.
pub const MAGIC: [u8; 4] = [0x52, 0x4E, 0x53, 0x49]; // "RNSI"

/// Combined raw identity size (X25519 private key + Ed25519 seed).
const IDENTITY_BYTES: usize = PUBLIC_KEY_LENGTH * 2; // 64

/// Total flash footprint including the magic prefix.
pub const STORAGE_SIZE: usize = MAGIC.len() + IDENTITY_BYTES; // 68

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from identity storage operations.
#[derive(Debug, Error)]
pub enum StorageError<E> {
    /// The underlying flash driver returned an error.
    #[error("flash error")]
    Flash(E),
    /// The stored identity bytes could not be decoded.
    #[error("identity bytes could not be decoded")]
    InvalidIdentity,
    /// Failed to obtain random bytes for identity generation.
    #[error("randomness source failed")]
    Randomness,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Attempt to load a [`PrivateIdentity`] from flash at `offset`.
///
/// Returns:
/// - `Ok(Some(identity))` — valid identity found and decoded.
/// - `Ok(None)` — the magic marker is absent; flash appears blank or erased.
/// - `Err(StorageError::InvalidIdentity)` — magic present but bytes corrupt.
/// - `Err(StorageError::Flash(_))` — underlying read failed.
pub fn load_identity<F>(
    flash: &mut F,
    offset: u32,
) -> Result<Option<PrivateIdentity>, StorageError<F::Error>>
where
    F: ReadNorFlash,
{
    let mut buf = [0u8; STORAGE_SIZE];
    flash.read(offset, &mut buf).map_err(StorageError::Flash)?;

    if buf[..4] != MAGIC {
        return Ok(None); // Blank or uninitialised.
    }

    let raw: &[u8; IDENTITY_BYTES] = buf[4..4 + IDENTITY_BYTES]
        .try_into()
        .map_err(|_| StorageError::InvalidIdentity)?;

    PrivateIdentity::new_from_raw_bytes(raw)
        .map(Some)
        .map_err(|_| StorageError::InvalidIdentity)
}

/// Write a [`PrivateIdentity`] to flash at `offset`.
///
/// The flash region `[offset, offset + STORAGE_SIZE)` is erased before
/// writing.  `offset` must be aligned to the flash's erase granularity.
pub fn store_identity<F>(
    flash: &mut F,
    offset: u32,
    identity: &PrivateIdentity,
) -> Result<(), StorageError<F::Error>>
where
    F: NorFlash,
{
    let mut buf = [0u8; STORAGE_SIZE];
    buf[..4].copy_from_slice(&MAGIC);
    buf[4..4 + IDENTITY_BYTES].copy_from_slice(&identity.to_raw_bytes());

    flash
        .erase(offset, offset + STORAGE_SIZE as u32)
        .map_err(StorageError::Flash)?;
    flash.write(offset, &buf).map_err(StorageError::Flash)
}

/// Load the identity from flash, or generate a new one and persist it.
///
/// This is the primary entry-point for embedded targets.  The random identity
/// is generated using `rng`.
pub fn load_or_generate<F, R>(
    flash: &mut F,
    offset: u32,
    rng: &mut R,
) -> Result<PrivateIdentity, StorageError<F::Error>>
where
    F: NorFlash,
    R: TryCryptoRng,
{
    if let Some(id) = load_identity(flash, offset)? {
        log::debug!("storage: loaded identity from flash @ 0x{:08X}", offset);
        return Ok(id);
    }

    log::info!("storage: no identity in flash — generating new one");
    let id =
        PrivateIdentity::try_new_from_rand(&mut *rng).map_err(|_| StorageError::Randomness)?;
    store_identity(flash, offset, &id)?;
    log::info!("storage: identity stored to flash @ 0x{:08X}", offset);
    Ok(id)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct MockFlashError;

    impl embedded_storage::nor_flash::NorFlashError for MockFlashError {
        fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
            embedded_storage::nor_flash::NorFlashErrorKind::Other
        }
    }

    struct MockFlash([u8; 512]);

    impl MockFlash {
        fn new() -> Self {
            Self([0xFF; 512])
        }
    }

    impl embedded_storage::nor_flash::ErrorType for MockFlash {
        type Error = MockFlashError;
    }

    impl ReadNorFlash for MockFlash {
        const READ_SIZE: usize = 1;
        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
            let s = offset as usize;
            buf.copy_from_slice(&self.0[s..s + buf.len()]);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.0.len()
        }
    }

    impl NorFlash for MockFlash {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = 256;
        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.0[from as usize..to as usize].fill(0xFF);
            Ok(())
        }
        fn write(&mut self, offset: u32, buf: &[u8]) -> Result<(), Self::Error> {
            let s = offset as usize;
            self.0[s..s + buf.len()].copy_from_slice(buf);
            Ok(())
        }
    }

    #[test]
    fn blank_flash_returns_none() {
        let mut flash = MockFlash::new();
        assert!(matches!(load_identity(&mut flash, 0), Ok(None)));
    }

    #[test]
    fn store_and_load_roundtrip() {
        let mut flash = MockFlash::new();
        let original =
            PrivateIdentity::try_new_from_rand(getrandom::SysRng).expect("system RNG");

        store_identity(&mut flash, 0, &original).unwrap();

        let loaded = load_identity(&mut flash, 0).unwrap().unwrap();
        assert_eq!(
            original.to_raw_bytes(),
            loaded.to_raw_bytes(),
            "private key must round-trip through flash"
        );
        assert_eq!(
            original.address_hash(),
            loaded.address_hash(),
            "address hash must be stable"
        );
    }

    #[test]
    fn load_or_generate_creates_and_persists() {
        let mut flash = MockFlash::new();
        let mut rng = getrandom::SysRng;

        let id1 = load_or_generate(&mut flash, 0, &mut rng).unwrap();
        let id2 = load_or_generate(&mut flash, 0, &mut rng).unwrap();

        assert_eq!(id1.to_raw_bytes(), id2.to_raw_bytes());
    }

    #[test]
    fn corrupt_magic_returns_none() {
        let mut flash = MockFlash::new();
        let id = PrivateIdentity::try_new_from_rand(getrandom::SysRng).expect("system RNG");
        store_identity(&mut flash, 0, &id).unwrap();

        flash.0[0] = 0xDE; // corrupt magic

        assert!(matches!(load_identity(&mut flash, 0), Ok(None)));
    }
}
