//! What the other microcontroller says on GPIO48, and how fast it says it.
//!
//! Audio on this board belongs to the classic ESP32, not to the S3: the DAC's soft-mute is that
//! chip's pin, and no pin of ours moves the analogue switch (`src/bin/switchhunt.rs`). The only
//! wire left between the two is the UART pair the schematic calls
//! `ESP32S3_TX` (GPIO38) and `ESP32S3_RX` (GPIO48). Before anything is said into it, it is worth
//! knowing what comes out of it -- so this binary never drives a pin. It only listens.
//!
//! Two questions have to be answered in one run, because they answer each other:
//!
//! 1. **How fast does it talk?** Nothing on paper says. So the line is sampled raw, as fast as
//!    the core can read a GPIO register, and the **shortest run of equal samples** is one bit
//!    time. Baud is the sample rate divided by that run. The run-length histogram is printed
//!    alongside: in real 8N1 traffic the lengths cluster on multiples of one bit, and if they do
//!    not, the number below is not a baud rate but noise.
//! 2. **What does it say?** The same captured window is decoded in software as 8N1 at the
//!    measured bit time, which gets the first bytes out even if the burst never repeats. Then
//!    the UART peripheral takes the pin over at the nearest standard rate and dumps everything
//!    that arrives, marking a **gap of more than 50 ms as a message boundary** -- a protocol is
//!    much easier to read once its frames are separated.
//!
//! Reading it:
//!
//! - **"line never fell"** -- the other chip is silent while it is being watched. It is an idle
//!   high line, so silence and a dead line look alike here. Give it something to talk about:
//!   turn the knob, connect the phone over Bluetooth, start and stop music, plug the headphones
//!   in. The wait runs for 30 s and says so.
//! - **A baud estimate that sits on a standard rate** (115200, 9600, ...) and framing errors
//!   that stay at zero -- the link is understood, and the bytes in the dump are the protocol.
//! - **Framing errors climbing with every burst** -- the rate is wrong. The estimate and the
//!   histogram in the same log say by how much.
//!
//! GPIO38, the other half of the pair, is deliberately left alone: this project measured it as
//! the haptic driver's enable line and the schematic calls it the S3's TX, and until that
//! conflict is settled, driving it is not a passive act.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Pull};
use esp_hal::time::{Duration, Instant};
use esp_hal::uart::{Config as UartConfig, RxError, UartRx};
use log::{error, info, warn};

/// The capture buffer, one bit per sample.
///
/// 16 KiB of samples is about five milliseconds of wall clock at the rate a tight polling loop
/// manages, which is a handful of bytes at 115200 baud and a few dozen at the fast rates. That is
/// plenty to measure a bit time; the long listening is the UART's job further down.
const SAMPLE_WORDS: usize = 4096;
const SAMPLE_BITS: usize = SAMPLE_WORDS * 32;

static mut SAMPLES: [u32; SAMPLE_WORDS] = [0; SAMPLE_WORDS];

/// How long to wait for the other chip to say anything at all.
const PATIENCE: Duration = Duration::from_secs(30);

/// Rates a chip is plausibly configured to, nearest wins.
const STANDARD_BAUDS: [u32; 12] = [
    9_600, 19_200, 38_400, 57_600, 74_880, 115_200, 230_400, 460_800, 921_600, 1_000_000,
    1_500_000, 2_000_000,
];

/// A burst is over once this much time passes with nothing on the line.
const GAP: Duration = Duration::from_millis(50);

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    let delay = Delay::new();

    // The monitor needs a moment after a reset before it is attached to anything.
    delay.delay_millis(500);
    info!("listening on GPIO48 -- the other microcontroller's transmit line. Nothing is driven.");

    let samples: &mut [u32; SAMPLE_WORDS] = unsafe { &mut *core::ptr::addr_of_mut!(SAMPLES) };

    let measured = {
        // A UART line idles high, so it is watched with a pull-up -- and the two readings on the
        // way there say whether anybody is driving it at all.
        let mut pin = Input::new(
            peripherals.GPIO48.reborrow(),
            InputConfig::default().with_pull(Pull::Down),
        );
        let pulled_down = pin.level();
        pin.apply_config(&InputConfig::default().with_pull(Pull::Up));
        let pulled_up = pin.level();
        if pulled_down == pulled_up {
            info!("GPIO48 reads {pulled_up:?} against both pulls: something is driving it");
        } else {
            warn!("GPIO48 follows whichever pull is applied -- it floats, nobody drives it");
            warn!("a chip that talks holds its transmit line high between messages, so either");
            warn!("the ESP32 keeps its UART shut until it is spoken to, or its transmit line is");
            warn!("not this pin. The wait below decides between silence and a wrong pin.");
        }
        capture(&pin, samples)
    };

    let baud = match measured {
        Some(rate) => rate,
        None => {
            warn!("nothing to listen to; falling back to 115200 for the dump below");
            115_200
        }
    };

    let mut rx = match UartRx::new(peripherals.UART1, UartConfig::default().with_baudrate(baud)) {
        Ok(rx) => rx.with_rx(peripherals.GPIO48),
        Err(e) => {
            error!("UART1 refused {baud} baud: {e:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    info!("--- listening at {baud} baud, 8N1, until reset ---");
    let mut buf = [0u8; 128];
    let mut errors = 0u32;
    let mut last_byte: Option<Instant> = None;

    loop {
        match rx.read_buffered(&mut buf) {
            Ok(0) => {
                if let Some(seen) = last_byte
                    && seen.elapsed() > GAP
                {
                    info!("  -- gap --");
                    last_byte = None;
                }
                delay.delay_millis(2);
            }
            Ok(n) => {
                last_byte = Some(Instant::now());
                dump(&buf[..n]);
            }
            Err(e) => {
                errors += 1;
                // A wrong baud rate shows up as a flood of these, so they are counted rather
                // than each one written out.
                if errors <= 8 || errors % 100 == 0 {
                    warn!("rx error #{errors}: {e:?}");
                    if matches!(e, RxError::FrameFormatViolated) && errors == 8 {
                        warn!("framing errors in numbers mean {baud} baud is not the rate");
                    }
                }
            }
        }
    }
}

/// Waits for the line to fall, captures a window of raw samples, and reports what it saw.
///
/// Returns the nearest standard baud rate to the measurement, or `None` if the line stayed quiet.
fn capture(pin: &Input<'_>, samples: &mut [u32; SAMPLE_WORDS]) -> Option<u32> {
    info!(
        "line idles {:?}; waiting up to 30 s for the first start bit",
        pin.level()
    );
    if pin.level() == Level::Low {
        warn!("the line is already low with a pull-up on it: that is a driven low, not an idle");
        warn!("UART, and the capture below starts wherever it happens to start");
    }

    let waiting = Instant::now();
    let mut spins = 0u32;
    while pin.is_high() {
        spins = spins.wrapping_add(1);
        // Checking the clock costs more than the sample does, so it is checked rarely.
        if spins % 4096 == 0 && waiting.elapsed() > PATIENCE {
            warn!("line never fell in 30 s: the other chip said nothing while it was watched");
            return None;
        }
    }

    // From here to the end of the loop nothing else may happen: every instruction between two
    // samples widens the sampling interval.
    let started = Instant::now();
    for word in samples.iter_mut() {
        let mut bits = 0u32;
        for _ in 0..32 {
            bits = (bits << 1) | pin.is_high() as u32;
        }
        *word = bits;
    }
    let elapsed = started.elapsed().as_micros().max(1);

    let sample_rate_khz = (SAMPLE_BITS as u64 * 1000 / elapsed as u64) as u32;
    info!(
        "captured {SAMPLE_BITS} samples in {elapsed} us -- {sample_rate_khz} kS/s, a window of {} us",
        elapsed
    );

    let shortest = histogram(samples)?;
    let estimate = (sample_rate_khz as u64 * 1000 / shortest as u64) as u32;
    let nearest = STANDARD_BAUDS
        .iter()
        .copied()
        .min_by_key(|&std| std.abs_diff(estimate))?;
    let off_by = (estimate.abs_diff(nearest) as u64 * 100 / nearest as u64) as u32;

    info!("shortest run {shortest} samples -> about {estimate} baud");
    info!("nearest standard rate {nearest} baud, {off_by} % away from the estimate");
    if off_by > 10 {
        warn!("that is far off any standard rate -- read the histogram before believing it");
    }

    decode(samples, sample_rate_khz, nearest);
    Some(nearest)
}

/// Prints how long the runs of equal samples were, and returns the shortest of them.
///
/// The shortest run is one bit time only if the lengths cluster on multiples of it, which is
/// what the printed table is for. A single stray one-sample run -- a glitch, or the moment the
/// loop was interrupted -- would otherwise be taken for a very fast baud rate, so runs of one or
/// two samples are reported but not used as the answer.
fn histogram(samples: &[u32; SAMPLE_WORDS]) -> Option<u32> {
    let mut counts = [0u32; 33];
    let mut shortest = u32::MAX;
    let mut runs = 0u32;
    let mut glitches = 0u32;

    let mut run = 1u32;
    let mut previous = bit_at(samples, 0);
    // The last run is left out: it runs into the end of the window and is not a whole run.
    for index in 1..SAMPLE_BITS {
        let current = bit_at(samples, index);
        if current == previous {
            run += 1;
            continue;
        }
        runs += 1;
        // The table buckets by powers of two so that one printable row covers every rate.
        let bucket = (32 - run.leading_zeros()) as usize;
        counts[bucket] += 1;
        if run <= 2 {
            glitches += 1;
        } else {
            shortest = shortest.min(run);
        }
        run = 1;
        previous = current;
    }

    if runs < 8 {
        warn!("only {runs} edges in the whole window -- too little traffic to measure a rate");
        return None;
    }
    info!("{runs} runs of equal samples, {glitches} of them one or two samples long:");
    for (bucket, count) in counts.iter().enumerate() {
        if *count > 0 {
            let low = if bucket == 0 { 0 } else { 1 << (bucket - 1) };
            info!("  {low:>7}..{:<7} samples: {count}", (1u32 << bucket) - 1);
        }
    }

    (shortest != u32::MAX).then_some(shortest)
}

/// Reads the captured window as 8N1 at the given rate and prints the bytes.
///
/// The window is only a few milliseconds wide, so this is the first breath of a message rather
/// than the message; the UART peripheral collects the rest. It matters anyway, because a burst
/// that happens once -- a greeting at boot, a reply to a plugged-in jack -- is in this window and
/// nowhere else.
fn decode(samples: &[u32; SAMPLE_WORDS], sample_rate_khz: u32, baud: u32) {
    // Samples per bit in 24.8 fixed point; the bit centres drift by less than a sample this way.
    let per_bit = ((sample_rate_khz as u64 * 1000) << 8) / baud as u64;
    if per_bit < (3 << 8) {
        warn!("under three samples to the bit at {baud} baud -- not decoding the window");
        return;
    }

    let mut bytes = [0u8; 64];
    let mut count = 0usize;
    let mut framing = 0u32;
    let mut index = 0usize;

    while index + 1 < SAMPLE_BITS && count < bytes.len() {
        // A frame starts where the line falls.
        if !(bit_at(samples, index) && !bit_at(samples, index + 1)) {
            index += 1;
            continue;
        }
        let start = ((index + 1) as u64) << 8;
        let centre = |bit: u64| ((start + per_bit * bit + per_bit / 2) >> 8) as usize;
        let stop = centre(9);
        if stop >= SAMPLE_BITS {
            break;
        }
        let mut byte = 0u8;
        for bit in 0..8 {
            // Least significant bit first, the way a UART sends it.
            byte |= (bit_at(samples, centre(1 + bit as u64)) as u8) << bit;
        }
        if !bit_at(samples, stop) {
            framing += 1;
        }
        bytes[count] = byte;
        count += 1;
        index = stop;
    }

    if count == 0 {
        warn!("no frame decoded out of the window");
        return;
    }
    info!("{count} bytes decoded from the window, {framing} of them with a bad stop bit:");
    dump(&bytes[..count]);
}

/// One sample out of the packed capture buffer.
#[inline(always)]
fn bit_at(samples: &[u32; SAMPLE_WORDS], index: usize) -> bool {
    samples[index >> 5] >> (31 - (index & 31)) & 1 != 0
}

/// Bytes as hex and as text, sixteen to the line.
fn dump(bytes: &[u8]) {
    for chunk in bytes.chunks(16) {
        let mut text = [b'.'; 16];
        for (slot, byte) in text.iter_mut().zip(chunk) {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *slot = *byte;
            }
        }
        let text = core::str::from_utf8(&text[..chunk.len()]).unwrap_or("");
        info!("  {:02X?}  {}", chunk, text);
    }
}
