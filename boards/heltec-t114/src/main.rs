//! Heltec T114 (nRF52840 + SX1262) Reticulum node firmware.
//!
//! # Wiring — Heltec T114 default pin assignment
//!
//! | Signal     | nRF52840 pin | Notes                               |
//! |------------|--------------|-------------------------------------|
//! | SPI SCK    | P0.19        | SX1262 SPI clock                    |
//! | SPI MOSI   | P0.22        | SX1262 SPI MOSI                     |
//! | SPI MISO   | P0.23        | SX1262 SPI MISO                     |
//! | NSS (CS)   | P0.24        | SX1262 chip select (driven by SPI)  |
//! | RESET      | P0.25        | SX1262 hardware reset               |
//! | BUSY       | P0.17        | SX1262 busy indicator               |
//! | DIO1       | P0.20        | SX1262 IRQ / RxDone                 |
//! | ANT_RX_SW  | P0.13        | Antenna switch RX (active high)     |
//! | ANT_TX_SW  | P0.14        | Antenna switch TX (active high)     |
//!
//! Verify all pin assignments against your board schematic before flashing.
//!
//! # Identity persistence
//!
//! The node identity is stored in the **last 256-byte page** of internal
//! flash (address `IDENTITY_FLASH_OFFSET`).  On first boot, a new identity is
//! generated using the nRF52840 hardware RNG and persisted there.  Subsequent
//! boots load the stored identity, so the node address is stable across power
//! cycles.
//!
//! # Building
//!
//! ```sh
//! cargo build --release --target thumbv7em-none-eabihf
//! ```
//!
//! # Flashing
//!
//! ```sh
//! cargo run --release --target thumbv7em-none-eabihf
//! # or directly:
//! probe-rs run --chip nRF52840_xxAA target/thumbv7em-none-eabihf/release/heltec-t114
//! ```

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

use embassy_executor::Spawner;
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::nvmc::Nvmc;
use embassy_nrf::rng::Rng;
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::Delay;

use lora_phy::iv::GenericSx126xInterfaceVariant;
use lora_phy::sx126x::{Config as Sx126xConfig, Sx126x, Sx126xVariant};
use lora_phy::LoRa;

use reticulum_core::hash::{AddressHash, Hash};
use reticulum_core::routing::{RxMessage, TxMessage};

use reticulum_embassy::iface::{drive_interface, InterfaceRouter};

use reticulum_node::config::{LoraConfig, NodeConfig};
use reticulum_node::lora::LoraInterface;
use reticulum_node::node::run;
use reticulum_node::storage::load_or_generate;

// ── Flash layout ──────────────────────────────────────────────────────────────

/// nRF52840 internal flash: 1 MB total.  Identity lives in the last page
/// (page size = 4 KB on nRF52840, so last page starts at 0xFF000).
///
/// Adjust if your linker script allocates flash differently.
const IDENTITY_FLASH_OFFSET: u32 = 0x000F_F000;

// ── Pin assignments ───────────────────────────────────────────────────────────

type SpiPeripheral = peripherals::SPI3;
type PinNss = peripherals::P0_24;
type PinReset = peripherals::P0_25;
type PinBusy = peripherals::P0_17;
type PinDio1 = peripherals::P0_20;
type PinAntRx = peripherals::P0_13;
type PinAntTx = peripherals::P0_14;

// ── Channel capacities ────────────────────────────────────────────────────────

const RX_CAP: usize = 2;
const TX_CAP: usize = 2;

// ── Static channels ───────────────────────────────────────────────────────────

static RX: Channel<CriticalSectionRawMutex, RxMessage, RX_CAP> = Channel::new();
static TX_LORA: Channel<CriticalSectionRawMutex, TxMessage, TX_CAP> = Channel::new();

// ── Interrupt binding ─────────────────────────────────────────────────────────

bind_interrupts!(struct Irqs {
    SPIM3 => spim::InterruptHandler<SpiPeripheral>;
    RNG   => embassy_nrf::rng::InterruptHandler<peripherals::RNG>;
});

// ── Type aliases ──────────────────────────────────────────────────────────────

type Iv<'d> = GenericSx126xInterfaceVariant<Output<'d, PinReset>, Input<'d, PinDio1>>;

type Radio<'d> = Sx126x<Spim<'d, SpiPeripheral>, Output<'d, PinNss>, Iv<'d>, Sx126xVariant>;

// ── Embassy tasks ─────────────────────────────────────────────────────────────

#[embassy_executor::task]
async fn lora_task(iface: LoraInterface<Radio<'static>, Delay>, addr: AddressHash) {
    drive_interface::<_, 255>(iface, addr, RX.sender().into(), TX_LORA.receiver().into()).await;
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    defmt::info!("reticulum-node starting on nRF52840 / Heltec T114");

    let p = embassy_nrf::init(Default::default());

    // ── Hardware RNG ──────────────────────────────────────────────────────────
    let mut rng = Rng::new(p.RNG, Irqs);

    // ── Identity (load from flash or generate and persist) ────────────────────
    let mut nvmc = Nvmc::new(p.NVMC);
    let identity =
        load_or_generate(&mut nvmc, IDENTITY_FLASH_OFFSET, &mut rng).expect("identity init");
    let node_addr = *identity.address_hash();
    defmt::info!("node address: {:?}", defmt::Debug2Format(&node_addr));

    // ── SPI bus ───────────────────────────────────────────────────────────────
    let mut spi_config = spim::Config::default();
    spi_config.frequency = spim::Frequency::M8;

    let spi = Spim::new(
        p.SPI3, Irqs, p.P0_19, // SCK
        p.P0_22, // MOSI
        p.P0_23, // MISO
        spi_config,
    );

    let nss = Output::new(p.P0_24, Level::High, OutputDrive::Standard);
    let reset = Output::new(p.P0_25, Level::High, OutputDrive::Standard);
    let busy = Input::new(p.P0_17, Pull::None);
    let dio1 = Input::new(p.P0_20, Pull::None);
    let ant_rx = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);
    let ant_tx = Output::new(p.P0_14, Level::Low, OutputDrive::Standard);

    // ── Interface variant ─────────────────────────────────────────────────────
    let iv = GenericSx126xInterfaceVariant::new(reset, dio1, busy, Some(ant_rx), Some(ant_tx))
        .expect("IV init");

    // ── SX1262 radio ──────────────────────────────────────────────────────────
    let sx126x_config = Sx126xConfig {
        chip: Sx126xVariant::Sx1262,
        tcxo_ctrl: Some(lora_phy::sx126x::TcxoCtrlVoltage::Ctrl1V7),
        use_dcdc: true,
        rx_boost: false,
    };

    let radio = Sx126x::new(spi, nss, iv, sx126x_config);

    let lora = LoRa::new(radio, true, Delay).await.expect("LoRa init");

    let lora_config = LoraConfig::eu_868();
    let lora_iface = LoraInterface::new(lora, lora_config)
        .await
        .expect("LoraInterface init");

    // ── Interface address (derived from index 1) ──────────────────────────────
    let lora_addr = AddressHash::new_from_hash(&Hash::new_from_slice(&[1u8; 32]));

    // ── InterfaceRouter ───────────────────────────────────────────────────────
    let mut iface_router = InterfaceRouter::<1>::new();
    iface_router
        .register(lora_addr, TX_LORA.sender().into())
        .expect("register LoRa interface");

    // ── Spawn interface driver ────────────────────────────────────────────────
    spawner.must_spawn(lora_task(lora_iface, lora_addr));

    // ── Run the forwarding loop ───────────────────────────────────────────────
    let node_config = NodeConfig::embedded();
    run::<64, 32, 1>(node_config, node_addr, iface_router, RX.receiver().into()).await;
}
