//! `reticulum-node` — full RNode implementation for the Reticulum Network Stack.
//!
//! This crate provides all the building blocks needed to run a Reticulum
//! forwarding node on bare-metal embedded targets, in particular the
//! **Heltec T114** (nRF52840 + SX1262).
//!
//! # Feature flags
//!
//! | Feature           | Enables                                              |
//! |-------------------|------------------------------------------------------|
//! | `embassy`         | Async node runner, Embassy sync + time primitives    |
//! | `lora`            | [`LoraInterface`] wrapping lora-phy SX126x driver    |
//! | `storage`         | Identity persistence helpers (embedded-storage)      |
//! | `ui`       | Generic ratatui + mousefood status UI primitives     |
//!
//! # Crate layout
//!
//! | Module        | Contents                                             |
//! |---------------|------------------------------------------------------|
//! | [`config`]    | [`NodeConfig`], [`LoraConfig`], regional presets     |
//! | [`router`]    | no_std [`Router`] with dedup, hop-counting, paths    |
//! | [`storage`]   | Identity persistence over NOR flash (feature `storage`) |
//! | [`lora`]      | [`LoraInterface`] (feature `lora`)                   |
//! | [`node`]      | [`run()`] async Embassy runner (feature `embassy`)   |
//! | [`ui`] | Generic node status UI primitives (feature `ui`) |
//!
//! [`NodeConfig`]: config::NodeConfig
//! [`LoraConfig`]: config::LoraConfig
//! [`Router`]: router::Router
//! [`LoraInterface`]: lora::LoraInterface
//! [`run()`]: node::run

#![no_std]
#![deny(missing_docs)]

pub mod config;
pub mod router;

#[cfg(feature = "lora")]
pub mod lora;

#[cfg(feature = "embassy")]
pub mod node;

#[cfg(feature = "storage")]
pub mod storage;

#[cfg(feature = "ui")]
pub mod ui;
