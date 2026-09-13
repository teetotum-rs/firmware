//! What sequence do the two encoder pins actually produce?
//!
//! The pin scan found edges on GPIO8 and on GPIO7, but never in the same second: four seconds
//! of GPIO8 alone, then four seconds of GPIO7 alone. Two channels of one quadrature encoder
//! cannot behave that way -- they are a quarter cycle apart, so every turn moves both. Either
//! the two pins belong to two different controls, or the pair is not quadrature at all.
//!
//! A counter cannot tell those apart, so this prints the raw sequence instead. Both pins are
//! sampled every 100 microseconds, each change is stored as its two-bit state with the
//! milliseconds since boot, and a burst is flushed to the log once the pins have been still
//! for a moment. What comes out is the transition sequence itself: `11 -> 10 -> 00 -> 01` and
//! back to `11` is quadrature turning one way, the same states in reverse are the other way,
//! and a pin that toggles between two states while the other never moves is not an encoder
//! channel at all.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::time::Instant;
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

/// How often the two pins are read.
///
/// Fast enough that a contact bouncing for a few hundred microseconds shows as several
/// entries rather than as one clean edge -- bounce is worth seeing here, not hiding.
const SAMPLE_INTERVAL_US: u32 = 100;
/// How long the pins must stay still before the collected burst is printed.
const QUIET_MS: u64 = 150;
/// Transitions held before the buffer is flushed regardless of quiet time.
const BURST: usize = 24;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    let config = InputConfig::default().with_pull(Pull::Up);
    let a = Input::new(peripherals.GPIO8, config);
    let b = Input::new(peripherals.GPIO7, config);

    let mut state = state_of(&a, &b);
    info!("Encoder: resting at A={} B={}", state >> 1, state & 1);
    info!("Encoder: tracing -- turn the knob slowly, one way");

    // Each entry is a transition: the state after it, and when it happened.
    let mut burst = [(0u8, 0u64); BURST];
    let mut held = 0usize;
    let mut last_change = Instant::now();

    loop {
        let now = state_of(&a, &b);
        if now != state {
            state = now;
            if held < BURST {
                burst[held] = (now, Instant::now().duration_since_epoch().as_millis());
                held += 1;
            }
            last_change = Instant::now();
        }

        let quiet = (Instant::now() - last_change).as_millis() >= QUIET_MS;
        if held == BURST || (held > 0 && quiet) {
            report(&burst[..held]);
            held = 0;
        }

        delay.delay_micros(SAMPLE_INTERVAL_US);
    }
}

/// Reads the two pins as one two-bit state, A in the high bit.
fn state_of(a: &Input<'_>, b: &Input<'_>) -> u8 {
    u8::from(a.is_high()) << 1 | u8::from(b.is_high())
}

/// Prints one burst of transitions as states with the gap in milliseconds between them.
fn report(burst: &[(u8, u64)]) {
    let mut previous = burst[0].1;
    for &(state, at) in burst {
        info!(
            "Encoder: A={} B={} after {} ms",
            state >> 1,
            state & 1,
            at.saturating_sub(previous)
        );
        previous = at;
    }
    info!("Encoder: --- burst of {} ---", burst.len());
}
