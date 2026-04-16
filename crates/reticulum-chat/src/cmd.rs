//! Typed chat command trait.
//!
//! Every message kind that can be sent over a Reticulum Channel is described by
//! an implementation of [`ChatCmd`].  The trait is **sealed** — external crates
//! cannot add new implementations, which keeps the dispatch table closed and
//! prevents accidental `msg_type` collisions.
//!
//! # Adding a new message kind
//!
//! 1. Define a struct for the payload.
//! 2. `impl private::Sealed for MyCmd {}`
//! 3. `impl ChatCmd for MyCmd { const MSG_TYPE: u16 = 0x000N; ... }`
//! 4. Add a match arm in [`crate::node::dispatch_inbound`].

mod private {
    /// Prevents external crates from implementing [`super::ChatCmd`].
    pub trait Sealed {}
}

/// A typed message that can be sent or received over a Reticulum Channel.
///
/// The associated constant [`MSG_TYPE`](ChatCmd::MSG_TYPE) is the u16 message
/// type embedded in every Channel envelope.  `encode` / `decode` handle
/// serialisation.  The sealed super-trait prevents unsound external impls.
pub trait ChatCmd: private::Sealed + Sized {
    /// The Channel message-type discriminant for this command.
    const MSG_TYPE: u16;

    /// Encode `self` into the Channel envelope payload bytes.
    fn encode(&self) -> Vec<u8>;

    /// Decode a payload received from the Channel.  Returns `None` if the
    /// bytes are malformed.
    fn decode(data: &[u8]) -> Option<Self>;
}

// ── Implementations ───────────────────────────────────────────────────────────

use std::time::{SystemTime, UNIX_EPOCH};

use reticulum_core::hash::{AddressHash, ADDRESS_HASH_SIZE};

/// Minimum on-wire payload: sender (16 B) + timestamp (8 B LE).
const MIN_PAYLOAD: usize = ADDRESS_HASH_SIZE + 8;

/// A plain-text chat message.
///
/// Wire layout:
/// ```text
/// [sender:    16 bytes]  — chat destination address hash of the sender
/// [timestamp:  8 bytes]  — Unix seconds, little-endian u64
/// [content:    N bytes]  — UTF-8 text
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct TextMessage {
    /// Address hash of the sender's `reticulum_chat.text` destination.
    pub sender: AddressHash,
    /// Unix timestamp (seconds) at message creation.
    pub timestamp: u64,
    /// Message text.
    pub content: String,
}

impl TextMessage {
    /// Create a new outgoing message stamped with the current wall-clock time.
    pub fn new(sender: AddressHash, content: impl Into<String>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            sender,
            timestamp,
            content: content.into(),
        }
    }
}

impl private::Sealed for TextMessage {}

impl ChatCmd for TextMessage {
    const MSG_TYPE: u16 = 0x0001;

    fn encode(&self) -> Vec<u8> {
        let content_bytes = self.content.as_bytes();
        let mut buf = Vec::with_capacity(MIN_PAYLOAD + content_bytes.len());
        buf.extend_from_slice(self.sender.as_slice());
        buf.extend_from_slice(&self.timestamp.to_le_bytes());
        buf.extend_from_slice(content_bytes);
        buf
    }

    fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < MIN_PAYLOAD {
            return None;
        }
        // The sender field is stored as raw address-hash bytes, not re-hashed.
        let sender_bytes: [u8; ADDRESS_HASH_SIZE] = data[..ADDRESS_HASH_SIZE].try_into().ok()?;
        let sender = AddressHash::new(sender_bytes);
        let ts_bytes: [u8; 8] = data[ADDRESS_HASH_SIZE..MIN_PAYLOAD].try_into().ok()?;
        let timestamp = u64::from_le_bytes(ts_bytes);
        let content = String::from_utf8(data[MIN_PAYLOAD..].to_vec()).ok()?;
        Some(Self { sender, timestamp, content })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use reticulum_core::hash::AddressHash;

    #[test]
    fn text_message_round_trips() {
        let sender = AddressHash::new_from_rand(OsRng);
        let msg = TextMessage::new(sender, "hello, world");
        let encoded = msg.encode();
        let decoded = TextMessage::decode(&encoded).expect("decode");
        assert_eq!(decoded.sender, msg.sender);
        assert_eq!(decoded.timestamp, msg.timestamp);
        assert_eq!(decoded.content, "hello, world");
    }

    #[test]
    fn decode_empty_returns_none() {
        assert!(TextMessage::decode(&[]).is_none());
    }

    #[test]
    fn decode_too_short_returns_none() {
        assert!(TextMessage::decode(&[0u8; 10]).is_none());
    }

    #[test]
    fn decode_invalid_utf8_returns_none() {
        let sender = AddressHash::new_from_rand(OsRng);
        let mut buf = Vec::new();
        buf.extend_from_slice(sender.as_slice()); // 16 bytes
        buf.extend_from_slice(&42u64.to_le_bytes()); // 8 bytes
        buf.extend_from_slice(&[0xFF, 0xFE]); // invalid UTF-8
        assert!(TextMessage::decode(&buf).is_none());
    }

    #[test]
    fn msg_type_constant_is_correct() {
        assert_eq!(TextMessage::MSG_TYPE, 0x0001);
    }

    #[test]
    fn empty_content_round_trips() {
        let sender = AddressHash::new_from_rand(OsRng);
        let msg = TextMessage::new(sender, "");
        let decoded = TextMessage::decode(&msg.encode()).expect("decode");
        assert_eq!(decoded.content, "");
    }
}
