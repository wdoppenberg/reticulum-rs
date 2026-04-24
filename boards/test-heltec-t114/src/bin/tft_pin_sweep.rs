//! TFT pin sweep: tests candidate BL, VDD/VEXT, and orientation combinations.
//!
//! Key changes from tft_sweep:
//! - VEXT (P0.21) now enabled LOW — was HIGH (disabled!) in all prior tests
//! - BL tested as active-HIGH (Meshtastic reference) and active-LOW
//! - Portrait offsets corrected to ox=52, oy=40 (production profile)
//!
//! LED coding (active-low, P1.03):
//!   Phase separator = 10 rapid blinks + 2s gap
//!   Case N          = N slow pulses + 0.9s gap → 3s RED / 1.5s GREEN / 1.5s BLUE
//!
//! Phases:
//!   Phase 1 (cases 1-6): BL pin/polarity sweep, VTFT active-low, VEXT enabled
//!   Phase 2 (cases 1-4): VDD/VEXT combination sweep, BL=P0.15 active-high
//!   Phase 3 (cases 1-4): Portrait orientation sweep (135x240)
//!
//! Record which case (phase + pulse count) first shows display color.

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
static MARKER: [u8; 40] = *b"T114_TFT_PIN_SWEEP_MARKER_2026_04_18_B\0\0";

#[cfg(target_arch = "arm")]
bind_interrupts!(struct Irqs {
    TWISPI1 => spim::InterruptHandler<peripherals::TWISPI1>;
});

// ── SPI helpers ──────────────────────────────────────────────────────────────

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

// ── Display init ─────────────────────────────────────────────────────────────

/// Power-cycle and init ST7789. BL control is left to the caller.
///
/// `vtft_on`  = level to apply to P0.03 to ENABLE display power (Low = active-low)
/// `vext_on`  = level to apply to P0.21 to ENABLE ext power rail (Low = active-low)
#[cfg(target_arch = "arm")]
async fn tft_init(
    spi: &mut Spim<'_>,
    cs: &mut Output<'_>,
    dc: &mut Output<'_>,
    rst: &mut Output<'_>,
    vtft: &mut Output<'_>,
    vext: &mut Output<'_>,
    madctl: u8,
    vtft_on: Level,
    vext_on: Level,
) {
    let vtft_off = if vtft_on == Level::Low {
        Level::High
    } else {
        Level::Low
    };
    let vext_off = if vext_on == Level::Low {
        Level::High
    } else {
        Level::Low
    };

    cs.set_high();
    dc.set_high();
    vtft.set_level(vtft_off);
    vext.set_level(vext_off);
    rst.set_high(); // RST idle (active-low)
    Timer::after(Duration::from_millis(10)).await;

    vext.set_level(vext_on);
    vtft.set_level(vtft_on);
    Timer::after(Duration::from_millis(30)).await;

    rst.set_low(); // assert RST
    Timer::after(Duration::from_millis(20)).await;
    rst.set_high(); // deassert RST
    Timer::after(Duration::from_millis(150)).await;

    tft_cmd(spi, cs, dc, 0x01); // SWRESET
    Timer::after(Duration::from_millis(150)).await;
    tft_cmd(spi, cs, dc, 0x11); // SLPOUT
    Timer::after(Duration::from_millis(120)).await;
    tft_cmd_data(spi, cs, dc, 0x3A, &[0x55]); // COLMOD RGB565
    tft_cmd_data(spi, cs, dc, 0x36, &[madctl]); // MADCTL
    tft_cmd(spi, cs, dc, 0x21); // INVON
    tft_cmd(spi, cs, dc, 0x13); // NORON
    tft_cmd(spi, cs, dc, 0x29); // DISPON
    Timer::after(Duration::from_millis(50)).await;
}

// ── LED / timing helpers ──────────────────────────────────────────────────────

#[cfg(target_arch = "arm")]
async fn pulse(led: &mut Output<'_>, n: u8) {
    for _ in 0..n {
        led.set_low(); // active-low: low = ON
        Timer::after(Duration::from_millis(200)).await;
        led.set_high();
        Timer::after(Duration::from_millis(200)).await;
    }
    Timer::after(Duration::from_millis(900)).await;
}

#[cfg(target_arch = "arm")]
async fn phase_sep(led: &mut Output<'_>) {
    for _ in 0..10 {
        led.set_low();
        Timer::after(Duration::from_millis(70)).await;
        led.set_high();
        Timer::after(Duration::from_millis(70)).await;
    }
    Timer::after(Duration::from_millis(2000)).await;
}

#[cfg(target_arch = "arm")]
async fn show_colors(
    spi: &mut Spim<'_>,
    cs: &mut Output<'_>,
    dc: &mut Output<'_>,
    w: u16,
    h: u16,
    ox: u16,
    oy: u16,
) {
    tft_fill(spi, cs, dc, 0xF8, 0x00, w, h, ox, oy); // RED
    Timer::after(Duration::from_millis(3000)).await;
    tft_fill(spi, cs, dc, 0x07, 0xE0, w, h, ox, oy); // GREEN
    Timer::after(Duration::from_millis(1500)).await;
    tft_fill(spi, cs, dc, 0x00, 0x1F, w, h, ox, oy); // BLUE
    Timer::after(Duration::from_millis(1500)).await;
}

// ── Main ──────────────────────────────────────────────────────────────────────

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

    // VTFT: display power enable, active-low (P0.03 LOW = on)
    let mut vtft = Output::new(p.P0_03, Level::High, OutputDrive::Standard);

    // VEXT: external power rail enable, active-low (P0.21 LOW = on)
    // PRIOR BUG: all previous test firmware set this HIGH = disabled!
    let mut vext = Output::new(p.P0_21, Level::Low, OutputDrive::Standard);

    // ADC_EN — keep high, same as previous tests
    let _adc_en = Output::new(p.P0_06, Level::High, OutputDrive::Standard);

    // BL candidates (LOW = off for active-high BL, most common)
    let mut bl_p015 = Output::new(p.P0_15, Level::Low, OutputDrive::Standard);
    let mut bl_p014 = Output::new(p.P0_14, Level::Low, OutputDrive::Standard);
    let mut bl_p013 = Output::new(p.P0_13, Level::Low, OutputDrive::Standard);
    let mut bl_p004 = Output::new(p.P0_04, Level::Low, OutputDrive::Standard);

    loop {
        // =================================================================
        // PHASE 1: BL pin / polarity sweep
        // Fixed: VTFT active-low (P0.03=LOW=on), VEXT=LOW (enabled)
        //        RST active-low, landscape 240x135, MADCTL=0x00
        // =================================================================
        phase_sep(&mut led).await;

        // Case 1: P0.15 active-HIGH (Meshtastic documented polarity)
        pulse(&mut led, 1).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_high(); // BL ON (active-high)
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 2: P0.15 active-LOW
        pulse(&mut led, 2).await;
        bl_p015.set_high();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_low(); // BL ON (active-low)
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_high();
        Timer::after(Duration::from_millis(500)).await;

        // Case 3: P0.14 active-HIGH
        pulse(&mut led, 3).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p014.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p014.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 4: P0.14 active-LOW
        pulse(&mut led, 4).await;
        bl_p015.set_low();
        bl_p014.set_high();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p014.set_low();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p014.set_high();
        Timer::after(Duration::from_millis(500)).await;

        // Case 5: P0.13 active-HIGH
        pulse(&mut led, 5).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p013.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p013.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 6: P0.04 active-HIGH
        pulse(&mut led, 6).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p004.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p004.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // =================================================================
        // PHASE 2: VDD/VEXT combination sweep
        // Fixed: BL=P0.15 active-HIGH, RST active-low, 240x135 landscape
        // =================================================================
        phase_sep(&mut led).await;

        // Case 1: VTFT active-low + VEXT=LOW (both on — Meshtastic correct)
        pulse(&mut led, 1).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 2: VTFT active-low + VEXT=HIGH (disabled) — VTFT only
        pulse(&mut led, 2).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::High,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 3: VTFT=HIGH (off) + VEXT=LOW — VEXT alone powers display
        pulse(&mut led, 3).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::High,
            Level::Low,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 4: VTFT=HIGH + VEXT=HIGH — neither powered (baseline / sanity check)
        pulse(&mut led, 4).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::High,
            Level::High,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // =================================================================
        // PHASE 3: Portrait orientation sweep
        // Fixed: BL=P0.15 active-HIGH, VTFT active-low, VEXT=LOW, RST active-low
        // Production profile: 135x240, OFFSET_X=52, OFFSET_Y=40
        // =================================================================
        phase_sep(&mut led).await;

        // Case 1: Portrait 135x240, ox=52, oy=40, MADCTL=0x00
        pulse(&mut led, 1).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 135, 240, 52, 40).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 2: Portrait 135x240, ox=52, oy=40, MADCTL=0x60 (MV+MX)
        pulse(&mut led, 2).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x60,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 135, 240, 52, 40).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 3: Portrait 135x240, ox=40, oy=52 (swapped — as tft_sweep mistakenly used)
        pulse(&mut led, 3).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x00,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 135, 240, 40, 52).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // Case 4: Landscape 240x135, MADCTL=0x70 (all rotation bits set)
        pulse(&mut led, 4).await;
        bl_p015.set_low();
        bl_p014.set_low();
        bl_p013.set_low();
        bl_p004.set_low();
        tft_init(
            &mut spi,
            &mut cs,
            &mut dc,
            &mut rst,
            &mut vtft,
            &mut vext,
            0x70,
            Level::Low,
            Level::Low,
        )
        .await;
        bl_p015.set_high();
        show_colors(&mut spi, &mut cs, &mut dc, 240, 135, 0, 0).await;
        bl_p015.set_low();
        Timer::after(Duration::from_millis(500)).await;

        // End of cycle marker: 8 rapid blinks before repeating
        for _ in 0..8 {
            led.set_low();
            Timer::after(Duration::from_millis(100)).await;
            led.set_high();
            Timer::after(Duration::from_millis(100)).await;
        }
        Timer::after(Duration::from_millis(3000)).await;
    }
}

#[cfg(not(target_arch = "arm"))]
fn main() {
    eprintln!("tft_pin_sweep: build with --target thumbv7em-none-eabihf");
}
