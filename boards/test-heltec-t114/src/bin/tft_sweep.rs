#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(target_arch = "arm", no_main)]

use defmt_rtt as _;

use panic_halt as _;

extern crate alloc;

use core::mem::MaybeUninit;

use embassy_nrf::gpio::{Level, Output, OutputDrive};

use embassy_nrf::spim::{self, Spim};

use embassy_nrf::{bind_interrupts, peripherals};

use embassy_time::{Duration, Timer};

use embedded_alloc::LlffHeap as Heap;

use mousefood::embedded_graphics::draw_target::DrawTarget;

use mousefood::embedded_graphics::geometry::{OriginDimensions, Size};

use mousefood::embedded_graphics::pixelcolor::Rgb565;

use mousefood::embedded_graphics::prelude::{Dimensions, IntoStorage, Pixel, RgbColor};

use mousefood::embedded_graphics::primitives::Rectangle;

#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 4096;

static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

#[used]
#[link_section = ".rodata"]
static SWEEP_MARKER: [u8; 35] = *b"T114_TFT_SWEEP_MARKER_2026_04_18_A\0";

#[derive(Clone, Copy)]
struct DisplayError;

impl From<spim::Error> for DisplayError {
    fn from(_: spim::Error) -> Self {
        Self
    }
}

#[derive(Clone, Copy)]
struct SweepProfile {
    vdd_active_low: bool,
    bl_active_low: bool,
    rst_active_low: bool,
    madctl: u8,
    w: u16,
    h: u16,
    ox: u16,
    oy: u16,
}

struct Tft<'d> {
    spi: Spim<'d>,
    cs: Output<'d>,
    dc: Output<'d>,
    rst: Output<'d>,
    vdd: Output<'d>,
    bl: Output<'d>,
    w: u16,
    h: u16,
    ox: u16,
    oy: u16,
}

impl<'d> Tft<'d> {
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
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        }
    }

    async fn init_with(&mut self, p: SweepProfile) -> Result<(), DisplayError> {
        self.w = p.w;
        self.h = p.h;
        self.ox = p.ox;
        self.oy = p.oy;

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
        Timer::after(Duration::from_millis(20)).await;

        self.vdd.set_level(vdd_on);
        Timer::after(Duration::from_millis(30)).await;
        self.rst.set_level(rst_active);
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_level(rst_inactive);
        Timer::after(Duration::from_millis(120)).await;

        self.write_cmd(0x01)?; // SWRESET
        Timer::after(Duration::from_millis(120)).await;
        self.write_cmd(0x11)?; // SLPOUT
        Timer::after(Duration::from_millis(120)).await;
        self.write_cmd_data(0x3A, &[0x55])?; // RGB565
        self.write_cmd_data(0x36, &[p.madctl])?;
        self.write_cmd(0x21)?; // INVON
        self.write_cmd(0x29)?; // DISPON
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

    fn write_cmd_data(&mut self, cmd: u8, data: &[u8]) -> Result<(), DisplayError> {
        let cmd_buf = [cmd];
        let mut chunk = [0u8; 64];
        self.cs.set_low();
        self.dc.set_low();
        self.spi.blocking_write_from_ram(&cmd_buf)?;
        self.dc.set_high();
        let mut off = 0;
        while off < data.len() {
            let n = (data.len() - off).min(chunk.len());
            chunk[..n].copy_from_slice(&data[off..off + n]);
            self.spi.blocking_write_from_ram(&chunk[..n])?;
            off += n;
        }
        self.cs.set_high();
        Ok(())
    }

    fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) -> Result<(), DisplayError> {
        let xs = x0 + self.ox;
        let xe = x1 + self.ox;
        let ys = y0 + self.oy;
        let ye = y1 + self.oy;
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

    fn fill_rgb565(&mut self, c: Rgb565) -> Result<(), DisplayError> {
        let area = Rectangle::new(
            mousefood::embedded_graphics::geometry::Point::new(0, 0),
            Size::new(self.w as u32, self.h as u32),
        );
        self.fill_solid(&area, c)
    }
}

impl OriginDimensions for Tft<'_> {
    fn size(&self) -> Size {
        Size::new(self.w as u32, self.h as u32)
    }
}

impl DrawTarget for Tft<'_> {
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
            if x >= self.w || y >= self.h {
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
        let mut left = clipped.size.width as usize * clipped.size.height as usize;
        while left > 0 {
            let px = left.min(chunk.len() / 2);
            self.spi.blocking_write_from_ram(&chunk[..px * 2])?;
            left -= px;
        }
        self.cs.set_high();
        Ok(())
    }
}

bind_interrupts!(struct Irqs {
    TWISPI1 => spim::InterruptHandler<peripherals::TWISPI1>;
});

async fn pulse(led: &mut Output<'_>, n: u8) {
    // Observed visible LED is active-low.
    for _ in 0..n {
        led.set_low();
        Timer::after(Duration::from_millis(180)).await;
        led.set_high();
        Timer::after(Duration::from_millis(180)).await;
    }
    Timer::after(Duration::from_millis(700)).await;
}

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    unsafe {
        HEAP.init(
            core::ptr::addr_of_mut!(HEAP_MEM) as *mut u8 as usize,
            HEAP_SIZE,
        );
    }
    let p = embassy_nrf::init(Default::default());

    let mut led = Output::new(p.P1_03, Level::High, OutputDrive::Standard);
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
    let bl = Output::new(p.P0_15, Level::High, OutputDrive::Standard);
    let _vext = Output::new(p.P0_21, Level::High, OutputDrive::Standard);
    let _adc_en = Output::new(p.P0_06, Level::High, OutputDrive::Standard);

    let mut tft = Tft::new(spi, cs, dc, rst, vdd, bl);

    let profiles = [
        SweepProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x00,
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        },
        SweepProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x60,
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        },
        SweepProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x00,
            w: 135,
            h: 240,
            ox: 40,
            oy: 53,
        },
        SweepProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x60,
            w: 135,
            h: 240,
            ox: 40,
            oy: 53,
        },
        SweepProfile {
            vdd_active_low: false,
            bl_active_low: true,
            rst_active_low: true,
            madctl: 0x00,
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        },
        SweepProfile {
            vdd_active_low: true,
            bl_active_low: false,
            rst_active_low: true,
            madctl: 0x00,
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        },
        SweepProfile {
            vdd_active_low: true,
            bl_active_low: true,
            rst_active_low: false,
            madctl: 0x00,
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        },
        SweepProfile {
            vdd_active_low: false,
            bl_active_low: false,
            rst_active_low: false,
            madctl: 0x60,
            w: 240,
            h: 135,
            ox: 0,
            oy: 0,
        },
    ];

    loop {
        for (i, profile) in profiles.iter().enumerate() {
            pulse(&mut led, (i as u8) + 1).await;

            if tft.init_with(*profile).await.is_ok() {
                let _ = tft.fill_rgb565(Rgb565::RED);
                Timer::after(Duration::from_millis(500)).await;
                let _ = tft.fill_rgb565(Rgb565::GREEN);
                Timer::after(Duration::from_millis(500)).await;
                let _ = tft.fill_rgb565(Rgb565::BLUE);
                Timer::after(Duration::from_millis(500)).await;
            }

            Timer::after(Duration::from_millis(500)).await;
        }
        pulse(&mut led, 8).await;
    }
}

#[cfg(not(target_arch = "arm"))]
fn main() {
    eprintln!("tft_sweep is an embedded target; build with --target thumbv7em-none-eabihf");
}
