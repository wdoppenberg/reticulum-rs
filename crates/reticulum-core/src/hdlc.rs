//! HDLC frame codec.
//!
//! Reticulum serial interfaces (RNode LoRa, KISS TNC, …) carry Reticulum
//! packets wrapped in HDLC frames.  This module provides the stateless
//! encode / decode / find primitives that are shared by both the
//! `reticulum-tokio` and `reticulum-embassy` runtime crates.
//!
//! The framing variant used is the simplified flag-escape scheme without CRC:
//!
//! ```text
//! 0x7E | <escaped payload> | 0x7E
//! ```
//!
//! Bytes `0x7E` (flag) and `0x7D` (escape) inside the payload are escaped by
//! prepending `0x7D` and XOR-masking the byte with `0x20`.
//!
//! # Example
//!
//! ```
//! use reticulum_core::hdlc::Hdlc;
//! use reticulum_core::buffer::OutputBuffer;
//!
//! let payload = b"hello";
//! let mut enc_buf = [0u8; 64];
//! let mut enc = OutputBuffer::new(&mut enc_buf);
//! Hdlc::encode(payload, &mut enc).unwrap();
//!
//! let mut dec_buf = [0u8; 64];
//! let mut dec = OutputBuffer::new(&mut dec_buf);
//! Hdlc::decode(enc.as_slice(), &mut dec).unwrap();
//! assert_eq!(dec.as_slice(), payload);
//! ```

use crate::buffer::OutputBuffer;
use crate::error::RnsError;

/// Flag byte that marks the start and end of an HDLC frame.
pub const HDLC_FLAG: u8 = 0x7E;
/// Escape byte that precedes any escaped payload byte.
pub const HDLC_ESCAPE: u8 = 0x7D;
/// XOR mask applied to a byte that follows an escape byte.
pub const HDLC_ESCAPE_MASK: u8 = 0x20;

/// Stateless HDLC frame codec.
pub struct Hdlc;

impl Hdlc {
    /// Encode `payload` as an HDLC frame into `out`.
    ///
    /// Writes `FLAG | escaped(payload) | FLAG`.  Returns the total number of
    /// bytes written (including the two flag bytes), or
    /// [`RnsError::OutOfMemory`] if `out` is too small.
    pub fn encode(payload: &[u8], out: &mut OutputBuffer<'_>) -> Result<usize, RnsError> {
        out.write_byte(HDLC_FLAG)?;
        for &byte in payload {
            if byte == HDLC_FLAG || byte == HDLC_ESCAPE {
                out.write(&[HDLC_ESCAPE, byte ^ HDLC_ESCAPE_MASK])?;
            } else {
                out.write_byte(byte)?;
            }
        }
        out.write_byte(HDLC_FLAG)?;
        Ok(out.offset())
    }

    /// Locate the first complete HDLC frame in `data`.
    ///
    /// Returns `Some((start, end))` where `data[start]` and `data[end]` are
    /// both [`HDLC_FLAG`] bytes and `end > start`.  Returns `None` if fewer
    /// than two flag bytes exist in `data` (frame not yet complete).
    ///
    /// The caller should pass `data[start..=end]` to [`decode`](Self::decode)
    /// and then advance its accumulation buffer past `end + 1`.
    pub fn find(data: &[u8]) -> Option<(usize, usize)> {
        let mut start: Option<usize> = None;
        for (i, &byte) in data.iter().enumerate() {
            if byte != HDLC_FLAG {
                continue;
            }
            match start {
                None => start = Some(i),
                Some(s) if i > s => return Some((s, i)),
                _ => {}
            }
        }
        None
    }

    /// Decode one HDLC frame (including its surrounding flag bytes) into `out`.
    ///
    /// `frame` must begin and end with [`HDLC_FLAG`] bytes.  Returns the
    /// number of decoded bytes written to `out`, or:
    ///
    /// - [`RnsError::PacketError`] if `frame` is shorter than 2 bytes or does
    ///   not start/end with a flag byte.
    /// - [`RnsError::OutOfMemory`] if `out` does not have enough space.
    pub fn decode(frame: &[u8], out: &mut OutputBuffer<'_>) -> Result<usize, RnsError> {
        if frame.len() < 2 || frame[0] != HDLC_FLAG || frame[frame.len() - 1] != HDLC_FLAG {
            return Err(RnsError::PacketError);
        }

        let mut escape_next = false;
        // Inner slice excludes both flag bytes.
        for &byte in &frame[1..frame.len() - 1] {
            if escape_next {
                escape_next = false;
                out.write_byte(byte ^ HDLC_ESCAPE_MASK)?;
            } else if byte == HDLC_ESCAPE {
                escape_next = true;
            } else if byte == HDLC_FLAG {
                // Unexpected flag inside the payload — treat as end (best-effort).
                break;
            } else {
                out.write_byte(byte)?;
            }
        }

        Ok(out.offset())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(payload: &[u8]) -> [u8; 512] {
        let mut buf = [0u8; 512];
        let mut out = OutputBuffer::new(&mut buf);
        Hdlc::encode(payload, &mut out).unwrap();
        buf
    }

    fn encoded_len(payload: &[u8]) -> usize {
        let mut buf = [0u8; 512];
        let mut out = OutputBuffer::new(&mut buf);
        Hdlc::encode(payload, &mut out).unwrap();
        out.offset()
    }

    fn decode(frame: &[u8]) -> ([u8; 512], usize) {
        let mut buf = [0u8; 512];
        let mut out = OutputBuffer::new(&mut buf);
        let n = Hdlc::decode(frame, &mut out).unwrap();
        (buf, n)
    }

    #[test]
    fn roundtrip_plain() {
        let payload = b"hello world";
        let enc = encode(payload);
        let n = encoded_len(payload);
        let (dec, dn) = decode(&enc[..n]);
        assert_eq!(&dec[..dn], payload);
    }

    #[test]
    fn roundtrip_empty() {
        let n = encoded_len(b"");
        let enc = encode(b"");
        assert_eq!(&enc[..n], &[HDLC_FLAG, HDLC_FLAG]);
        let (dec, dn) = decode(&enc[..n]);
        assert_eq!(dn, 0);
        let _ = dec;
    }

    #[test]
    fn roundtrip_flag_and_escape_bytes() {
        let payload = [0x00, HDLC_FLAG, 0x01, HDLC_ESCAPE, 0xFF];
        let enc = encode(&payload);
        let n = encoded_len(&payload);
        // No raw flag bytes should appear in the interior.
        assert!(!enc[1..n - 1].contains(&HDLC_FLAG));
        let (dec, dn) = decode(&enc[..n]);
        assert_eq!(&dec[..dn], &payload);
    }

    #[test]
    fn roundtrip_all_special_bytes() {
        let payload = [HDLC_FLAG, HDLC_ESCAPE, HDLC_FLAG, HDLC_ESCAPE];
        let n = encoded_len(&payload);
        let enc = encode(&payload);
        let (dec, dn) = decode(&enc[..n]);
        assert_eq!(&dec[..dn], &payload);
    }

    #[test]
    fn find_single_frame() {
        let n = encoded_len(b"data");
        let enc = encode(b"data");
        assert_eq!(Hdlc::find(&enc[..n]), Some((0, n - 1)));
    }

    #[test]
    fn find_frame_with_leading_garbage() {
        // Prepend bytes that contain no flag byte.
        let mut stream = [0x01u8; 64];
        let n = encoded_len(b"payload");
        let enc = encode(b"payload");
        stream[10..10 + n].copy_from_slice(&enc[..n]);
        let (s, e) = Hdlc::find(&stream[..10 + n]).unwrap();
        let (dec, dn) = decode(&stream[s..=e]);
        assert_eq!(&dec[..dn], b"payload");
    }

    #[test]
    fn find_two_consecutive_frames() {
        let n1 = encoded_len(b"first");
        let n2 = encoded_len(b"second");
        let mut stream = [0u8; 512];
        stream[..n1].copy_from_slice(&encode(b"first")[..n1]);
        stream[n1..n1 + n2].copy_from_slice(&encode(b"second")[..n2]);
        let total = n1 + n2;

        let (s1, e1) = Hdlc::find(&stream[..total]).unwrap();
        let (dec1, dn1) = decode(&stream[s1..=e1]);
        assert_eq!(&dec1[..dn1], b"first");

        let base = e1 + 1;
        let (s2, e2) = Hdlc::find(&stream[base..total]).unwrap();
        let (dec2, dn2) = decode(&stream[base + s2..=base + e2]);
        assert_eq!(&dec2[..dn2], b"second");
    }

    #[test]
    fn find_incomplete_returns_none() {
        let partial = [HDLC_FLAG, 0x01, 0x02]; // only one flag
        assert_eq!(Hdlc::find(&partial), None);
        assert_eq!(Hdlc::find(&[]), None);
    }

    #[test]
    fn decode_rejects_missing_flags() {
        let mut buf = [0u8; 64];
        let mut out = OutputBuffer::new(&mut buf);
        assert!(matches!(
            Hdlc::decode(b"no flags", &mut out),
            Err(RnsError::PacketError)
        ));
    }

    #[test]
    fn decode_rejects_too_short() {
        let mut buf = [0u8; 64];
        let mut out = OutputBuffer::new(&mut buf);
        assert!(matches!(
            Hdlc::decode(&[HDLC_FLAG], &mut out),
            Err(RnsError::PacketError)
        ));
        assert!(matches!(
            Hdlc::decode(&[], &mut out),
            Err(RnsError::PacketError)
        ));
    }
}
