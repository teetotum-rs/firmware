//! Which pins move when the knob is turned?
//!
//! The first probe found the encoder's two channels on GPIO8 and GPIO7, as the pin table
//! claimed -- except that only one of them ever changed. A quadrature pair that never advances
//! is one real channel and one pin that belongs to something else, so the count walks one step
//! out and one step back forever.
//!
//! Rather than guess the partner, this listens to every pin that is free to listen on. Each
//! candidate becomes an input with a pull-up, edges are counted for a second at a time, and
//! every second the pins that saw any prints one line. Turn the knob and the channels announce
//! themselves; press it and the button does.
//!
//! Left out, deliberately: GPIO13-18, 21 and 47 drive the display; GPIO19 and 20 are the USB
//! lines this log arrives over; GPIO26-37 belong to the flash and PSRAM, and configuring one
//! of those would take the firmware down with it. GPIO43-46, the UART pair and the two
//! strapping pins, are awkward rather than unavailable and stay in the list.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Pull};
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

/// How often the pins are sampled.
const POLL_INTERVAL_MS: u32 = 1;
/// How many samples make up one report.
const REPORT_EVERY: u32 = 1000;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    // A pull-up on every candidate: an encoder contact and a button both pull to ground, so
    // whatever is wired shows up as a pin that leaves the resting high.
    let config = InputConfig::default().with_pull(Pull::Up);
    let pins: [(u8, Input<'_>); 21] = [
        (0, Input::new(peripherals.GPIO0, config)),
        (1, Input::new(peripherals.GPIO1, config)),
        (2, Input::new(peripherals.GPIO2, config)),
        (3, Input::new(peripherals.GPIO3, config)),
        (4, Input::new(peripherals.GPIO4, config)),
        (5, Input::new(peripherals.GPIO5, config)),
        (6, Input::new(peripherals.GPIO6, config)),
        (7, Input::new(peripherals.GPIO7, config)),
        (8, Input::new(peripherals.GPIO8, config)),
        (9, Input::new(peripherals.GPIO9, config)),
        (10, Input::new(peripherals.GPIO10, config)),
        (38, Input::new(peripherals.GPIO38, config)),
        (39, Input::new(peripherals.GPIO39, config)),
        (40, Input::new(peripherals.GPIO40, config)),
        (41, Input::new(peripherals.GPIO41, config)),
        (42, Input::new(peripherals.GPIO42, config)),
        (43, Input::new(peripherals.GPIO43, config)),
        (44, Input::new(peripherals.GPIO44, config)),
        (45, Input::new(peripherals.GPIO45, config)),
        (46, Input::new(peripherals.GPIO46, config)),
        (48, Input::new(peripherals.GPIO48, config)),
    ];

    let mut level = [false; 21];
    let mut edges = [0u32; 21];
    for (index, (number, pin)) in pins.iter().enumerate() {
        level[index] = pin.is_high();
        info!(
            "Scan: GPIO{number} rests {}",
            if level[index] { "high" } else { "low" }
        );
    }
    info!("Scan: listening -- turn the knob, then press it");

    let mut ticks: u32 = 0;
    loop {
        for (index, (_, pin)) in pins.iter().enumerate() {
            let now = pin.is_high();
            if now != level[index] {
                edges[index] += 1;
                level[index] = now;
            }
        }

        ticks += 1;
        if ticks.is_multiple_of(REPORT_EVERY) {
            for (index, (number, _)) in pins.iter().enumerate() {
                if edges[index] > 0 {
                    info!(
                        "Scan: GPIO{number} {} edges, now {}",
                        edges[index],
                        if level[index] { "high" } else { "low" }
                    );
                    edges[index] = 0;
                }
            }
        }

        delay.delay_millis(POLL_INTERVAL_MS);
    }
}
