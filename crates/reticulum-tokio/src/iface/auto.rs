use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::iface::RxMessage;
use reticulum_core::buffer::{InputBuffer, OutputBuffer};
use reticulum_core::error::RnsError;
use reticulum_core::packet::Packet;
use reticulum_core::serde::Serialize;

use super::{Interface, InterfaceContext};

// TODO: Configure via features
const PACKET_TRACE: bool = false;

/// Default multicast group for Auto interface discovery
const DEFAULT_GROUP: &str = "239.255.0.1";
/// Default discovery port
const DEFAULT_DISCOVERY_PORT: u16 = 29716;
/// Default data port
const DEFAULT_DATA_PORT: u16 = 42671;

pub struct AutoInterface {
    group: String,
    discovery_port: u16,
    data_port: u16,
}

impl AutoInterface {
    pub fn new(
        group: Option<String>,
        discovery_port: Option<u16>,
        data_port: Option<u16>,
    ) -> Self {
        Self {
            group: group.unwrap_or_else(|| DEFAULT_GROUP.to_string()),
            discovery_port: discovery_port.unwrap_or(DEFAULT_DISCOVERY_PORT),
            data_port: data_port.unwrap_or(DEFAULT_DATA_PORT),
        }
    }

    pub async fn spawn(context: InterfaceContext<Self>) {
        let group = { context.inner.lock().unwrap().group.clone() };
        let discovery_port = { context.inner.lock().unwrap().discovery_port };
        let data_port = { context.inner.lock().unwrap().data_port };
        let iface_address = context.channel.address;

        let (rx_channel, tx_channel) = context.channel.split();
        let tx_channel = Arc::new(tokio::sync::Mutex::new(tx_channel));

        loop {
            if context.cancel.is_cancelled() {
                break;
            }

            // Bind to data port for receiving packets
            let bind_addr = format!("0.0.0.0:{}", data_port);
            let socket = UdpSocket::bind(&bind_addr)
                .await
                .map_err(|_| RnsError::ConnectionError);

            if socket.is_err() {
                log::info!("auto_interface: couldn't bind to <{}>", bind_addr);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }

            let socket = socket.unwrap();

            // Join multicast group
            let multicast_addr: Ipv4Addr = match group.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    log::error!("auto_interface: invalid multicast group '{}': {}", group, e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            if let Err(e) = socket.join_multicast_v4(multicast_addr, Ipv4Addr::UNSPECIFIED) {
                log::warn!(
                    "auto_interface: couldn't join multicast group {}: {}",
                    group,
                    e
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }

            let cancel = context.cancel.clone();
            let stop = CancellationToken::new();

            let read_socket = Arc::new(socket);
            let write_socket = read_socket.clone();

            log::info!(
                "auto_interface: listening on {} (group: {}, data_port: {})",
                bind_addr,
                group,
                data_port
            );

            const BUFFER_SIZE: usize = core::mem::size_of::<Packet>() * 3;

            // Start receive task
            let rx_task = {
                let cancel = cancel.clone();
                let stop = stop.clone();
                let socket = read_socket;
                let rx_channel = rx_channel.clone();

                tokio::spawn(async move {
                    loop {
                        let mut rx_buffer = [0u8; BUFFER_SIZE];

                        tokio::select! {
                            _ = cancel.cancelled() => {
                                    break;
                            }
                            _ = stop.cancelled() => {
                                    break;
                            }
                            result = socket.recv_from(&mut rx_buffer) => {
                                match result {
                                    Ok((0, _)) => {
                                        log::warn!("auto_interface: connection closed");
                                        stop.cancel();
                                        break;
                                    }
                                    Ok((n, peer_addr)) => {
                                        if let Ok(packet) = Packet::deserialize(&mut InputBuffer::new(&rx_buffer[..n])) {
                                            if PACKET_TRACE {
                                                log::trace!("auto_interface: rx << ({}) from {} {}", iface_address, peer_addr, packet);
                                            }
                                            let _ = rx_channel.send(RxMessage { address: iface_address, packet }).await;
                                        } else {
                                            log::warn!("auto_interface: couldn't decode packet from {}", peer_addr);
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("auto_interface: connection error {}", e);
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
                let socket = write_socket;
                let multicast_dest = SocketAddr::new(IpAddr::V4(multicast_addr), data_port);

                tokio::spawn(async move {
                    loop {
                        if stop.is_cancelled() {
                            break;
                        }

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
                                    log::trace!("auto_interface: tx >> ({}) to {} {}", iface_address, multicast_dest, packet);
                                }
                                let mut output = OutputBuffer::new(&mut tx_buffer);
                                if packet.serialize(&mut output).is_ok() {
                                    let _ = socket.send_to(output.as_slice(), multicast_dest).await;
                                }
                            }
                        };
                    }
                })
            };

            tx_task.await.unwrap();
            rx_task.await.unwrap();

            log::info!("auto_interface: closed");
        }
    }
}

impl Interface for AutoInterface {
    fn mtu() -> usize {
        1280 // Conservative MTU for multicast
    }
}
