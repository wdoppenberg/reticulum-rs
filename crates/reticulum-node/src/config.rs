//! Node and LoRa configuration types.
//!
//! [`RouterConfig`] controls routing behaviour (announce forwarding, transport
//! mode).  [`LoraConfig`] carries all radio parameters and includes factory
//! presets for common Reticulum LoRa frequency plans.  The two types are kept
//! separate because they are consumed at different call sites:
//! [`LoraConfig`] is passed to [`LoraInterface::new`], while [`RouterConfig`]
//! is passed to [`run`].
//!
//! [`LoraInterface::new`]: crate::lora::LoraInterface::new
//! [`run`]: crate::node::run
//!
//! All types are `no_std` compatible and `Copy` so they can live in
//! `static` storage without any allocation.

// ── LoRa configuration ────────────────────────────────────────────────────────

/// LoRa spreading factor.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadingFactor {
    /// SF7 — fastest, shortest range.
    SF7,
    /// SF8.
    SF8,
    /// SF9.
    SF9,
    /// SF10.
    SF10,
    /// SF11.
    SF11,
    /// SF12 — slowest, longest range.
    SF12,
}

/// LoRa bandwidth in kHz.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    /// 125 kHz (Reticulum default).
    BW125,
    /// 250 kHz.
    BW250,
    /// 500 kHz.
    BW500,
}

/// LoRa coding rate denominator (4/x).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingRate {
    /// 4/5 — most error correction (Reticulum default).
    Cr45,
    /// 4/6.
    Cr46,
    /// 4/7.
    Cr47,
    /// 4/8 — least overhead.
    Cr48,
}

/// All radio parameters required to bring up a LoRa interface.
///
/// Use one of the regional factory constructors ([`eu_868`](Self::eu_868),
/// [`aus_nz`](Self::aus_nz), [`us_915`](Self::us_915)) to get a
/// Reticulum-compatible starting point, then override fields as needed.
#[derive(Debug, Clone, Copy)]
pub struct LoraConfig {
    /// Centre frequency in Hz.
    pub frequency_hz: u32,
    /// Spreading factor (default SF7 for Reticulum).
    pub spreading_factor: SpreadingFactor,
    /// Bandwidth (default 125 kHz for Reticulum).
    pub bandwidth: Bandwidth,
    /// Coding rate (default 4/5 for Reticulum).
    pub coding_rate: CodingRate,
    /// Preamble length in symbols (Reticulum: 8).
    pub preamble_len: u16,
    /// TX output power in dBm (0–22 for SX1262).
    pub tx_power_dbm: i8,
    /// CRC enabled on received packets.
    pub crc_enabled: bool,
    /// Maximum payload length in bytes (LoRa SX126x: 255).
    pub max_payload: u8,
}

impl LoraConfig {
    /// EU 868 MHz Reticulum preset (868.0 MHz, SF7, BW125, CR4/5).
    pub const fn eu_868() -> Self {
        Self {
            frequency_hz: 868_000_000,
            spreading_factor: SpreadingFactor::SF7,
            bandwidth: Bandwidth::BW125,
            coding_rate: CodingRate::Cr45,
            preamble_len: 8,
            tx_power_dbm: 14,
            crc_enabled: true,
            max_payload: 255,
        }
    }

    /// Australia / New Zealand 915 MHz preset (915.2 MHz, SF7, BW125, CR4/5).
    pub const fn aus_nz() -> Self {
        Self {
            frequency_hz: 915_200_000,
            ..Self::eu_868()
        }
    }

    /// US 915 MHz preset (915.2 MHz, SF7, BW125, CR4/5).
    pub const fn us_915() -> Self {
        Self::aus_nz()
    }

    /// Asia 433 MHz preset (433.175 MHz, SF7, BW125, CR4/5).
    pub const fn asia_433() -> Self {
        Self {
            frequency_hz: 433_175_000,
            ..Self::eu_868()
        }
    }
}

impl Default for LoraConfig {
    fn default() -> Self {
        Self::eu_868()
    }
}

// ── Router configuration ──────────────────────────────────────────────────────

/// Routing behaviour configuration passed to [`run`](crate::node::run).
///
/// Radio parameters belong in [`LoraConfig`]; deduplication and path-table
/// sizes are const-generic parameters on [`Router`](crate::router::Router)
/// and [`run`](crate::node::run) — they are intentionally not runtime values.
#[derive(Debug, Clone, Copy)]
pub struct RouterConfig {
    /// Forward announces received on one interface to all others.
    pub forward_announces: bool,
    /// Forward data packets between interfaces (transport / relay mode).
    pub transport_enabled: bool,
}

impl RouterConfig {
    /// Sensible defaults for an embedded forwarding node (both flags enabled).
    pub const fn embedded() -> Self {
        Self {
            forward_announces: true,
            transport_enabled: true,
        }
    }
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self::embedded()
    }
}
