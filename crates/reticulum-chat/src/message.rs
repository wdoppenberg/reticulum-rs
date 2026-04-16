//! Chat message wire format.
//!
//! `ChatMessage` is an alias for [`crate::cmd::TextMessage`]; the encoding
//! logic and `MSG_TYPE` constant live there.  This module exists for
//! backwards-compatible re-export only.

pub use crate::cmd::TextMessage as ChatMessage;
