//! HDLC frame codec — re-exported from [`reticulum_core::hdlc`].
//!
//! The canonical implementation lives in `reticulum-core` and is shared by
//! both `reticulum-tokio` and `reticulum-embassy`.

pub use reticulum_core::hdlc::{Hdlc, HDLC_ESCAPE, HDLC_ESCAPE_MASK, HDLC_FLAG};
