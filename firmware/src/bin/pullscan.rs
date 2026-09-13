//! Which pins does the board itself hold high?
//!
//! The SD card and the audio path are the last two pieces of hardware nobody has measured, and
//! no pin list names them. The factory image proves the card is driven by the SDMMC host, but
//! on an ESP32-S3 that host runs through the GPIO matrix, so the pin numbers live as immediates
//! in the factory code and not as a table anyone can read.
//!
//! This asks the board instead. Every free pin is read twice: once with the chip's internal
//! pull-up, once with its internal pull-down. The internal resistors are weak -- 45 kOhm is the
//! datasheet's typical value -- so a pin with the usual 10 kOhm external pull-up wins the tug of
//! war and reads high both times. A pin with nothing on it simply follows whichever internal
//! resistor is switched on.
//!
//! | reads with pull-down | reads with pull-up | what it means |
//! |---|---|---|
//! | high | high | held high by something external -- a pull-up, or a chip driving it |
//! | low | low | held low by something external |
//! | low | high | floating, nothing attached that matters at rest |
//!
//! SD is the reason this is worth a flash cycle: CMD and DAT0-DAT3 carry external pull-ups on
//! every board that works, CLK does not. GPIO11 and GPIO12 are in the list as a control -- they
//! are the I2C bus, they are known to be pulled up, and if they do not come out as "external
//! high" then the method is wrong and nothing else in the run means anything.
//!
//! Left out, as in `gpioscan`: GPIO13-18, 21 and 47 drive the display, GPIO19 and 20 are the USB
//! lines this log arrives over, and GPIO26-37 belong to the flash and the PSRAM.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Pull};
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

/// How long to let a pin settle after switching the internal resistor.
///
/// A pin with nothing on it is a few picofarads against 45 kOhm, so microseconds would do; the
/// millisecond is for whatever capacitance a real net brings with it.
const SETTLE_MS: u32 = 2;

/// How many times each pin is read, to catch a line that is being driven rather than pulled.
const ROUNDS: u32 = 8;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    let up = InputConfig::default().with_pull(Pull::Up);
    let down = InputConfig::default().with_pull(Pull::Down);

    let mut pins: [(u8, Input<'_>); 23] = [
        (0, Input::new(peripherals.GPIO0, up)),
        (1, Input::new(peripherals.GPIO1, up)),
        (2, Input::new(peripherals.GPIO2, up)),
        (3, Input::new(peripherals.GPIO3, up)),
        (4, Input::new(peripherals.GPIO4, up)),
        (5, Input::new(peripherals.GPIO5, up)),
        (6, Input::new(peripherals.GPIO6, up)),
        (7, Input::new(peripherals.GPIO7, up)),
        (8, Input::new(peripherals.GPIO8, up)),
        (9, Input::new(peripherals.GPIO9, up)),
        (10, Input::new(peripherals.GPIO10, up)),
        (11, Input::new(peripherals.GPIO11, up)),
        (12, Input::new(peripherals.GPIO12, up)),
        (38, Input::new(peripherals.GPIO38, up)),
        (39, Input::new(peripherals.GPIO39, up)),
        (40, Input::new(peripherals.GPIO40, up)),
        (41, Input::new(peripherals.GPIO41, up)),
        (42, Input::new(peripherals.GPIO42, up)),
        (43, Input::new(peripherals.GPIO43, up)),
        (44, Input::new(peripherals.GPIO44, up)),
        (45, Input::new(peripherals.GPIO45, up)),
        (46, Input::new(peripherals.GPIO46, up)),
        (48, Input::new(peripherals.GPIO48, up)),
    ];

    // Counted separately so a line that changes under the same internal resistor -- a chip
    // talking, not a resistor pulling -- shows up as a count between 0 and ROUNDS.
    let mut high_with_up = [0u32; 23];
    let mut high_with_down = [0u32; 23];

    for _ in 0..ROUNDS {
        for (index, (_, pin)) in pins.iter_mut().enumerate() {
            pin.apply_config(&up);
            delay.delay_millis(SETTLE_MS);
            if pin.is_high() {
                high_with_up[index] += 1;
            }

            pin.apply_config(&down);
            delay.delay_millis(SETTLE_MS);
            if pin.is_high() {
                high_with_down[index] += 1;
            }
        }
    }

    info!("Pull: {ROUNDS} rounds, pin / high with pull-up / high with pull-down / verdict");
    for (index, (number, _)) in pins.iter().enumerate() {
        let u = high_with_up[index];
        let d = high_with_down[index];
        let verdict = match (u, d) {
            (u, d) if u == ROUNDS && d == ROUNDS => "held HIGH externally",
            (0, 0) => "held low externally",
            (u, 0) if u == ROUNDS => "floating",
            _ => "changing -- something is driving it",
        };
        info!("Pull: GPIO{number:<2} up {u}/{ROUNDS} down {d}/{ROUNDS}  {verdict}");
    }
    info!("Pull: done");

    loop {
        delay.delay_millis(1000);
    }
}
