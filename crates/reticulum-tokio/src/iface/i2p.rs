use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::iface::RxMessage;
use reticulum_core::buffer::{InputBuffer, OutputBuffer};
use reticulum_core::error::RnsError;
use reticulum_core::packet::Packet;
use reticulum_core::serde::Serialize;

use super::hdlc::Hdlc;
use super::{Interface, InterfaceContext};

// TODO: Configure via features
const PACKET_TRACE: bool = false;

pub struct I2PInterface {
    sam_host: String,
    sam_port: u16,
    destination: Option<String>,
}

impl I2PInterface {
    pub fn new(sam_host: String, sam_port: u16, destination: Option<String>) -> Self {
        Self {
            sam_host,
            sam_port,
            destination,
        }
    }

    async fn sam_hello(stream: &mut TcpStream) -> Result<(), RnsError> {
        // Send HELLO command to SAM bridge
        stream
            .write_all(b"HELLO VERSION MIN=3.1 MAX=3.1\n")
            .await
            .map_err(|_| RnsError::ConnectionError)?;
        stream.flush().await.map_err(|_| RnsError::ConnectionError)?;

        // Read response
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .await
            .map_err(|_| RnsError::ConnectionError)?;

        if response.starts_with("HELLO REPLY RESULT=OK") {
            Ok(())
        } else {
            log::error!("i2p_interface: SAM HELLO failed: {}", response.trim());
            Err(RnsError::ConnectionError)
        }
    }

    async fn sam_session_create(
        stream: &mut TcpStream,
        session_id: &str,
        destination: Option<&str>,
    ) -> Result<String, RnsError> {
        // Create session command
        let cmd = if let Some(dest) = destination {
            format!(
                "SESSION CREATE STYLE=STREAM ID={} DESTINATION={}\n",
                session_id, dest
            )
        } else {
            format!(
                "SESSION CREATE STYLE=STREAM ID={} DESTINATION=TRANSIENT\n",
                session_id
            )
        };

        stream
            .write_all(cmd.as_bytes())
            .await
            .map_err(|_| RnsError::ConnectionError)?;
        stream.flush().await.map_err(|_| RnsError::ConnectionError)?;

        // Read response
        let mut reader = BufReader::new(stream);
        let mut response = String::new();
        reader
            .read_line(&mut response)
            .await
            .map_err(|_| RnsError::ConnectionError)?;

        if response.starts_with("SESSION STATUS RESULT=OK") {
            // Extract destination from response
            if let Some(dest_start) = response.find("DESTINATION=") {
                let dest_str = &response[dest_start + 12..];
                if let Some(dest_end) = dest_str.find(|c: char| c.is_whitespace()) {
                    let destination = dest_str[..dest_end].to_string();
                    log::debug!("i2p_interface: session destination: {}", destination);
                    Ok(destination)
                } else {
                    Err(RnsError::ConnectionError)
                }
            } else {
                Err(RnsError::ConnectionError)
            }
        } else {
            log::error!(
                "i2p_interface: SAM SESSION CREATE failed: {}",
                response.trim()
            );
            Err(RnsError::ConnectionError)
        }
    }

    pub async fn spawn(context: InterfaceContext<Self>) {
        let sam_host = { context.inner.lock().unwrap().sam_host.clone() };
        let sam_port = { context.inner.lock().unwrap().sam_port };
        let destination = { context.inner.lock().unwrap().destination.clone() };
        let iface_address = context.channel.address;

        let (rx_channel, tx_channel) = context.channel.split();
        let tx_channel = Arc::new(tokio::sync::Mutex::new(tx_channel));

        loop {
            if context.cancel.is_cancelled() {
                break;
            }

            let sam_addr = format!("{}:{}", sam_host, sam_port);

            // Connect to SAM bridge
            let stream = TcpStream::connect(&sam_addr).await;

            if stream.is_err() {
                log::warn!(
                    "i2p_interface: couldn't connect to SAM bridge at <{}>",
                    sam_addr
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }

            let mut stream = stream.unwrap();

            // Perform SAM handshake
            if Self::sam_hello(&mut stream).await.is_err() {
                log::warn!("i2p_interface: SAM handshake failed");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }

            // Create session
            let session_id = format!("reticulum_{}", rand::random::<u32>());
            let session_dest =
                Self::sam_session_create(&mut stream, &session_id, destination.as_deref()).await;

            if session_dest.is_err() {
                log::warn!("i2p_interface: SAM session creation failed");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }

            let session_dest = session_dest.unwrap();
            log::info!(
                "i2p_interface: connected to SAM bridge at <{}>, destination: {}",
                sam_addr,
                &session_dest[..16]
            );

            let cancel = context.cancel.clone();
            let stop = CancellationToken::new();

            let (read_stream, write_stream) = stream.into_split();

            const BUFFER_SIZE: usize = core::mem::size_of::<Packet>() * 2;

            // Start receive task
            let rx_task = {
                let cancel = cancel.clone();
                let stop = stop.clone();
                let mut stream = read_stream;
                let rx_channel = rx_channel.clone();

                tokio::spawn(async move {
                    let mut hdlc_rx_buffer = [0u8; BUFFER_SIZE];
                    let mut rx_buffer = [0u8; BUFFER_SIZE + (BUFFER_SIZE / 2)];
                    let mut i2p_buffer = [0u8; BUFFER_SIZE * 4];

                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                    break;
                            }
                            _ = stop.cancelled() => {
                                    break;
                            }
                            result = stream.read(&mut i2p_buffer[..]) => {
                                    match result {
                                        Ok(0) => {
                                            log::warn!("i2p_interface: connection closed");
                                            stop.cancel();
                                            break;
                                        }
                                        Ok(n) => {
                                            // I2P stream may contain several or partial HDLC frames
                                            for i in 0..n {
                                                // Push new byte from the end of buffer
                                                rx_buffer[BUFFER_SIZE-1] = i2p_buffer[i];

                                                // Check if it contains a HDLC frame
                                                let frame = Hdlc::find(&rx_buffer[..]);
                                                if let Some(frame) = frame {
                                                    // Decode HDLC frame and deserialize packet
                                                    let frame_buffer = &mut rx_buffer[frame.0..frame.1+1];
                                                    let mut output = OutputBuffer::new(&mut hdlc_rx_buffer[..]);
                                                    if Hdlc::decode(frame_buffer, &mut output).is_ok() {
                                                        if let Ok(packet) = Packet::deserialize(&mut InputBuffer::new(output.as_slice())) {
                                                            if PACKET_TRACE {
                                                                log::trace!("i2p_interface: rx << ({}) {}", iface_address, packet);
                                                            }
                                                            let _ = rx_channel.send(RxMessage { address: iface_address, packet }).await;
                                                        } else {
                                                            log::warn!("i2p_interface: couldn't decode packet");
                                                        }
                                                    } else {
                                                        log::warn!("i2p_interface: couldn't decode hdlc frame");
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
                                            log::warn!("i2p_interface: connection error {}", e);
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
                let mut stream = write_stream;

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
                                    log::trace!("i2p_interface: tx >> ({}) {}", iface_address, packet);
                                }
                                let mut output = OutputBuffer::new(&mut tx_buffer);
                                if packet.serialize(&mut output).is_ok() {
                                    let mut hdlc_output = OutputBuffer::new(&mut hdlc_tx_buffer[..]);

                                    if Hdlc::encode(output.as_slice(), &mut hdlc_output).is_ok() {
                                        let _ = stream.write_all(hdlc_output.as_slice()).await;
                                        let _ = stream.flush().await;
                                    }
                                }
                            }
                        };
                    }
                })
            };

            tx_task.await.unwrap();
            rx_task.await.unwrap();

            log::info!("i2p_interface: disconnected from SAM bridge");
        }
    }
}

impl Interface for I2PInterface {
    fn mtu() -> usize {
        1280 // Conservative MTU for I2P
    }
}
