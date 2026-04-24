//! Targeted BL test: drives P1.10 LOW (active-low) as backlight enable.
//!
//! Hypothesis from hardware observation: the non-reset user button is on P1.10
//! and pressing it (pulling P1.10 to GND) enables the TFT backlight — meaning
//! P1.10 is the actual BL pin for this board revision, not P0.15.
//!
//! LED sequence (P1.03, active-low):
//!   1 pulse  → starting init
//!   2 pulses → init done, filling RED
//!   3 pulses → filling GREEN
//!   4 pulses → filling BLUE
//!   then loops: fast 8 blinks, repeat

#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(target_arch = "arm", no_main)]

#[cfg(target_arch = "arm")]
use defmt_rtt as _;
#[cfg(target_arch = "arm")]
use panic_halt as _;

#[cfg(target_arch = "arm")]
use embassy_nrf::gpio::{Level, Output, OutputDrive};
#[cfg(target_arch = "arm")]
use embassy_nrf::spim::{self, Spim};
#[cfg(target_arch = "arm")]
use embassy_nrf::{bind_interrupts, peripherals};
#[cfg(target_arch = "arm")]
use embassy_time::{Duration, Timer};

#[cfg(target_arch = "arm")]
#[used]
#[link_section = ".rodata"]
static MARKER: [u8; 36] = *b"T114_BL_P110_TEST_MARKER_2026_04_18\0";

#[cfg(target_arch = "arm")]
bind_interrupts!(struct Irqs {
    TWISPI1 => spim::InterruptHandler<peripherals::TWISPI1>;
});

#[cfg(target_arch = "arm")]
fn tft_cmd(spi: &mut Spim<'_>, cs: &mut Output<'_>, dc: &mut Output<'_>, cmd: u8) {
    cs.set_low();
    dc.set_low();
    let _ = spi.blocking_write_from_ram(&[cmd]);
    cs.set_high();
}

#[cfg(target_arch = "arm")]
fn tft_cmd_data(
    spi: &mut Spim<'_>,
    cs: &mut Output<'_>,
    dc: &mut Output<'_>,
    cmd: u8,
    data: &[u8],
) {
    let mut buf = [0u8; 64];
    cs.set_low();
    dc.set_low();
    let _ = spi.blocking_write_from_ram(&[cmd]);
    dc.set_high();
    let mut off = 0;
    while off < data.len() {
        let n = (data.len() - off).min(64);
        buf[..n].copy_from_slice(&data[off..off + n]);
        let _ = spi.blocking_write_from_ram(&buf[..n]);
        off += n;
    }
    cs.set_high();
}

#[cfg(target_arch = "arm")]
fn tft_fill(
    spi: &mut Spim<'_>,
    cs: &mut Output<'_>,
    dc: &mut Output<'_>,
    hi: u8,
    lo: u8,
    w: u16,
    h: u16,
    ox: u16,
    oy: u16,
) {
    let xe = ox + w - 1;
    let ye = oy + h - 1;
    tft_cmd_data(
        spi,
        cs,
        dc,
        0x2A,
        &[(ox >> 8) as u8, ox as u8, (xe >> 8) as u8, xe as u8],
    );
    tft_cmd_data(
        spi,
        cs,
        dc,
        0x2B,
        &[(oy >> 8) as u8, oy as u8, (ye >> 8) as u8, ye as u8],
    );
    tft_cmd(spi, cs, dc, 0x2C);

    let mut chunk = [0u8; 128];
    for i in (0..128).step_by(2) {
        chunk[i] = hi;
        chunk[i + 1] = lo;
    }
    cs.set_low();
    dc.set_high();
    let mut left = w as usize * h as usize;
    while left > 0 {
        let px = left.min(64);
        let _ = spi.blocking_write_from_ram(&chunk[..px * 2]);
        left -= px;
    }
    cs.set_high();
}

#[cfg(target_arch = "arm")]
async fn pulse(led: &mut Output<'_>, n: u8) {
    for _ in 0..n {
        led.set_low();
        Timer::after(Duration::from_millis(250)).await;
        led.set_high();
        Timer::after(Duration::from_millis(250)).await;
    }
    Timer::after(Duration::from_millis(800)).await;
}

#[cfg(target_arch = "arm")]
#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    let p = embassy_nrf::init(Default::default());

    let mut led = Output::new(p.P1_03, Level::High, OutputDrive::Standard);

    let mut spi_cfg = spim::Config::default();
    spi_cfg.frequency = spim::Frequency::M8;
    let mut spi = Spim::new_txonly(p.TWISPI1, Irqs, p.P1_08, p.P1_09, spi_cfg);

    let mut cs = Output::new(p.P0_11, Level::High, OutputDrive::Standard);
    let mut dc = Output::new(p.P0_12, Level::High, OutputDrive::Standard);
    let mut rst = Output::new(p.P0_02, Level::High, OutputDrive::Standard);
    let mut vtft = Output::new(p.P0_03, Level::High, OutputDrive::Standard); // active-low
    let mut vext = Output::new(p.P0_21, Level::Low, OutputDrive::Standard); // active-low, now enabled
    let _adc_en = Output::new(p.P0_06, Level::High, OutputDrive::Standard);

    // BL: P1.10, active-LOW (hypothesis: button pin = backlight enable)
    let mut bl = Output::new(p.P1_10, Level::High, OutputDrive::Standard); // HIGH = off initially

    // Keep P0.15 low (inactive for active-high BL, so it doesn't interfere)
    let _p015 = Output::new(p.P0_15, Level::Low, OutputDrive::Standard);

    loop {
        // Power everything off, BL off
        vtft.set_high();
        vext.set_low(); // VEXT always enabled (active-low)
        bl.set_high(); // BL off
        rst.set_high();
        Timer::after(Duration::from_millis(20)).await;

        // 1 pulse = starting init
        pulse(&mut led, 1).await;

        // Power on VTFT, reset display
        vtft.set_low(); // VTFT on (active-low)
        Timer::after(Duration::from_millis(30)).await;
        rst.set_low();
        Timer::after(Duration::from_millis(20)).await;
        rst.set_high();
        Timer::after(Duration::from_millis(150)).await;

        // ST7789 init sequence
        tft_cmd(&mut spi, &mut cs, &mut dc, 0x01); // SWRESET
        Timer::after(Duration::from_millis(150)).await;
        tft_cmd(&mut spi, &mut cs, &mut dc, 0x11); // SLPOUT
        Timer::after(Duration::from_millis(120)).await;
        tft_cmd_data(&mut spi, &mut cs, &mut dc, 0x3A, &[0x55]); // COLMOD RGB565
        tft_cmd_data(&mut spi, &mut cs, &mut dc, 0x36, &[0x00]); // MADCTL
        tft_cmd(&mut spi, &mut cs, &mut dc, 0x21); // INVON
        tft_cmd(&mut spi, &mut cs, &mut dc, 0x13); // NORON
        tft_cmd(&mut spi, &mut cs, &mut dc, 0x29); // DISPON
        Timer::after(Duration::from_millis(50)).await;

        // Enable BL: P1.10 LOW = on (active-low hypothesis)
        bl.set_low();

        // 2 pulses = RED fill
        pulse(&mut led, 2).await;
        tft_fill(&mut spi, &mut cs, &mut dc, 0xF8, 0x00, 240, 135, 0, 0);
        Timer::after(Duration::from_millis(3000)).await;

        // 3 pulses = GREEN fill
        pulse(&mut led, 3).await;
        tft_fill(&mut spi, &mut cs, &mut dc, 0x07, 0xE0, 240, 135, 0, 0);
        Timer::after(Duration::from_millis(3000)).await;

        // 4 pulses = BLUE fill
        pulse(&mut led, 4).await;
        tft_fill(&mut spi, &mut cs, &mut dc, 0x00, 0x1F, 240, 135, 0, 0);
        Timer::after(Duration::from_millis(3000)).await;

        // End-of-cycle: 8 rapid blinks
        bl.set_high();
        for _ in 0..8 {
            led.set_low();
            Timer::after(Duration::from_millis(100)).await;
            led.set_high();
            Timer::after(Duration::from_millis(100)).await;
        }
        Timer::after(Duration::from_millis(2000)).await;
    }
}

#[cfg(not(target_arch = "arm"))]
fn main() {
    eprintln!("bl_p110_test: build with --target thumbv7em-none-eabihf");
}
