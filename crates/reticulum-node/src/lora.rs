//! LoRa radio interface adapter.
//!
//! [`LoraInterface`] wraps a [`lora_phy::LoRa`] radio driver and presents it
//! as a [`reticulum_core::interface::Interface`], enabling it to be driven by
//! [`reticulum_embassy::iface::drive_interface`].
//!
//! # Usage
//!
//! ```rust,ignore
//! use reticulum_node::lora::LoraInterface;
//! use reticulum_node::config::LoraConfig;
//!
//! let config = LoraConfig::eu_868();
//! let lora_iface = LoraInterface::new(lora, config).await?;
//!
//! // Use with drive_interface:
//! drive_interface::<_, 255>(lora_iface, addr, rx_send, tx_recv).await;
//! ```
//!
//! # MTU
//!
//! The LoRa SX126x supports a maximum payload of 255 bytes.  `mtu()` always
//! returns 255.
//!
//! # RX / TX mode switching
//!
//! `receive()` puts the radio in **continuous RX** mode and blocks until a
//! packet arrives.  `transmit()` prepares the payload, fires the transmission,
//! and returns after the air-time completes.  The lora-phy 3.x API separates
//! `prepare_for_tx` (sets up payload + params) from `tx` (triggers the actual
//! transmission and waits for the TxDone IRQ).

use lora_phy::mod_params::{
    Bandwidth as LoraBw, CodingRate as LoraCr, ModulationParams, PacketParams, RadioError,
    SpreadingFactor as LoraSf,
};
use lora_phy::mod_traits::RadioKind;
use lora_phy::DelayNs;
use lora_phy::LoRa;
use lora_phy::RxMode;

use reticulum_core::interface::Interface;

use crate::config::{Bandwidth, CodingRate, LoraConfig, SpreadingFactor};

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors produced by [`LoraInterface`].
#[derive(Debug)]
pub enum LoraError {
    /// Error from the lora-phy radio driver.
    Radio(RadioError),
}

impl core::fmt::Display for LoraError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LoraError::Radio(e) => write!(f, "radio error: {:?}", e),
        }
    }
}

impl From<RadioError> for LoraError {
    fn from(e: RadioError) -> Self {
        LoraError::Radio(e)
    }
}

// ── LoraInterface ─────────────────────────────────────────────────────────────

/// Maximum LoRa payload for SX126x (255 bytes).
pub const LORA_MTU: usize = 255;

/// A Reticulum [`Interface`] adapter over a [`lora_phy::LoRa`] driver.
///
/// `RK` is the [`RadioKind`] type parameter (e.g.
/// `lora_phy::sx126x::SX1261_2<...>`).
/// `DLY` is the delay provider implementing [`DelayNs`].
pub struct LoraInterface<RK: RadioKind, DLY: DelayNs> {
    lora: LoRa<RK, DLY>,
    config: LoraConfig,
    mdltn_params: ModulationParams,
    rx_pkt_params: PacketParams,
    tx_pkt_params: PacketParams,
}

impl<RK: RadioKind, DLY: DelayNs> LoraInterface<RK, DLY> {
    /// Initialise the radio and build modulation / packet parameters from
    /// `config`.
    pub async fn new(mut lora: LoRa<RK, DLY>, config: LoraConfig) -> Result<Self, LoraError> {
        let sf = map_sf(config.spreading_factor);
        let bw = map_bw(config.bandwidth);
        let cr = map_cr(config.coding_rate);

        let mdltn_params = lora
            .create_modulation_params(sf, bw, cr, config.frequency_hz)
            .map_err(LoraError::Radio)?;

        let rx_pkt_params = lora
            .create_rx_packet_params(
                config.preamble_len,
                false, // explicit header
                config.max_payload,
                config.crc_enabled,
                false, // standard IQ
                &mdltn_params,
            )
            .map_err(LoraError::Radio)?;

        let tx_pkt_params = lora
            .create_tx_packet_params(
                config.preamble_len,
                false, // explicit header
                config.crc_enabled,
                false, // standard IQ
                &mdltn_params,
            )
            .map_err(LoraError::Radio)?;

        Ok(Self {
            lora,
            config,
            mdltn_params,
            rx_pkt_params,
            tx_pkt_params,
        })
    }

    /// Return a reference to the underlying [`LoRa`] driver.
    pub fn inner(&self) -> &LoRa<RK, DLY> {
        &self.lora
    }

    /// Return a mutable reference to the underlying [`LoRa`] driver.
    pub fn inner_mut(&mut self) -> &mut LoRa<RK, DLY> {
        &mut self.lora
    }

    /// Unwrap, returning the underlying [`LoRa`] driver.
    pub fn into_inner(self) -> LoRa<RK, DLY> {
        self.lora
    }
}

impl<RK: RadioKind, DLY: DelayNs> Interface for LoraInterface<RK, DLY> {
    type Error = LoraError;

    /// Receive one Reticulum frame from the radio.
    ///
    /// Puts the radio in **continuous RX** mode and blocks until a packet
    /// arrives.  The decoded payload (without LoRa framing) is written into
    /// `buf`.
    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.lora
            .prepare_for_rx(RxMode::Continuous, &self.mdltn_params, &self.rx_pkt_params)
            .await?;

        let (n, _status) = self.lora.rx(&self.rx_pkt_params, buf).await?;
        Ok(n as usize)
    }

    /// Transmit a Reticulum frame via the radio.
    ///
    /// Calls `prepare_for_tx` with the payload and TX power, then calls `tx()`
    /// which blocks until the TxDone IRQ fires.
    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.lora
            .prepare_for_tx(
                &self.mdltn_params,
                &mut self.tx_pkt_params,
                self.config.tx_power_dbm as i32,
                frame,
            )
            .await?;

        self.lora.tx().await.map_err(LoraError::Radio)
    }

    /// Always returns 255 for SX126x LoRa.
    fn mtu(&self) -> usize {
        LORA_MTU
    }
}

// ── Parameter conversions ─────────────────────────────────────────────────────

fn map_sf(sf: SpreadingFactor) -> LoraSf {
    match sf {
        SpreadingFactor::SF7 => LoraSf::_7,
        SpreadingFactor::SF8 => LoraSf::_8,
        SpreadingFactor::SF9 => LoraSf::_9,
        SpreadingFactor::SF10 => LoraSf::_10,
        SpreadingFactor::SF11 => LoraSf::_11,
        SpreadingFactor::SF12 => LoraSf::_12,
    }
}

fn map_bw(bw: Bandwidth) -> LoraBw {
    match bw {
        Bandwidth::BW125 => LoraBw::_125KHz,
        Bandwidth::BW250 => LoraBw::_250KHz,
        Bandwidth::BW500 => LoraBw::_500KHz,
    }
}

fn map_cr(cr: CodingRate) -> LoraCr {
    match cr {
        CodingRate::Cr45 => LoraCr::_4_5,
        CodingRate::Cr46 => LoraCr::_4_6,
        CodingRate::Cr47 => LoraCr::_4_7,
        CodingRate::Cr48 => LoraCr::_4_8,
    }
}
