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
//! # BLE connectivity
//!
//! The nRF52840's built-in BLE radio is driven by the Nordic SoftDevice
//! Controller (nrf-sdc, open-source LGPL) managed through the Multiprotocol
//! Service Layer (nrf-mpsl).  No proprietary SoftDevice binary is required and
//! the application starts at address 0x00000000.
//!
//! The node advertises as "Reticulum Node" and accepts one BLE connection at a
//! time.  Connected apps can write raw Reticulum frames to the `from_client`
//! characteristic and receive forwarded frames via `to_client` notifications.
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

#[cfg(feature = "ui")]
mod ui;

mod ble;

use defmt_rtt as _;
use panic_probe as _;

extern crate alloc;

use core::mem::MaybeUninit;

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::mode::Blocking;
use embassy_nrf::nvmc::Nvmc;
use embassy_nrf::rng::Rng;
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{bind_interrupts, peripherals};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex;
use embassy_time::Delay;
use embedded_alloc::LlffHeap as Heap;
use lora_phy::iv::GenericSx126xInterfaceVariant;
use lora_phy::sx126x::{Config as Sx126xConfig, Sx1262, Sx126x};
use lora_phy::LoRa;
use rand_core::{Infallible, TryCryptoRng, TryRng};
use static_cell::StaticCell;

use reticulum_core::hash::{AddressHash, Hash};
use reticulum_core::routing::{RxMessage, TxMessage};
use reticulum_embassy::iface::{drive_interface, InterfaceRouter};
use reticulum_node::config::{LoraConfig, RouterConfig};
use reticulum_node::lora::LoraInterface;
use reticulum_node::node::run;
use reticulum_node::storage::load_or_generate;

use ble::{ble_task, BleFrame, BleInterface};

// ── Flash layout ──────────────────────────────────────────────────────────────

/// Last 4 KB of the firmware flash region (0x26000 + 820K = 0xF3000).
/// Must stay within FLASH in memory.x and away from the bootloader settings page at 0xFF000.
const IDENTITY_FLASH_OFFSET: u32 = 0x000F_3000;

// ── Pin assignments ───────────────────────────────────────────────────────────

type SpiPeripheral = peripherals::SPI3;

// ── Channel capacities ────────────────────────────────────────────────────────

const RX_CAP: usize = 2;
const TX_CAP: usize = 2;

// ── Static channels ───────────────────────────────────────────────────────────

/// Shared inbound channel: both LoRa and BLE deposit received packets here.
static RX: Channel<CriticalSectionRawMutex, RxMessage, RX_CAP> = Channel::new();

/// Outbound channel for the LoRa interface.
static TX_LORA: Channel<CriticalSectionRawMutex, TxMessage, TX_CAP> = Channel::new();

/// Outbound channel for the BLE interface.
static TX_BLE: Channel<CriticalSectionRawMutex, TxMessage, TX_CAP> = Channel::new();

/// Internal channel: GATT task → BleInterface (client-written frames).
static BLE_INNER_RX: Channel<CriticalSectionRawMutex, BleFrame, 2> = Channel::new();

/// Internal channel: BleInterface → GATT task (frames to notify).
static BLE_INNER_TX: Channel<CriticalSectionRawMutex, BleFrame, 2> = Channel::new();

static SPI_BUS: StaticCell<Mutex<CriticalSectionRawMutex, Spim<'static>>> = StaticCell::new();

#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 8192;
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

// ── SDC static memory ─────────────────────────────────────────────────────────
// 4720 bytes is the documented minimum for 1 peripheral link on nRF52840;
// use 8 KiB to give headroom for longer data lengths.
const SDC_MEM_SIZE: usize = 8192;
static SDC_MEM: StaticCell<nrf_sdc::Mem<SDC_MEM_SIZE>> = StaticCell::new();

// ── SDC RNG (rand_core 0.9) ───────────────────────────────────────────────────

/// Owns the hardware RNG and implements the `rand_core 0.9` traits required
/// by `nrf-sdc`.  Stored in a `StaticCell` so the controller can be `'static`.
struct SdcRng(Rng<'static, Blocking>);

// SAFETY: `Rng<'static, Blocking>` holds a PAC peripheral pointer.  No other
// code touches the RNG peripheral after `SdcRng` is constructed.
unsafe impl Send for SdcRng {}

impl rand_core09::RngCore for SdcRng {
    fn next_u32(&mut self) -> u32 {
        self.0.blocking_next_u32()
    }

    fn next_u64(&mut self) -> u64 {
        self.0.blocking_next_u64()
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.blocking_fill_bytes(dest);
    }
}

impl rand_core09::CryptoRng for SdcRng {}

static SDC_RNG: StaticCell<SdcRng> = StaticCell::new();

// ── Identity RNG adapter (rand_core 0.10) ────────────────────────────────────

struct NrfRngAdapter<'a>(&'a mut Rng<'static, Blocking>);

impl TryRng for NrfRngAdapter<'_> {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(self.0.blocking_next_u32())
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(self.0.blocking_next_u64())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        self.0.blocking_fill_bytes(dst);
        Ok(())
    }
}

impl TryCryptoRng for NrfRngAdapter<'_> {}

// ── Interrupt binding ─────────────────────────────────────────────────────────

bind_interrupts!(struct Irqs {
    // LoRa SPI
    SPIM3 => spim::InterruptHandler<SpiPeripheral>;

    // MPSL high-priority (radio timing — must not be masked by critical sections)
    RADIO       => nrf_mpsl::HighPrioInterruptHandler;
    TIMER0      => nrf_mpsl::HighPrioInterruptHandler;
    RTC0        => nrf_mpsl::HighPrioInterruptHandler;

    // MPSL low-priority (protocol events)
    EGU0_SWI0 => nrf_mpsl::LowPrioInterruptHandler;

    // Clock / power management (MPSL LFXO calibration)
    CLOCK_POWER => nrf_mpsl::ClockInterruptHandler;
});

// ── Type aliases ──────────────────────────────────────────────────────────────

type Iv<'d> = GenericSx126xInterfaceVariant<Output<'d>, Input<'d>>;
type Radio<'d> =
    Sx126x<SpiDevice<'d, CriticalSectionRawMutex, Spim<'static>, Output<'static>>, Iv<'d>, Sx1262>;

// ── Embassy tasks ─────────────────────────────────────────────────────────────

#[embassy_executor::task]
async fn lora_task(iface: LoraInterface<Radio<'static>, Delay>, addr: AddressHash) {
    drive_interface::<_, 255>(iface, addr, RX.sender().into(), TX_LORA.receiver().into()).await;
}

#[embassy_executor::task]
async fn ble_iface_task(iface: BleInterface<'static>, addr: AddressHash) {
    drive_interface::<_, { ble::BLE_MTU }>(
        iface,
        addr,
        RX.sender().into(),
        TX_BLE.receiver().into(),
    )
    .await;
}

/// Runs the MPSL low-priority event loop.  Must be a dedicated task so that
/// the low-priority handler can yield to higher-priority radio events.
#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static nrf_mpsl::MultiprotocolServiceLayer<'static>) {
    mpsl.run().await;
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    defmt::info!("reticulum-node starting on nRF52840 / Heltec T114");

    unsafe {
        HEAP.init(
            core::ptr::addr_of_mut!(HEAP_MEM) as *mut u8 as usize,
            HEAP_SIZE,
        );
    }

    let p = embassy_nrf::init(Default::default());

    // ── Hardware RNG ──────────────────────────────────────────────────────────
    let mut rng = Rng::new_blocking(p.RNG);

    // ── Identity (load from flash or generate and persist) ────────────────────
    let mut nvmc = Nvmc::new(p.NVMC);
    let identity = {
        let mut rng_adapter = NrfRngAdapter(&mut rng);
        load_or_generate(&mut nvmc, IDENTITY_FLASH_OFFSET, &mut rng_adapter)
            .expect("identity init")
    };
    let node_addr = *identity.address_hash();
    defmt::info!("node address: {:?}", defmt::Debug2Format(&node_addr));

    // ── MPSL — must be initialised before SDC ─────────────────────────────────
    static MPSL: StaticCell<nrf_mpsl::MultiprotocolServiceLayer<'static>> = StaticCell::new();

    let mpsl_periph = nrf_mpsl::Peripherals::new(
        p.RTC0,
        p.TIMER0,
        p.TEMP,
        p.PPI_CH19,
        p.PPI_CH30,
        p.PPI_CH31,
    );

    let lfclk_cfg = nrf_mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: nrf_mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: 16,        // calibrate every 4 s
        rc_temp_ctiv: 2,    // calibrate after 0.5 °C change
        accuracy_ppm: 500,
        skip_wait_lfclk_started: false,
    };

    let mpsl = MPSL.init(
        nrf_mpsl::MultiprotocolServiceLayer::new(mpsl_periph, Irqs, lfclk_cfg)
            .expect("MPSL init"),
    );

    spawner.spawn(mpsl_task(mpsl).expect("mpsl_task"));

    // ── SDC (SoftDevice Controller) ───────────────────────────────────────────
    let sdc_periph = nrf_sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21,
        p.PPI_CH22, p.PPI_CH23, p.PPI_CH24, p.PPI_CH25,
        p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );

    let sdc_mem = SDC_MEM.init(nrf_sdc::Mem::new());
    let sdc_rng = SDC_RNG.init(SdcRng(rng));

    let sdc_controller = nrf_sdc::Builder::new()
        .expect("SDC Builder::new")
        .support_peripheral()
        .peripheral_count(1)
        .expect("SDC peripheral_count")
        .build(sdc_periph, sdc_rng, mpsl, sdc_mem)
        .expect("SDC build");

    // ── SPI bus ───────────────────────────────────────────────────────────────
    let mut spi_config = spim::Config::default();
    spi_config.frequency = spim::Frequency::M8;

    let spi = Spim::new(
        p.SPI3, Irqs, p.P0_19, // SCK
        p.P0_22, // MOSI
        p.P0_23, // MISO
        spi_config,
    );

    let nss   = Output::new(p.P0_24, Level::High, OutputDrive::Standard);
    let reset = Output::new(p.P0_25, Level::High, OutputDrive::Standard);
    let busy  = Input::new(p.P0_17, Pull::None);
    let dio1  = Input::new(p.P0_20, Pull::None);
    let ant_rx = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);
    let ant_tx = Output::new(p.P0_14, Level::Low, OutputDrive::Standard);

    // ── Interface variant ─────────────────────────────────────────────────────
    let iv = GenericSx126xInterfaceVariant::new(reset, dio1, busy, Some(ant_rx), Some(ant_tx))
        .expect("IV init");

    let spi_bus    = SPI_BUS.init(Mutex::new(spi));
    let spi_device = SpiDevice::new(spi_bus, nss);

    // ── SX1262 radio ──────────────────────────────────────────────────────────
    let sx126x_config = Sx126xConfig {
        chip:      Sx1262,
        tcxo_ctrl: Some(lora_phy::sx126x::TcxoCtrlVoltage::Ctrl1V7),
        use_dcdc:  true,
        rx_boost:  false,
    };

    let radio = Sx126x::new(spi_device, iv, sx126x_config);
    let lora  = LoRa::new(radio, true, Delay).await.expect("LoRa init");

    let lora_config = LoraConfig::eu_868();
    let lora_iface  = LoraInterface::new(lora, lora_config)
        .await
        .expect("LoraInterface init");

    // ── Interface addresses ───────────────────────────────────────────────────
    // Derived from fixed indices so they are stable across reboots.
    let lora_addr = AddressHash::new_from_hash(&Hash::new_from_slice(&[1u8; 32]));
    let ble_addr  = AddressHash::new_from_hash(&Hash::new_from_slice(&[2u8; 32]));

    // ── InterfaceRouter (2 interfaces: LoRa + BLE) ────────────────────────────
    let mut iface_router = InterfaceRouter::<2>::new();
    iface_router
        .register(lora_addr, TX_LORA.sender().into())
        .expect("register LoRa");
    iface_router
        .register(ble_addr, TX_BLE.sender().into())
        .expect("register BLE");

    // ── BLE interface adapter ─────────────────────────────────────────────────
    let ble_iface = BleInterface::new(
        BLE_INNER_RX.receiver().into(),
        BLE_INNER_TX.sender().into(),
    );

    // ── Spawn interface drivers ───────────────────────────────────────────────
    spawner.spawn(lora_task(lora_iface, lora_addr).expect("lora_task"));

    spawner.spawn(ble_iface_task(ble_iface, ble_addr).expect("ble_iface_task"));

    spawner.spawn(
        ble_task(
            sdc_controller,
            BLE_INNER_RX.sender().into(),
            BLE_INNER_TX.receiver().into(),
        )
        .expect("ble_task"),
    );

    // ── Run the forwarding loop ───────────────────────────────────────────────
    let node_config = RouterConfig::embedded();
    run::<64, 32, 2>(node_config, node_addr, iface_router, RX.receiver().into()).await;
}
