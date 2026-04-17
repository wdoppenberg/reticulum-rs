//! Identity persistence over [`embedded-storage`] NOR flash.
//!
//! A [`PrivateIdentity`] is serialised as 64 raw bytes (two concatenated
//! 32-byte keys: X25519 private key followed by Ed25519 signing key) and
//! stored at a fixed flash offset, preceded by a 4-byte magic marker so that
//! blank or corrupt flash can be detected.
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
use rand_core::CryptoRng;

use reticulum_core::identity::{PrivateIdentity, PUBLIC_KEY_LENGTH};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Magic bytes written before the identity payload to detect valid storage.
pub const MAGIC: [u8; 4] = [0x52, 0x4E, 0x53, 0x49]; // "RNSI"

/// Raw bytes for the X25519 private key.
const PRIV_KEY_BYTES: usize = PUBLIC_KEY_LENGTH; // 32
/// Raw bytes for the Ed25519 signing key.
const SIGN_KEY_BYTES: usize = PUBLIC_KEY_LENGTH; // 32
/// Combined raw identity size.
const IDENTITY_BYTES: usize = PRIV_KEY_BYTES + SIGN_KEY_BYTES; // 64

/// Total flash footprint including the magic prefix.
pub const STORAGE_SIZE: usize = MAGIC.len() + IDENTITY_BYTES; // 68

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors from identity storage operations.
#[derive(Debug)]
pub enum StorageError<E> {
    /// The underlying flash driver returned an error.
    Flash(E),
    /// The stored identity bytes could not be decoded.
    InvalidIdentity,
}

impl<E: core::fmt::Debug> core::fmt::Display for StorageError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StorageError::Flash(e) => write!(f, "flash error: {:?}", e),
            StorageError::InvalidIdentity => write!(f, "identity bytes could not be decoded"),
        }
    }
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

    // Check magic.
    if buf[..4] != MAGIC {
        return Ok(None); // Blank or uninitialised.
    }

    // Parse raw bytes: [0..4] magic, [4..36] priv key, [36..68] sign key.
    // `new_from_hex_string` expects hex, so convert bytes → hex on the stack.
    let raw = &buf[4..4 + IDENTITY_BYTES];
    let mut hex = [0u8; IDENTITY_BYTES * 2];
    for (i, &b) in raw.iter().enumerate() {
        let hi = nibble_to_hex(b >> 4);
        let lo = nibble_to_hex(b & 0xF);
        hex[i * 2] = hi;
        hex[i * 2 + 1] = lo;
    }

    let hex_str = core::str::from_utf8(&hex).map_err(|_| StorageError::InvalidIdentity)?;
    PrivateIdentity::new_from_hex_string(hex_str)
        .map(Some)
        .map_err(|_| StorageError::InvalidIdentity)
}

/// Write a [`PrivateIdentity`] to flash at `offset`.
///
/// The flash region `[offset, offset + STORAGE_SIZE)` is erased before
/// writing.  `offset` must be aligned to the flash's erase granularity.
///
/// Requires the `alloc` feature on `reticulum-core` so that
/// `PrivateIdentity::to_hex_string()` is available.
#[cfg(feature = "alloc")]
pub fn store_identity<F>(
    flash: &mut F,
    offset: u32,
    identity: &PrivateIdentity,
) -> Result<(), StorageError<F::Error>>
where
    F: NorFlash,
{
    // Convert hex string → raw bytes.
    let hex = identity.to_hex_string();
    let hex_bytes = hex.as_bytes();

    let mut raw = [0u8; IDENTITY_BYTES];
    for (i, chunk) in hex_bytes.chunks_exact(2).enumerate().take(IDENTITY_BYTES) {
        raw[i] = (hex_nibble(chunk[0]) << 4) | hex_nibble(chunk[1]);
    }

    let mut buf = [0u8; STORAGE_SIZE];
    buf[..4].copy_from_slice(&MAGIC);
    buf[4..4 + IDENTITY_BYTES].copy_from_slice(&raw);

    flash
        .erase(offset, offset + STORAGE_SIZE as u32)
        .map_err(StorageError::Flash)?;
    flash.write(offset, &buf).map_err(StorageError::Flash)?;

    Ok(())
}

/// Load the identity from flash, or generate a new one and persist it.
///
/// This is the primary entry-point for embedded targets.  The random identity
/// is generated using `rng` (must implement [`CryptoRng`] + `RngCore`).
///
/// Requires the `alloc` feature on `reticulum-core`.
#[cfg(feature = "alloc")]
pub fn load_or_generate<F, R>(
    flash: &mut F,
    offset: u32,
    rng: &mut R,
) -> Result<PrivateIdentity, StorageError<F::Error>>
where
    F: NorFlash,
    R: CryptoRng + rand_core::RngCore,
{
    if let Some(id) = load_identity(flash, offset)? {
        log::debug!("storage: loaded identity from flash @ 0x{:08X}", offset);
        return Ok(id);
    }

    log::info!("storage: no identity in flash — generating new one");
    let id = PrivateIdentity::new_from_rand(&mut *rng);
    store_identity(flash, offset, &id)?;
    log::info!("storage: identity stored to flash @ 0x{:08X}", offset);
    Ok(id)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn nibble_to_hex(n: u8) -> u8 {
    match n {
        0..=9 => b'0' + n,
        10..=15 => b'a' + n - 10,
        _ => b'0',
    }
}

fn hex_nibble(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
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

    #[cfg(feature = "alloc")]
    #[test]
    fn store_and_load_roundtrip() {
        let mut flash = MockFlash::new();
        let original = PrivateIdentity::new_from_rand(rand_core::UnwrapErr(getrandom::SysRng));

        store_identity(&mut flash, 0, &original).unwrap();

        let loaded = load_identity(&mut flash, 0).unwrap().unwrap();
        assert_eq!(
            original.to_hex_string(),
            loaded.to_hex_string(),
            "private key must round-trip through flash"
        );
        assert_eq!(
            original.address_hash(),
            loaded.address_hash(),
            "address hash must be stable"
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn load_or_generate_creates_and_persists() {
        let mut flash = MockFlash::new();
        let mut rng = rand_core::UnwrapErr(getrandom::SysRng);

        let id1 = load_or_generate(&mut flash, 0, &mut rng).unwrap();
        let id2 = load_or_generate(&mut flash, 0, &mut rng).unwrap();

        assert_eq!(id1.to_hex_string(), id2.to_hex_string());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn corrupt_magic_returns_none() {
        let mut flash = MockFlash::new();
        let id = PrivateIdentity::new_from_rand(rand_core::UnwrapErr(getrandom::SysRng));
        store_identity(&mut flash, 0, &id).unwrap();

        flash.0[0] = 0xDE; // corrupt magic

        assert!(matches!(load_identity(&mut flash, 0), Ok(None)));
    }
}
