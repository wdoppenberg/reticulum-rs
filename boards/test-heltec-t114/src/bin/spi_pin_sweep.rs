//! SPI pin sweep: tries multiple SCK/MOSI pin combinations for the TFT.
//!
//! VEXT=LOW, VTFT active-low, RST active-low — all confirmed working.
//! BL: P0.15 HIGH + P1.10 LOW simultaneously (covers both polarity guesses).
//!
//! LED coding (P1.03, active-low):
//!   N slow pulses → case N SPI combo is being tried
//!   RED 3s / GREEN 3s / BLUE 3s → if display shows color, note N
//!   8 rapid blinks → end of cycle
//!
//! Combo table:
//!   1: P1.08 / P1.09  (original assumption — re-tested here as baseline)
//!   2: P0.08 / P0.09  (most likely: Arduino pin 8/9 = P0.08/P0.09)
//!   3: P0.09 / P0.10  (shifted by one)
//!   4: P0.07 / P0.06  (near other port-0 TFT control pins)
//!   5: P0.05 / P0.04  (another port-0 cluster)
//!   6: P0.16 / P0.17  (mid-range port 0)
//!   7: P0.26 / P0.27  (upper port 0)
//!   8: P0.14 / P0.13  (was tried as BL — maybe SCK/MOSI?)

#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(target_arch = "arm", no_main)]

#[cfg(target_arch = "arm")]
use defmt_rtt as _;
#[cfg(target_arch = "arm")]
use panic_halt as _;

#[cfg(target_arch = "arm")]
use embassy_nrf::gpio::{Level, Output, OutputDrive};
#[cfg(target_arch = "arm")]
use embassy_nrf::pac;
#[cfg(target_arch = "arm")]
use embassy_nrf::pac::shared::vals::Connect;
#[cfg(target_arch = "arm")]
use embassy_nrf::pac::spim::vals::{Enable as SpimEnable, Frequency as SpimFrequency};
#[cfg(target_arch = "arm")]
use embassy_time::{Duration, Timer};

#[cfg(target_arch = "arm")]
#[used]
#[link_section = ".rodata"]
static MARKER: [u8; 39] = *b"T114_SPI_PIN_SWEEP_MARKER_2026_04_18_A\0";

// SPI combos: (sck_port, sck_pin, mosi_port, mosi_pin)
// port: 0 = P0.xx, 1 = P1.xx
#[cfg(target_arch = "arm")]
const COMBOS: [(u8, u8, u8, u8); 8] = [
    (1, 8, 1, 9),   // 1: P1.08 / P1.09 — original (baseline re-test)
    (0, 8, 0, 9),   // 2: P0.08 / P0.09 — most likely correct
    (0, 9, 0, 10),  // 3: P0.09 / P0.10
    (0, 7, 0, 6),   // 4: P0.07 / P0.06
    (0, 5, 0, 4),   // 5: P0.05 / P0.04
    (0, 16, 0, 17), // 6: P0.16 / P0.17
    (0, 26, 0, 27), // 7: P0.26 / P0.27
    (0, 14, 0, 13), // 8: P0.14 / P0.13 (was tested as BL candidates)
];

// Completely disable-then-reconfigure SPIM1 for new SCK/MOSI pins.
// Uses the PAC directly so we can change PSEL at runtime.
//
// Safety: no embassy Spim<'_> may be alive for TWISPI1 when called.
#[cfg(target_arch = "arm")]
unsafe fn spim1_configure(sck_port: u8, sck_pin: u8, mosi_port: u8, mosi_pin: u8) {
    let s = pac::SPIM1;

    s.enable().write(|w| w.set_enable(SpimEnable::DISABLED));

    // Disconnect all pins first
    s.psel()
        .sck()
        .write(|w| w.set_connect(Connect::DISCONNECTED));
    s.psel()
        .mosi()
        .write(|w| w.set_connect(Connect::DISCONNECTED));
    s.psel()
        .miso()
        .write(|w| w.set_connect(Connect::DISCONNECTED));

    // Set SCK
    s.psel().sck().write(|w| {
        w.set_pin(sck_pin);
        w.set_port(sck_port != 0);
        w.set_connect(Connect::CONNECTED);
    });

    // Set MOSI
    s.psel().mosi().write(|w| {
        w.set_pin(mosi_pin);
        w.set_port(mosi_port != 0);
        w.set_connect(Connect::CONNECTED);
    });

    // 8 MHz, Mode 0 (CPOL=0, CPHA=0), MSB first
    s.frequency().write(|w| w.set_frequency(SpimFrequency::M8));
    s.config().write(|_w| {}); // default = MSB first, mode 0

    s.enable().write(|w| w.set_enable(SpimEnable::ENABLED));
}

// Blocking SPI write via SPIM1 EasyDMA.
// `buf` MUST be in RAM (not flash).
//
// Safety: SPIM1 must be configured and enabled; `buf` must be RAM-resident.
#[cfg(target_arch = "arm")]
unsafe fn spim1_write_raw(buf: &[u8]) {
    if buf.is_empty() {
        return;
    }
    let s = pac::SPIM1;
    s.events_end().write_value(0);
    s.events_endtx().write_value(0);
    s.dma().tx().ptr().write_value(buf.as_ptr() as u32);
    s.dma()
        .tx()
        .maxcnt()
        .write(|w| w.set_maxcnt(buf.len() as _));
    s.dma().rx().maxcnt().write(|w| w.set_maxcnt(0));
    s.tasks_start().write_value(1);
    while s.events_end().read() == 0 {}
    s.events_end().write_value(0);
}

#[cfg(target_arch = "arm")]
fn tft_cmd(cs: &mut Output<'_>, dc: &mut Output<'_>, cmd: u8) {
    let buf = [cmd];
    cs.set_low();
    dc.set_low();
    unsafe { spim1_write_raw(&buf) };
    cs.set_high();
}

#[cfg(target_arch = "arm")]
fn tft_cmd_data(cs: &mut Output<'_>, dc: &mut Output<'_>, cmd: u8, data: &[u8]) {
    let buf = [cmd];
    let mut tmp = [0u8; 64];
    cs.set_low();
    dc.set_low();
    unsafe { spim1_write_raw(&buf) };
    dc.set_high();
    let mut off = 0;
    while off < data.len() {
        let n = (data.len() - off).min(64);
        tmp[..n].copy_from_slice(&data[off..off + n]);
        unsafe { spim1_write_raw(&tmp[..n]) };
        off += n;
    }
    cs.set_high();
}

#[cfg(target_arch = "arm")]
fn tft_fill(
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
        cs,
        dc,
        0x2A,
        &[(ox >> 8) as u8, ox as u8, (xe >> 8) as u8, xe as u8],
    );
    tft_cmd_data(
        cs,
        dc,
        0x2B,
        &[(oy >> 8) as u8, oy as u8, (ye >> 8) as u8, ye as u8],
    );
    tft_cmd(cs, dc, 0x2C);

    let mut chunk = [0u8; 128];
    for i in (0..128usize).step_by(2) {
        chunk[i] = hi;
        chunk[i + 1] = lo;
    }
    cs.set_low();
    dc.set_high();
    let mut left = w as usize * h as usize;
    while left > 0 {
        let px = left.min(64);
        unsafe { spim1_write_raw(&chunk[..px * 2]) };
        left -= px;
    }
    cs.set_high();
}

#[cfg(target_arch = "arm")]
async fn tft_init(
    cs: &mut Output<'_>,
    dc: &mut Output<'_>,
    rst: &mut Output<'_>,
    vtft: &mut Output<'_>,
    vext: &mut Output<'_>,
) {
    cs.set_high();
    dc.set_high();
    vtft.set_high(); // VTFT off
    vext.set_low(); // VEXT always on (active-low)
    rst.set_high();
    Timer::after(Duration::from_millis(10)).await;

    vtft.set_low(); // VTFT on (active-low)
    Timer::after(Duration::from_millis(30)).await;

    rst.set_low();
    Timer::after(Duration::from_millis(20)).await;
    rst.set_high();
    Timer::after(Duration::from_millis(150)).await;

    tft_cmd(cs, dc, 0x01); // SWRESET
    Timer::after(Duration::from_millis(150)).await;
    tft_cmd(cs, dc, 0x11); // SLPOUT
    Timer::after(Duration::from_millis(120)).await;
    tft_cmd_data(cs, dc, 0x3A, &[0x55]); // COLMOD RGB565
    tft_cmd_data(cs, dc, 0x36, &[0x00]); // MADCTL
    tft_cmd(cs, dc, 0x21); // INVON
    tft_cmd(cs, dc, 0x13); // NORON
    tft_cmd(cs, dc, 0x29); // DISPON
    Timer::after(Duration::from_millis(50)).await;
}

#[cfg(target_arch = "arm")]
async fn pulse(led: &mut Output<'_>, n: u8) {
    for _ in 0..n {
        led.set_low();
        Timer::after(Duration::from_millis(250)).await;
        led.set_high();
        Timer::after(Duration::from_millis(250)).await;
    }
    Timer::after(Duration::from_millis(900)).await;
}

#[cfg(target_arch = "arm")]
#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    let p = embassy_nrf::init(Default::default());

    // Configure SPIM1 directly via PAC — no embassy Spim used.
    // Initial config with placeholder pins; spim1_configure() will override.
    unsafe { spim1_configure(1, 8, 1, 9) };

    let mut led = Output::new(p.P1_03, Level::High, OutputDrive::Standard);
    let mut cs = Output::new(p.P0_11, Level::High, OutputDrive::Standard);
    let mut dc = Output::new(p.P0_12, Level::High, OutputDrive::Standard);
    let mut rst = Output::new(p.P0_02, Level::High, OutputDrive::Standard);
    let mut vtft = Output::new(p.P0_03, Level::High, OutputDrive::Standard);
    let mut vext = Output::new(p.P0_21, Level::Low, OutputDrive::Standard);
    let _adc_en = Output::new(p.P0_06, Level::High, OutputDrive::Standard);

    // Enable both BL candidates simultaneously.
    let mut bl_p015 = Output::new(p.P0_15, Level::High, OutputDrive::Standard);
    let mut bl_p110 = Output::new(p.P1_10, Level::Low, OutputDrive::Standard);

    loop {
        for (i, &(sck_p, sck_n, mosi_p, mosi_n)) in COMBOS.iter().enumerate() {
            let case = (i + 1) as u8;

            pulse(&mut led, case).await;

            unsafe { spim1_configure(sck_p, sck_n, mosi_p, mosi_n) };

            bl_p015.set_low();
            bl_p110.set_high();
            tft_init(&mut cs, &mut dc, &mut rst, &mut vtft, &mut vext).await;
            bl_p015.set_high();
            bl_p110.set_low();

            tft_fill(&mut cs, &mut dc, 0xF8, 0x00, 240, 135, 0, 0); // RED
            Timer::after(Duration::from_millis(3000)).await;
            tft_fill(&mut cs, &mut dc, 0x07, 0xE0, 240, 135, 0, 0); // GREEN
            Timer::after(Duration::from_millis(3000)).await;
            tft_fill(&mut cs, &mut dc, 0x00, 0x1F, 240, 135, 0, 0); // BLUE
            Timer::after(Duration::from_millis(3000)).await;

            bl_p015.set_low();
            bl_p110.set_high();
            Timer::after(Duration::from_millis(500)).await;
        }

        for _ in 0..8u8 {
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
    eprintln!("spi_pin_sweep: build with --target thumbv7em-none-eabihf");
}
