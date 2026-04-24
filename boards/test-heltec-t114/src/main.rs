//! Heltec T114 USB flash test:
//! - LED heartbeat on GPIO35 (`P1.03`)
//! - ST7789 debug message rendered with mousefood + ratatui
//!
//! Display pinout sourced from the board profile in `boards/heltec-t114/src/ui.rs`:
//! - SCK  -> P1.08
//! - MOSI -> P1.09
//! - CS   -> P0.11
//! - DC   -> P0.12
//! - RST  -> P0.02
//! - VDD  -> P0.03 (VTFT_CTRL, active-low enable)
//! - BL   -> P0.15

#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(target_arch = "arm", no_main)]

#[cfg(target_arch = "arm")]
use defmt_rtt as _;
#[cfg(target_arch = "arm")]
use panic_halt as _;

#[cfg(target_arch = "arm")]
extern crate alloc;

#[cfg(target_arch = "arm")]
use core::mem::MaybeUninit;

#[cfg(target_arch = "arm")]
use embassy_nrf::gpio::{Level, Output, OutputDrive};
#[cfg(target_arch = "arm")]
use embassy_nrf::spim::{self, Spim};
#[cfg(target_arch = "arm")]
use embassy_nrf::{bind_interrupts, peripherals};
#[cfg(target_arch = "arm")]
use embassy_time::{Duration, Timer};
#[cfg(target_arch = "arm")]
use embedded_alloc::LlffHeap as Heap;

#[cfg(target_arch = "arm")]
use mousefood::embedded_graphics::draw_target::DrawTarget;
#[cfg(target_arch = "arm")]
use mousefood::embedded_graphics::geometry::{OriginDimensions, Size};
#[cfg(target_arch = "arm")]
use mousefood::embedded_graphics::pixelcolor::Rgb565;
#[cfg(target_arch = "arm")]
use mousefood::embedded_graphics::prelude::{Dimensions, IntoStorage, Pixel, RgbColor};
#[cfg(target_arch = "arm")]
use mousefood::embedded_graphics::primitives::Rectangle;
#[cfg(target_arch = "arm")]
use mousefood::{EmbeddedBackend, EmbeddedBackendConfig};
#[cfg(target_arch = "arm")]
use ratatui::widgets::{Block, Borders, Paragraph};
#[cfg(target_arch = "arm")]
use ratatui::Terminal;

#[cfg(target_arch = "arm")]
const TFT_WIDTH: u16 = 240;
#[cfg(target_arch = "arm")]
const TFT_HEIGHT: u16 = 135;
#[cfg(target_arch = "arm")]
const TFT_OFFSET_X: u16 = 0;
#[cfg(target_arch = "arm")]
const TFT_OFFSET_Y: u16 = 0;
#[cfg(target_arch = "arm")]
const BUILD_MARKER: &str = "T114_MARKER_2026_04_18_A";
#[cfg(target_arch = "arm")]
#[used]
#[link_section = ".rodata"]
static BUILD_MARKER_BYTES: [u8; 25] = *b"T114_MARKER_2026_04_18_A\0";

#[cfg(target_arch = "arm")]
#[global_allocator]
static HEAP: Heap = Heap::empty();

#[cfg(target_arch = "arm")]
const HEAP_SIZE: usize = 128 * 1024;
#[cfg(target_arch = "arm")]
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

#[cfg(target_arch = "arm")]
#[derive(Debug, Clone, Copy)]
enum DisplayError {
    Spi(spim::Error),
    InitStep(u8, spim::Error),
}

#[cfg(target_arch = "arm")]
impl From<spim::Error> for DisplayError {
    fn from(value: spim::Error) -> Self {
        Self::Spi(value)
    }
}

#[cfg(target_arch = "arm")]
struct T114Display<'d> {
    spi: Spim<'d>,
    cs: Output<'d>,
    dc: Output<'d>,
    rst: Output<'d>,
    vdd: Output<'d>,
    bl: Output<'d>,
}

#[cfg(target_arch = "arm")]
#[derive(Clone, Copy)]
struct PanelProfile {
    vdd_active_low: bool,
    bl_active_low: bool,
    rst_active_low: bool,
    madctl: u8,
}

#[cfg(target_arch = "arm")]
impl<'d> T114Display<'d> {
    fn new(
        spi: Spim<'d>,
        cs: Output<'d>,
        dc: Output<'d>,
        rst: Output<'d>,
        vdd: Output<'d>,
        bl: Output<'d>,
    ) -> Self {
        Self {
            spi,
            cs,
            dc,
            rst,
            vdd,
            bl,
        }
    }

    async fn init_with(&mut self, p: PanelProfile) -> Result<(), DisplayError> {
        let vdd_off = if p.vdd_active_low {
            Level::High
        } else {
            Level::Low
        };
        let vdd_on = if p.vdd_active_low {
            Level::Low
        } else {
            Level::High
        };
        let bl_off = if p.bl_active_low {
            Level::High
        } else {
            Level::Low
        };
        let bl_on = if p.bl_active_low {
            Level::Low
        } else {
            Level::High
        };
        let rst_inactive = if p.rst_active_low {
            Level::High
        } else {
            Level::Low
        };
        let rst_active = if p.rst_active_low {
            Level::Low
        } else {
            Level::High
        };

        self.vdd.set_level(vdd_off);
        self.bl.set_level(bl_off);
        self.cs.set_high();
        self.dc.set_high();
        self.rst.set_level(rst_inactive);

        self.vdd.set_level(vdd_on);
        Timer::after(Duration::from_millis(20)).await;

        self.rst.set_level(rst_active);
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_level(rst_inactive);
        Timer::after(Duration::from_millis(120)).await;

        self.write_cmd(0x01).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(1, se),
            other => other,
        })?; // SWRESET
        Timer::after(Duration::from_millis(150)).await;

        // ST7789 common init sequence (checkpoint 2)
        self.write_cmd_data(0x36, &[p.madctl])
            .map_err(|e| match e {
                DisplayError::Spi(se) => DisplayError::InitStep(2, se),
                other => other,
            })?; // MADCTL
        self.write_cmd_data(0x3A, &[0x55]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?; // COLMOD = RGB565
        self.write_cmd_data(0xB2, &[0x0C, 0x0C, 0x00, 0x33, 0x33])
            .map_err(|e| match e {
                DisplayError::Spi(se) => DisplayError::InitStep(2, se),
                other => other,
            })?;
        self.write_cmd_data(0xB7, &[0x35]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xBB, &[0x19]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xC0, &[0x2C]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xC2, &[0x01]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xC3, &[0x12]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xC4, &[0x20]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xC6, &[0x0F]).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(0xD0, &[0xA4, 0xA1])
            .map_err(|e| match e {
                DisplayError::Spi(se) => DisplayError::InitStep(2, se),
                other => other,
            })?;
        self.write_cmd_data(
            0xE0,
            &[
                0xD0, 0x04, 0x0D, 0x11, 0x13, 0x2B, 0x3F, 0x54, 0x4C, 0x18, 0x0D, 0x0B, 0x1F, 0x23,
            ],
        )
        .map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;
        self.write_cmd_data(
            0xE1,
            &[
                0xD0, 0x04, 0x0C, 0x11, 0x13, 0x2C, 0x3F, 0x44, 0x51, 0x2F, 0x1F, 0x1F, 0x20, 0x23,
            ],
        )
        .map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(2, se),
            other => other,
        })?;

        // checkpoint 3
        self.write_cmd(0x11).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(3, se),
            other => other,
        })?; // SLPOUT
        Timer::after(Duration::from_millis(120)).await;
        self.write_cmd(0x21).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(3, se),
            other => other,
        })?; // INVON
        self.write_cmd(0x13).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(3, se),
            other => other,
        })?; // NORON
        self.write_cmd(0x29).map_err(|e| match e {
            DisplayError::Spi(se) => DisplayError::InitStep(3, se),
            other => other,
        })?; // DISPON

        Timer::after(Duration::from_millis(20)).await;
        self.bl.set_level(bl_on);

        Ok(())
    }

    fn write_cmd(&mut self, cmd: u8) -> Result<(), DisplayError> {
        let buf = [cmd];
        self.cs.set_low();
        self.dc.set_low();
        self.spi.blocking_write_from_ram(&buf)?;
        self.cs.set_high();
        Ok(())
    }

    fn write_data(&mut self, data: &[u8]) -> Result<(), DisplayError> {
        let mut chunk = [0u8; 64];
        self.cs.set_low();
        self.dc.set_high();
        let mut offset = 0;
        while offset < data.len() {
            let n = (data.len() - offset).min(chunk.len());
            chunk[..n].copy_from_slice(&data[offset..offset + n]);
            self.spi.blocking_write_from_ram(&chunk[..n])?;
            offset += n;
        }
        self.cs.set_high();
        Ok(())
    }

    fn write_cmd_data(&mut self, cmd: u8, data: &[u8]) -> Result<(), DisplayError> {
        let cmd_buf = [cmd];
        let mut chunk = [0u8; 64];
        self.cs.set_low();
        self.dc.set_low();
        self.spi.blocking_write_from_ram(&cmd_buf)?;
        self.dc.set_high();
        let mut offset = 0;
        while offset < data.len() {
            let n = (data.len() - offset).min(chunk.len());
            chunk[..n].copy_from_slice(&data[offset..offset + n]);
            self.spi.blocking_write_from_ram(&chunk[..n])?;
            offset += n;
        }
        self.cs.set_high();
        Ok(())
    }

    fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) -> Result<(), DisplayError> {
        let xs = x0 + TFT_OFFSET_X;
        let xe = x1 + TFT_OFFSET_X;
        let ys = y0 + TFT_OFFSET_Y;
        let ye = y1 + TFT_OFFSET_Y;

        self.write_cmd_data(
            0x2A,
            &[
                (xs >> 8) as u8,
                (xs & 0xFF) as u8,
                (xe >> 8) as u8,
                (xe & 0xFF) as u8,
            ],
        )?;
        self.write_cmd_data(
            0x2B,
            &[
                (ys >> 8) as u8,
                (ys & 0xFF) as u8,
                (ye >> 8) as u8,
                (ye & 0xFF) as u8,
            ],
        )?;
        self.write_cmd(0x2C)?;
        Ok(())
    }

    fn write_rgb565_repeat(&mut self, color: Rgb565, count: usize) -> Result<(), DisplayError> {
        let raw = color.into_storage();
        let hi = (raw >> 8) as u8;
        let lo = (raw & 0xFF) as u8;
        let mut chunk = [0u8; 128];
        for i in (0..chunk.len()).step_by(2) {
            chunk[i] = hi;
            chunk[i + 1] = lo;
        }

        self.cs.set_low();
        self.dc.set_high();
        let mut left = count;
        while left > 0 {
            let px = left.min(chunk.len() / 2);
            self.spi.blocking_write_from_ram(&chunk[..px * 2])?;
            left -= px;
        }
        self.cs.set_high();
        Ok(())
    }
}

#[cfg(target_arch = "arm")]
impl OriginDimensions for T114Display<'_> {
    fn size(&self) -> Size {
        Size::new(TFT_WIDTH as u32, TFT_HEIGHT as u32)
    }
}

#[cfg(target_arch = "arm")]
impl DrawTarget for T114Display<'_> {
    type Color = Rgb565;
    type Error = DisplayError;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            if point.x < 0 || point.y < 0 {
                continue;
            }
            let x = point.x as u16;
            let y = point.y as u16;
            if x >= TFT_WIDTH || y >= TFT_HEIGHT {
                continue;
            }

            self.set_window(x, y, x, y)?;
            let raw = color.into_storage();
            let bytes = [(raw >> 8) as u8, (raw & 0xFF) as u8];
            self.cs.set_low();
            self.dc.set_high();
            self.spi.blocking_write_from_ram(&bytes)?;
            self.cs.set_high();
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let clipped = area.intersection(&self.bounding_box());
        if clipped.is_zero_sized() {
            return Ok(());
        }

        let x0 = clipped.top_left.x as u16;
        let y0 = clipped.top_left.y as u16;
        let x1 = (clipped.top_left.x + clipped.size.width as i32 - 1) as u16;
        let y1 = (clipped.top_left.y + clipped.size.height as i32 - 1) as u16;

        self.set_window(x0, y0, x1, y1)?;
        self.write_rgb565_repeat(
            color,
            clipped.size.width as usize * clipped.size.height as usize,
        )
    }
}

#[cfg(target_arch = "arm")]
bind_interrupts!(struct Irqs {
    TWISPI1 => spim::InterruptHandler<peripherals::TWISPI1>;
});

#[cfg(target_arch = "arm")]
async fn pulse(led: &mut Output<'_>, n: u8) {
    for _ in 0..n {
        led.set_high();
        Timer::after(Duration::from_millis(220)).await;
        led.set_low();
        Timer::after(Duration::from_millis(220)).await;
    }
    Timer::after(Duration::from_millis(1200)).await;
}

#[cfg(target_arch = "arm")]
#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    defmt::info!("test-heltec-t114 display + blinky start");
    defmt::info!("{}", BUILD_MARKER);

    unsafe {
        HEAP.init(
            core::ptr::addr_of_mut!(HEAP_MEM) as *mut u8 as usize,
            HEAP_SIZE,
        );
    }

    let p = embassy_nrf::init(Default::default());
    let mut led = Output::new(p.P1_03, Level::Low, OutputDrive::Standard);

    // Checkpoint A: reached main after embassy init.
    pulse(&mut led, 1).await;

    let mut spi_cfg = spim::Config::default();
    spi_cfg.frequency = spim::Frequency::M8;

    let spi = Spim::new_txonly(
        p.TWISPI1, Irqs, p.P1_08, // SCK
        p.P1_09, // MOSI
        spi_cfg,
    );

    let cs = Output::new(p.P0_11, Level::High, OutputDrive::Standard);
    let dc = Output::new(p.P0_12, Level::Low, OutputDrive::Standard);
    let rst = Output::new(p.P0_02, Level::High, OutputDrive::Standard);
    let vdd = Output::new(p.P0_03, Level::High, OutputDrive::Standard);
    let mut bl = Output::new(p.P0_15, Level::High, OutputDrive::Standard);
    let _vext = Output::new(p.P0_21, Level::High, OutputDrive::Standard);
    let _adc_en = Output::new(p.P0_06, Level::High, OutputDrive::Standard);

    // Backlight probe (active-low): clearly force OFF then ON so we can verify
    // whether P0.15 is actually wired to the TFT backlight on this board.
    bl.set_high();
    Timer::after(Duration::from_millis(1500)).await;
    bl.set_low();
    Timer::after(Duration::from_millis(1500)).await;
    bl.set_high();
    Timer::after(Duration::from_millis(1500)).await;
    bl.set_low();
    Timer::after(Duration::from_millis(1500)).await;

    // Checkpoint B: GPIO + SPI configured.
    pulse(&mut led, 2).await;

    let mut display = T114Display::new(spi, cs, dc, rst, vdd, bl);

    let profiles = [
        PanelProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x00,
        },
        PanelProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x60,
        },
        PanelProfile {
            vdd_active_low: false,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x00,
        },
        PanelProfile {
            vdd_active_low: false,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x60,
        },
        PanelProfile {
            vdd_active_low: true,
            bl_active_low: false,
            rst_active_low: true,
            madctl: 0x00,
        },
        PanelProfile {
            vdd_active_low: true,
            bl_active_low: false,
            rst_active_low: true,
            madctl: 0x60,
        },
        PanelProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: false,
            madctl: 0x00,
        },
        PanelProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: false,
            madctl: 0x60,
        },
    ];

    let mut init_ok = false;
    for (idx, profile) in profiles.iter().enumerate() {
        // Show profile index attempt as (idx+1) pulses.
        pulse(&mut led, (idx as u8) + 1).await;

        if display.init_with(*profile).await.is_ok() {
            init_ok = true;
            break;
        }
    }

    if !init_ok {
        loop {
            // All profiles failed.
            pulse(&mut led, 6).await;
        }
    }

    // Raw panel sanity check before mousefood/ratatui.
    if display
        .fill_solid(&display.bounding_box(), Rgb565::RED)
        .is_err()
    {
        // Error code 2: first fill failed.
        loop {
            for _ in 0..2 {
                led.set_high();
                Timer::after(Duration::from_millis(220)).await;
                led.set_low();
                Timer::after(Duration::from_millis(220)).await;
            }
            Timer::after(Duration::from_millis(1000)).await;
        }
    }
    Timer::after(Duration::from_millis(300)).await;
    let _ = display.fill_solid(&display.bounding_box(), Rgb565::GREEN);
    Timer::after(Duration::from_millis(300)).await;
    let _ = display.fill_solid(&display.bounding_box(), Rgb565::BLUE);
    Timer::after(Duration::from_millis(300)).await;

    let backend = EmbeddedBackend::new(&mut display, EmbeddedBackendConfig::default());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => {
            // Error code 3: mousefood terminal init failed.
            loop {
                for _ in 0..3 {
                    led.set_high();
                    Timer::after(Duration::from_millis(180)).await;
                    led.set_low();
                    Timer::after(Duration::from_millis(180)).await;
                }
                Timer::after(Duration::from_millis(1000)).await;
            }
        }
    };

    // Checkpoint D: terminal init succeeded.
    pulse(&mut led, 4).await;

    let mut seconds: u32 = 0;
    loop {
        seconds = seconds.wrapping_add(1);

        let draw_ok = terminal
            .draw(|frame| {
                let text = alloc::format!(
                    "USB UF2 firmware loaded\nmousefood + ratatui OK\nuptime: {}s",
                    seconds
                );
                let paragraph = Paragraph::new(text).block(
                    Block::default()
                        .title(" Heltec T114 Debug ")
                        .borders(Borders::ALL),
                );
                frame.render_widget(paragraph, frame.area());
            })
            .is_ok();

        if !draw_ok {
            // Error code 4: ratatui draw/flush failed.
            loop {
                for _ in 0..4 {
                    led.set_high();
                    Timer::after(Duration::from_millis(140)).await;
                    led.set_low();
                    Timer::after(Duration::from_millis(140)).await;
                }
                Timer::after(Duration::from_millis(1000)).await;
            }
        }

        // Runtime signature: two quick blinks then a pause.
        for _ in 0..2 {
            led.set_high();
            Timer::after(Duration::from_millis(120)).await;
            led.set_low();
            Timer::after(Duration::from_millis(120)).await;
        }
        Timer::after(Duration::from_millis(1100)).await;
    }
}

#[cfg(not(target_arch = "arm"))]
fn main() {
    eprintln!("test-heltec-t114 is an embedded target; build with --target thumbv7em-none-eabihf");
}
