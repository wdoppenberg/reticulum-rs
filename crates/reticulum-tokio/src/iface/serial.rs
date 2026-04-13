use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;

use crate::iface::RxMessage;
use reticulum_core::buffer::{InputBuffer, OutputBuffer};

use reticulum_core::packet::Packet;
use reticulum_core::serde::Serialize;

use super::hdlc::Hdlc;
use super::{Interface, InterfaceContext};

// TODO: Configure via features
const PACKET_TRACE: bool = false;

#[derive(Debug, Clone)]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
    pub data_bits: tokio_serial::DataBits,
    pub parity: tokio_serial::Parity,
    pub stop_bits: tokio_serial::StopBits,
}

pub struct SerialInterface {
    config: SerialConfig,
}

impl SerialInterface {
    pub fn new(
        port: String,
        baud_rate: u32,
        data_bits: u8,
        parity: Option<String>,
        stop_bits: u8,
    ) -> Self {
        let data_bits = match data_bits {
            5 => tokio_serial::DataBits::Five,
            6 => tokio_serial::DataBits::Six,
            7 => tokio_serial::DataBits::Seven,
            8 => tokio_serial::DataBits::Eight,
            _ => tokio_serial::DataBits::Eight,
        };

        let parity = match parity.as_deref() {
            Some("odd") | Some("Odd") => tokio_serial::Parity::Odd,
            Some("even") | Some("Even") => tokio_serial::Parity::Even,
            _ => tokio_serial::Parity::None,
        };

        let stop_bits = match stop_bits {
            2 => tokio_serial::StopBits::Two,
            _ => tokio_serial::StopBits::One,
        };

        Self {
            config: SerialConfig {
                port,
                baud_rate,
                data_bits,
                parity,
                stop_bits,
            },
        }
    }

    pub async fn spawn(context: InterfaceContext<Self>) {
        let config = { context.inner.lock().unwrap().config.clone() };
        let iface_address = context.channel.address;

        let (rx_channel, tx_channel) = context.channel.split();
        let tx_channel = Arc::new(tokio::sync::Mutex::new(tx_channel));

        loop {
            if context.cancel.is_cancelled() {
                break;
            }

            // Open serial port
            let port = tokio_serial::new(&config.port, config.baud_rate)
                .data_bits(config.data_bits)
                .parity(config.parity)
                .stop_bits(config.stop_bits)
                .open_native_async();

            let port = match port {
                Ok(port) => port,
                Err(e) => {
                    log::warn!(
                        "serial_interface: couldn't open port '{}': {}",
                        config.port,
                        e
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            let cancel = context.cancel.clone();
            let stop = CancellationToken::new();

            let (read_port, write_port) = tokio::io::split(port);

            log::info!(
                "serial_interface: opened port '{}' at {} baud",
                config.port,
                config.baud_rate
            );

            const BUFFER_SIZE: usize = core::mem::size_of::<Packet>() * 2;

            // Start receive task
            let rx_task = {
                let cancel = cancel.clone();
                let stop = stop.clone();
                let mut port = read_port;
                let rx_channel = rx_channel.clone();

                tokio::spawn(async move {
                    let mut hdlc_rx_buffer = [0u8; BUFFER_SIZE];
                    let mut rx_buffer = [0u8; BUFFER_SIZE + (BUFFER_SIZE / 2)];
                    let mut serial_buffer = [0u8; BUFFER_SIZE];

                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                    break;
                            }
                            _ = stop.cancelled() => {
                                    break;
                            }
                            result = port.read(&mut serial_buffer[..]) => {
                                    match result {
                                        Ok(0) => {
                                            log::warn!("serial_interface: connection closed");
                                            stop.cancel();
                                            break;
                                        }
                                        Ok(n) => {
                                            // Serial stream may contain several or partial HDLC frames
                                            for i in 0..n {
                                                // Push new byte from the end of buffer
                                                rx_buffer[BUFFER_SIZE-1] = serial_buffer[i];

                                                // Check if it contains a HDLC frame
                                                let frame = Hdlc::find(&rx_buffer[..]);
                                                if let Some(frame) = frame {
                                                    // Decode HDLC frame and deserialize packet
                                                    let frame_buffer = &mut rx_buffer[frame.0..frame.1+1];
                                                    let mut output = OutputBuffer::new(&mut hdlc_rx_buffer[..]);
                                                    if Hdlc::decode(frame_buffer, &mut output).is_ok() {
                                                        if let Ok(packet) = Packet::deserialize(&mut InputBuffer::new(output.as_slice())) {
                                                            if PACKET_TRACE {
                                                                log::trace!("serial_interface: rx << ({}) {}", iface_address, packet);
                                                            }
                                                            let _ = rx_channel.send(RxMessage { address: iface_address, packet }).await;
                                                        } else {
                                                            log::warn!("serial_interface: couldn't decode packet");
                                                        }
                                                    } else {
                                                        log::warn!("serial_interface: couldn't decode hdlc frame");
                                                    }

                                                    // Remove current HDLC frame data
                                                    frame_buffer.fill(0);
                                                } else {
                                                    // Move data left
                                                    rx_buffer.copy_within(1.., 0);
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            log::warn!("serial_interface: connection error {}", e);
                                            break;
                                        }
                                    }
                                },
                        };
                    }
                })
            };

            // Start transmit task
            let tx_task = {
                let cancel = cancel.clone();
                let tx_channel = tx_channel.clone();
                let mut port = write_port;

                tokio::spawn(async move {
                    loop {
                        if stop.is_cancelled() {
                            break;
                        }

                        let mut hdlc_tx_buffer = [0u8; BUFFER_SIZE];
                        let mut tx_buffer = [0u8; BUFFER_SIZE];

                        let mut tx_channel = tx_channel.lock().await;

                        tokio::select! {
                            _ = cancel.cancelled() => {
                                    break;
                            }
                            _ = stop.cancelled() => {
                                    break;
                            }
                            Some(message) = tx_channel.recv() => {
                                let packet = message.packet;
                                if PACKET_TRACE {
                                    log::trace!("serial_interface: tx >> ({}) {}", iface_address, packet);
                                }
                                let mut output = OutputBuffer::new(&mut tx_buffer);
                                if packet.serialize(&mut output).is_ok() {
                                    let mut hdlc_output = OutputBuffer::new(&mut hdlc_tx_buffer[..]);

                                    if Hdlc::encode(output.as_slice(), &mut hdlc_output).is_ok() {
                                        let _ = port.write_all(hdlc_output.as_slice()).await;
                                        let _ = port.flush().await;
                                    }
                                }
                            }
                        };
                    }
                })
            };

            tx_task.await.unwrap();
            rx_task.await.unwrap();

            log::info!("serial_interface: port '{}' closed", config.port);
        }
    }
}

impl Interface for SerialInterface {
    fn mtu() -> usize {
        508 // Conservative MTU for serial
    }
}
