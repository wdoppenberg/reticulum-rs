#![cfg_attr(target_arch = "arm", no_std)]
#![cfg_attr(target_arch = "arm", no_main)]

use defmt_rtt as _;

use panic_halt as _;

extern crate alloc;

use core::mem::MaybeUninit;

use embassy_nrf::gpio::{Level, Output, OutputDrive};

use embassy_time::{Duration, Timer};

use embedded_alloc::LlffHeap as Heap;

#[global_allocator]
static HEAP: Heap = Heap::empty();

const HEAP_SIZE: usize = 4096;

static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

#[used]
#[link_section = ".rodata"]
static STAGE0_MARKER: [u8; 30] = *b"T114_STAGE0_MARKER_2026_04_18\0";

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
        // Unmistakable pattern:
        // ON 3s, OFF 1s, ON 200ms, OFF 200ms, ON 200ms, OFF 3s.
        led.set_high();
        Timer::after(Duration::from_secs(3)).await;
        led.set_low();
        Timer::after(Duration::from_secs(1)).await;

        led.set_high();
        Timer::after(Duration::from_millis(200)).await;
        led.set_low();
        Timer::after(Duration::from_millis(200)).await;

        led.set_high();
        Timer::after(Duration::from_millis(200)).await;
        led.set_low();
        Timer::after(Duration::from_secs(3)).await;
    }
}
