#![no_std]
#![no_main]

use defmt_rtt as _;

use panic_halt as _;

use embassy_nrf::gpio::{Level, Output, OutputDrive};

use embassy_time::{Duration, Timer};

#[embassy_executor::main]
async fn main(_spawner: embassy_executor::Spawner) {
    let p = embassy_nrf::init(Default::default());
    let mut led = Output::new(p.P1_03, Level::Low, OutputDrive::HighDrive);

    // Distinctive signature: 5 very fast pulses, long pause.
    loop {
        for _ in 0..5 {
            led.set_high();
            Timer::after(Duration::from_millis(500)).await;
            led.set_low();
            Timer::after(Duration::from_millis(500)).await;
        }
        Timer::after(Duration::from_millis(2000)).await;
    }
}
