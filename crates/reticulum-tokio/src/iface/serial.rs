use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::SerialPortBuilderExt;

use reticulum_core::buffer::OutputBuffer;

use super::hdlc::Hdlc;
use crate::iface::TokioInterface;

const PACKET_TRACE: bool = false;
const RX_WINDOW: usize = 4096;

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
    port: tokio_serial::SerialStream,
    rx_window: Vec<u8>,
    rx_pos: usize,
}

impl SerialInterface {
    /// Open the serial port described by the given parameters.
    pub async fn open(
        port: String,
        baud_rate: u32,
        data_bits: u8,
        parity: Option<String>,
        stop_bits: u8,
    ) -> Result<Self, tokio_serial::Error> {
        let data_bits = match data_bits {
            5 => tokio_serial::DataBits::Five,
            6 => tokio_serial::DataBits::Six,
            7 => tokio_serial::DataBits::Seven,
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

        let config = SerialConfig {
            port: port.clone(),
            baud_rate,
            data_bits,
            parity,
            stop_bits,
        };

        let stream = tokio_serial::new(&config.port, config.baud_rate)
            .data_bits(config.data_bits)
            .parity(config.parity)
            .stop_bits(config.stop_bits)
            .open_native_async()?;

        log::info!(
            "serial_interface: opened '{}' at {} baud",
            config.port,
            config.baud_rate
        );

        Ok(Self {
            config,
            port: stream,
            rx_window: vec![0u8; RX_WINDOW],
            rx_pos: 0,
        })
    }
}

impl TokioInterface for SerialInterface {
    type Error = std::io::Error;

    async fn transmit(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        let mut hdlc_buf = vec![0u8; frame.len() * 2 + 4];
        let mut output = OutputBuffer::new(&mut hdlc_buf);
        Hdlc::encode(frame, &mut output).map_err(|_| std::io::Error::other("HDLC encode error"))?;
        if PACKET_TRACE {
            log::trace!(
                "serial_interface '{}': tx >> {} bytes",
                self.config.port,
                output.offset()
            );
        }
        self.port.write_all(output.as_slice()).await?;
        self.port.flush().await
    }

    async fn receive(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let window_cap = self.rx_window.len();
        loop {
            if let Some((start, end)) = Hdlc::find(&self.rx_window[..self.rx_pos]) {
                let frame_end = end + 1;
                let frame_bytes: Vec<u8> = self.rx_window[start..frame_end].to_vec();

                self.rx_window.copy_within(frame_end..self.rx_pos, 0);
                self.rx_pos -= frame_end;

                let mut decode_buf = vec![0u8; RX_WINDOW];
                let mut out = OutputBuffer::new(&mut decode_buf);
                if Hdlc::decode(&frame_bytes, &mut out).is_ok() {
                    let decoded = out.as_slice();
                    if PACKET_TRACE {
                        log::trace!(
                            "serial_interface '{}': rx << {} bytes",
                            self.config.port,
                            decoded.len()
                        );
                    }
                    let n = decoded.len().min(buf.len());
                    buf[..n].copy_from_slice(&decoded[..n]);
                    return Ok(n);
                }
                log::debug!(
                    "serial_interface '{}': discarded bad HDLC frame",
                    self.config.port
                );
                continue;
            }

            if self.rx_pos >= window_cap {
                let keep_from = window_cap / 2;
                self.rx_window.copy_within(keep_from..self.rx_pos, 0);
                self.rx_pos -= keep_from;
            }

            let n = self.port.read(&mut self.rx_window[self.rx_pos..]).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("serial_interface '{}': port closed", self.config.port),
                ));
            }
            self.rx_pos += n;
        }
    }

    fn mtu(&self) -> usize {
        508
    }
}
