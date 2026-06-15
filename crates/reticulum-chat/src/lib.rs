//! `reticulum-chat` — chat building blocks for the Reticulum Network Stack.
//!
//! Provides the [`ChatHandle`] / [`start`] entry-point, the [`ChatEvent`] bus,
//! and the [`ChatMessage`] wire type.  All networking is delegated to a
//! `reticulum-tokio` [`Transport`].
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use reticulum_core::identity::PrivateIdentity;
//! use reticulum_tokio::{Transport, TransportConfig};
//! use reticulum_chat::{start, ChatEvent};
//!
//! #[tokio::main]
//! async fn main() {
//!     let identity = PrivateIdentity::try_new_from_rand(getrandom::SysRng).expect("system RNG");
//!     let node_address = *identity.address_hash();
//!     let transport = Arc::new(Transport::new(TransportConfig::new(
//!         "chat", node_address, true,
//!     )));
//!
//!     let handle = start(transport, identity, Some("Alice".to_string()))
//!         .await
//!         .unwrap();
//!
//!     println!("My chat address: {}", handle.own_address);
//!
//!     let mut events = handle.subscribe();
//!     while let Ok(event) = events.recv().await {
//!         match event {
//!             ChatEvent::PeerDiscovered { desc, display_name } => {
//!                 println!("Discovered peer: {}", desc.address_hash);
//!                 handle.connect(*desc).await;
//!             }
//!             ChatEvent::PeerConnected { peer_address } => {
//!                 handle.send_message(peer_address, "Hello!").await.ok();
//!             }
//!             ChatEvent::MessageReceived { message } => {
//!                 println!("<{}> {}", message.sender, message.content);
//!             }
//!             ChatEvent::PeerDisconnected { peer_address } => {
//!                 println!("Peer {} disconnected", peer_address);
//!             }
//!         }
//!     }
//! }
//! ```

pub mod cmd;
pub mod error;
pub mod message;
pub mod node;

pub use cmd::{ChatCmd, TextMessage};
pub use error::ChatError;
pub use message::ChatMessage;
pub use node::{
    start, ChatEvent, ChatHandle, Connected, Disconnected, PeerHandle, APP_ASPECTS, APP_NAME,
};
