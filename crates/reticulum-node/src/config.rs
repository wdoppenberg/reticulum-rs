//! Node and LoRa configuration types.
//!
//! [`NodeConfig`] is the top-level configuration for a `reticulum-node`
//! instance.  [`LoraConfig`] carries all radio parameters and includes
//! factory presets for common Reticulum LoRa frequency plans.
//!
//! All types are `no_std` compatible and `Copy` so they can live in
//! `static` storage without any allocation.

// ── LoRa configuration ────────────────────────────────────────────────────────

/// LoRa spreading factor.
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

// ── Node configuration ────────────────────────────────────────────────────────

/// Top-level configuration for a `reticulum-node` instance.
#[derive(Debug, Clone, Copy)]
pub struct NodeConfig {
    /// LoRa radio parameters.
    pub lora: LoraConfig,
    /// Maximum number of recently-seen packet hashes to track for
    /// deduplication.  Must be a power of two.  Recommended: 64.
    pub dedup_capacity: usize,
    /// Maximum number of path-table entries.  Recommended: 64.
    pub path_table_capacity: usize,
    /// Whether to forward announces received on one interface to others.
    pub forward_announces: bool,
    /// Whether to forward data packets between interfaces (transport mode).
    pub transport_enabled: bool,
}

impl NodeConfig {
    /// Sensible defaults for a low-memory embedded node.
    pub const fn embedded() -> Self {
        Self {
            lora: LoraConfig::eu_868(),
            dedup_capacity: 64,
            path_table_capacity: 32,
            forward_announces: true,
            transport_enabled: true,
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::embedded()
    }
}
