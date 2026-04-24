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
use cortex_m::asm;
#[cfg(target_arch = "arm")]
use embassy_nrf::gpio::{Level, Output, OutputDrive};
#[cfg(target_arch = "arm")]
use embedded_alloc::LlffHeap as Heap;

#[cfg(target_arch = "arm")]
#[global_allocator]
static HEAP: Heap = Heap::empty();

#[cfg(target_arch = "arm")]
const HEAP_SIZE: usize = 4096;
#[cfg(target_arch = "arm")]
static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

#[cfg(target_arch = "arm")]
#[used]
#[link_section = ".rodata"]
static STAGE0_BUSY_MARKER: [u8; 35] = *b"T114_STAGE0_BUSY_MARKER_2026_04_18\0";

#[cfg(target_arch = "arm")]
fn busy_delay_ms(ms: u32) {
    // nRF52840 runs at 64 MHz by default after HAL init.
    // One loop delay call roughly burns the provided cycle count.
    let cycles_per_ms = 64_000;
    asm::delay(ms.saturating_mul(cycles_per_ms));
}

#[cfg(target_arch = "arm")]
#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    unsafe {
        HEAP.init(
            core::ptr::addr_of_mut!(HEAP_MEM) as *mut u8 as usize,
            HEAP_SIZE,
        );
    }

    let p = embassy_nrf::init(Default::default());
    let mut led = Output::new(p.P1_03, Level::Low, OutputDrive::Standard);

    loop {
        // Pattern:
        // ON 2500ms -> OFF 2500ms -> ON 150ms -> OFF 150ms -> ON 150ms -> OFF 1500ms.
        led.set_high();
        busy_delay_ms(2500);
        led.set_low();
        busy_delay_ms(2500);

        led.set_high();
        busy_delay_ms(150);
        led.set_low();
        busy_delay_ms(150);

        led.set_high();
        busy_delay_ms(150);
        led.set_low();
        busy_delay_ms(1500);
    }
}

#[cfg(not(target_arch = "arm"))]
fn main() {
    eprintln!("stage0_busy is an embedded target; build with --target thumbv7em-none-eabihf");
}
