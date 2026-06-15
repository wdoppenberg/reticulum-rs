//! BLE GATT peripheral for the Reticulum node.
//!
//! Exposes a single custom GATT service with two characteristics:
//!
//! | Characteristic | UUID                                   | Properties            |
//! |----------------|----------------------------------------|-----------------------|
//! | `from_client`  | `b3e90002-d4d4-4c34-b4e9-a8b65e3c6efd` | Write-without-response |
//! | `to_client`    | `b3e90003-d4d4-4c34-b4e9-a8b65e3c6efd` | Notify                |
//!
//! # Architecture
//!
//! ```text
//!  BLE Client
//!      │ write to from_client          notify to_client │
//!      ▼                                                 ▲
//!  GATT task  ──BLE_INNER_RX──▶  BleInterface  ──BLE_INNER_TX──▶
//!      │                                                 │
//!      └─── runs drive_interface<BleInterface, MTU> ─────┘
//!                      │
//!               shared RX channel ──▶ reticulum_node::run()
//! ```

use core::fmt;

use defmt::warn;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{DynamicReceiver, DynamicSender};
use heapless::Vec;
use static_cell::StaticCell;
use trouble_host::prelude::*;

use reticulum_core::interface::Interface;

// ── MTU / sizing ──────────────────────────────────────────────────────────────

/// Requested ATT_MTU for the BLE connection.
pub const BLE_ATT_MTU: u16 = 247;

/// Maximum usable bytes per BLE PDU (ATT_MTU − 3 byte header).
pub const BLE_MTU: usize = (BLE_ATT_MTU as usize) - 3;

// ── Host resource constants ───────────────────────────────────────────────────

const CONNECTIONS: usize = 1;
const L2CAP_CHANNELS: usize = 2;

// ── GATT server definition ────────────────────────────────────────────────────

#[gatt_server(mutex_type = CriticalSectionRawMutex, connections_max = 1)]
pub struct ReticulumServer {
    reticulum: ReticulumService,
}

#[gatt_service(uuid = "b3e90001-d4d4-4c34-b4e9-a8b65e3c6efd")]
struct ReticulumService {
    /// Raw Reticulum frame written by the connected app to the node.
    #[characteristic(uuid = "b3e90002-d4d4-4c34-b4e9-a8b65e3c6efd", write_without_response)]
    from_client: Vec<u8, 244>,

    /// Raw Reticulum frame notified from the node to the connected app.
    #[characteristic(uuid = "b3e90003-d4d4-4c34-b4e9-a8b65e3c6efd", notify)]
    to_client: Vec<u8, 244>,
}

// ── Frame type ────────────────────────────────────────────────────────────────

/// A single Reticulum frame exchanged over the two internal channels.
pub struct BleFrame {
    data: [u8; BLE_MTU],
    len: usize,
}

impl BleFrame {
    fn from_slice(src: &[u8]) -> Self {
        let mut f = Self { data: [0; BLE_MTU], len: 0 };
        let n = src.len().min(BLE_MTU);
        f.data[..n].copy_from_slice(&src[..n]);
        f.len = n;
        f
    }
}

// ── Error type ────────────────────────────────────────────────────────────────

/// Error returned by [`BleInterface`] I/O operations.
#[derive(Debug, defmt::Format)]
pub enum BleError {
    /// Frame is larger than [`BLE_MTU`]; it was dropped.
    FrameTooLarge,
}

impl fmt::Display for BleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BLE frame too large (> {} bytes)", BLE_MTU)
    }
}

// ── BleInterface ──────────────────────────────────────────────────────────────

/// Reticulum [`Interface`] adapter for the BLE GATT peripheral.
pub struct BleInterface<'a> {
    rx: DynamicReceiver<'a, BleFrame>,
    tx: DynamicSender<'a, BleFrame>,
}

impl<'a> BleInterface<'a> {
    pub fn new(rx: DynamicReceiver<'a, BleFrame>, tx: DynamicSender<'a, BleFrame>) -> Self {
        Self { rx, tx }
    }
}

impl Interface for BleInterface<'_> {
    type Error = BleError;

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, BleError> {
        let frame = self.rx.receive().await;
        let n = frame.len.min(buf.len());
        buf[..n].copy_from_slice(&frame.data[..n]);
        Ok(n)
    }

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), BleError> {
        if frame.len() > BLE_MTU {
            warn!(
                "BleInterface: dropping oversized frame ({} > {} bytes)",
                frame.len(),
                BLE_MTU
            );
            return Err(BleError::FrameTooLarge);
        }
        self.tx.send(BleFrame::from_slice(frame)).await;
        Ok(())
    }

    fn mtu(&self) -> usize {
        BLE_MTU
    }
}

// ── BLE host resources (static) ───────────────────────────────────────────────

type Controller = nrf_sdc::SoftdeviceController<'static>;
type HostRes = HostResources<Controller, DefaultPacketPool, CONNECTIONS, L2CAP_CHANNELS>;

static HOST_RESOURCES: StaticCell<HostRes> = StaticCell::new();

// ── Embassy task ─────────────────────────────────────────────────────────────

/// Embassy task that owns the BLE controller and runs the GATT server.
#[embassy_executor::task]
pub async fn ble_task(
    controller: nrf_sdc::SoftdeviceController<'static>,
    inner_rx_send: DynamicSender<'static, BleFrame>,
    inner_tx_recv: DynamicReceiver<'static, BleFrame>,
) {
    let resources = HOST_RESOURCES.init(HostResources::new());
    let stack = trouble_host::new(controller, resources)
        .set_random_address(Address::random([0x11, 0x52, 0x65, 0x74, 0x69, 0x63]))
        .build();

    let mut runner = stack.runner();

    let _ = embassy_futures::join::join(runner.run(), async {
        loop {
            let mut peripheral = stack.peripheral();
            advertise_and_run(&mut peripheral, &inner_rx_send, &inner_tx_recv).await;
        }
    })
    .await;
}

// ── Per-connection logic ──────────────────────────────────────────────────────

async fn advertise_and_run(
    peripheral: &mut Peripheral<'_, Controller, DefaultPacketPool>,
    inner_rx_send: &DynamicSender<'_, BleFrame>,
    inner_tx_recv: &DynamicReceiver<'_, BleFrame>,
) {
    // Encode advertisement payload as raw bytes.
    let mut adv_buf = [0u8; 31];
    let adv_len = match AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::CompleteLocalName(b"Reticulum Node"),
        ],
        &mut adv_buf,
    ) {
        Ok(n) => n,
        Err(_) => {
            defmt::warn!("BLE: adv encode failed");
            return;
        }
    };

    let mut scan_buf = [0u8; 31];
    let scan_len = match AdStructure::encode_slice(
        &[AdStructure::CompleteLocalName(b"Reticulum Node")],
        &mut scan_buf,
    ) {
        Ok(n) => n,
        Err(_) => {
            defmt::warn!("BLE: scan encode failed");
            return;
        }
    };

    defmt::info!("BLE: advertising as 'Reticulum Node'");

    let advertiser = match peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &adv_buf[..adv_len],
                scan_data: &scan_buf[..scan_len],
            },
        )
        .await
    {
        Ok(a) => a,
        Err(e) => {
            defmt::warn!("BLE: advertise error: {:?}", defmt::Debug2Format(&e));
            return;
        }
    };

    let conn = match advertiser.accept().await {
        Ok(c) => c,
        Err(e) => {
            defmt::warn!("BLE: accept error: {:?}", defmt::Debug2Format(&e));
            return;
        }
    };

    defmt::info!("BLE: client connected");

    let server = match ReticulumServer::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "Reticulum Node",
        appearance: &appearance::UNKNOWN,
    })) {
        Ok(s) => s,
        Err(e) => {
            defmt::warn!("BLE: server init error: {:?}", defmt::Debug2Format(&e));
            return;
        }
    };

    let conn = match conn.with_attribute_server(&server) {
        Ok(c) => c,
        Err(e) => {
            defmt::warn!("BLE: with_attribute_server error: {:?}", defmt::Debug2Format(&e));
            return;
        }
    };

    let from_client_handle = server.reticulum.from_client.handle();

    loop {
        match select(conn.next(), inner_tx_recv.receive()).await {
            Either::First(GattConnectionEvent::Disconnected { .. }) => {
                defmt::info!("BLE: client disconnected");
                break;
            }

            Either::First(GattConnectionEvent::Gatt { event: GattEvent::Write(write) }) => {
                if write.handle() == from_client_handle {
                    defmt::debug!("BLE rx: {} bytes from client", write.data().len());
                    inner_rx_send.send(BleFrame::from_slice(write.data())).await;
                }
                // Drop auto-processes (sends response or no-op for write_without_response).
            }

            Either::First(_) => {}

            Either::Second(frame) => {
                let payload = Vec::<u8, 244>::from_slice(&frame.data[..frame.len])
                    .unwrap_or_default();
                if let Err(e) = server.reticulum.to_client.notify(&conn, &payload).await {
                    defmt::warn!("BLE: notify error: {:?}", defmt::Debug2Format(&e));
                    break;
                }
            }
        }
    }
}
