//! Which pin, if any, takes the loudspeaker away from the other microcontroller.
//!
//! The schematic says a **CH445P** analogue switch decides whether the DAC listens to the S3 or
//! to the classic ESP32, and that its select input is the S3's **GPIO0**. On this board it is
//! not: with Bluetooth music playing out of the jack, driving GPIO0 to either level for eight
//! and twenty seconds at a time changed nothing at all (`src/bin/audio.rs`).
//!
//! So the pin gets asked for rather than read off paper -- the same method that found the haptic
//! driver's enable line on a GPIO no pin list mentions. Each candidate is driven **low for one
//! and a half seconds, then high for the same**, over and over, while the S3 plays a steady
//! 441 Hz note into its own I2S pins. The knob steps to the next candidate.
//!
//! Reading it, with music from the phone playing through the jack:
//!
//! - **the music chops in a one-and-a-half-second rhythm**, or is replaced by a steady note --
//!   that candidate is the switch, and the log says which pin and which level did it;
//! - **nothing changes on any of them** -- no S3 pin controls the audio path, and the loudspeaker
//!   belongs to the classic ESP32 in the same way the mute already does. Then the way to the DAC
//!   runs through the UART between the two chips, not through a pin of ours.
//!
//! Every step is acknowledged in the case: **the knob buzzes as many times as the number of the
//! candidate it has just moved to**, one for the first, four for the last. Without that there is
//! no way to tell "none of these pins matters" from "the knob never stepped at all" -- a
//! polling loop slow enough to miss a pulse looks exactly like a dead pin.
//!
//! Stop turning the moment something changes: the log records every step, so the pin can be read
//! out of the monitor afterwards rather than counted by hand.
//!
//! Four pins are deliberately not in the list. **GPIO48** is `ESP32S3_RX`, driven by the other
//! chip -- driving it back would be two outputs on one wire. **GPIO46** is `PDM_MIC_DATA`, an
//! output of the microphone, for the same reason. And **GPIO0** is out because it has already
//! been ruled out by ear, which is lucky: it is also the boot strapping pin, and holding it low
//! across a reset leaves the chip sitting in the ROM downloader with nothing on the serial port
//! to say so. **GPIO38** is out because it is spoken for twice over -- measured as the haptic
//! driver's enable line, the schematic calls it `ESP32S3_TX` -- and because holding the haptic
//! enabled is what makes the acknowledgement possible. A pin that has to be low to be tested
//! cannot also be the pin that says the test happened.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma_buffers;
use esp_hal::gpio::{Flex, Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::i2s::master::{Channels, Config, DataFormat, I2s};
use esp_hal::time::{Duration, Instant, Rate};
use teetotum::encoder::Encoder;
use teetotum::haptic::{Actuator, CalTime, Haptic, Library};
use log::{error, info};

const SAMPLE_RATE: u32 = 44_100;
const BYTES_PER_FRAME: usize = 4;
const TX_BYTES: usize = 32_000;
const AMPLITUDE: i16 = 8192;

/// One period of a sine, 256 points, as signed 16-bit samples.
static SINE: [i16; 256] = [
    0, 804, 1608, 2410, 3212, 4011, 4808, 5602, 6393, 7179, 7962, 8739, 9512, 10278, 11039, 11793,
    12539, 13279, 14010, 14732, 15446, 16151, 16846, 17530, 18204, 18868, 19519, 20159, 20787,
    21403, 22005, 22594, 23170, 23731, 24279, 24811, 25329, 25832, 26319, 26790, 27245, 27683,
    28105, 28510, 28898, 29268, 29621, 29956, 30273, 30571, 30852, 31113, 31356, 31580, 31785,
    31971, 32137, 32285, 32412, 32521, 32609, 32678, 32728, 32757, 32767, 32757, 32728, 32678,
    32609, 32521, 32412, 32285, 32137, 31971, 31785, 31580, 31356, 31113, 30852, 30571, 30273,
    29956, 29621, 29268, 28898, 28510, 28105, 27683, 27245, 26790, 26319, 25832, 25329, 24811,
    24279, 23731, 23170, 22594, 22005, 21403, 20787, 20159, 19519, 18868, 18204, 17530, 16846,
    16151, 15446, 14732, 14010, 13279, 12539, 11793, 11039, 10278, 9512, 8739, 7962, 7179, 6393,
    5602, 4808, 4011, 3212, 2410, 1608, 804, 0, -804, -1608, -2410, -3212, -4011, -4808, -5602,
    -6393, -7179, -7962, -8739, -9512, -10278, -11039, -11793, -12539, -13279, -14010, -14732,
    -15446, -16151, -16846, -17530, -18204, -18868, -19519, -20159, -20787, -21403, -22005, -22594,
    -23170, -23731, -24279, -24811, -25329, -25832, -26319, -26790, -27245, -27683, -28105, -28510,
    -28898, -29268, -29621, -29956, -30273, -30571, -30852, -31113, -31356, -31580, -31785, -31971,
    -32137, -32285, -32412, -32521, -32609, -32678, -32728, -32757, -32767, -32757, -32728, -32678,
    -32609, -32521, -32412, -32285, -32137, -31971, -31785, -31580, -31356, -31113, -30852, -30571,
    -30273, -29956, -29621, -29268, -28898, -28510, -28105, -27683, -27245, -26790, -26319, -25832,
    -25329, -24811, -24279, -23731, -23170, -22594, -22005, -21403, -20787, -20159, -19519, -18868,
    -18204, -17530, -16846, -16151, -15446, -14732, -14010, -13279, -12539, -11793, -11039, -10278,
    -9512, -8739, -7962, -7179, -6393, -5602, -4808, -4011, -3212, -2410, -1608, -804,
];

/// ROM effect 1, "strong click": short enough to count, sharp enough to feel through the case.
const CLICK: u8 = 1;

/// Samples per period of the note that plays throughout: 441 Hz.
const NOTE_PERIOD: u32 = 100;

fn fill_tone(buf: &mut [u8]) -> usize {
    let period_bytes = NOTE_PERIOD as usize * BYTES_PER_FRAME;
    let frames = (buf.len() / period_bytes) * NOTE_PERIOD as usize;

    for frame in 0..frames {
        let phase = (frame as u32 % NOTE_PERIOD) * 256 / NOTE_PERIOD;
        let sample = ((SINE[(phase & 0xff) as usize] as i32 * AMPLITUDE as i32) >> 15) as i16;
        let bytes = sample.to_le_bytes();
        let at = frame * BYTES_PER_FRAME;
        buf[at] = bytes[0];
        buf[at + 1] = bytes[1];
        buf[at + 2] = bytes[0];
        buf[at + 3] = bytes[1];
    }

    frames * BYTES_PER_FRAME
}

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    // Every free pin that can be driven without fighting another output. Held as inputs until
    // their turn comes, so that only one candidate is ever driven.
    let mut candidates = [
        (1u8, Flex::new(peripherals.GPIO1)),
        (43, Flex::new(peripherals.GPIO43)),
        (44, Flex::new(peripherals.GPIO44)),
        (45, Flex::new(peripherals.GPIO45)),
    ];
    for (_, pin) in candidates.iter_mut() {
        pin.set_input_enable(true);
    }

    // The acknowledgement. GPIO38 high first: with that line low the driver answers on the bus
    // and moves nothing at all.
    let delay = Delay::new();
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11)
    .with_scl(peripherals.GPIO12);
    let mut haptic = Haptic::new(
        Output::new(peripherals.GPIO38, Level::High, OutputConfig::default()),
        &delay,
    );
    let _ = haptic.wake(&mut i2c);
    let _ = haptic.set_actuator(&mut i2c, Actuator::Lra);
    let _ = haptic.auto_calibrate(&mut i2c, &delay, CalTime::Longest);
    let _ = haptic.set_library(&mut i2c, Library::Lra);

    let (_, _, tx_buffer, tx_descriptors) = dma_buffers!(0, TX_BYTES);
    let i2s = I2s::new(
        peripherals.I2S1,
        peripherals.DMA_CH0,
        Config::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(SAMPLE_RATE))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::STEREO),
    )
    .expect("I2S config rejected");
    let mut i2s_tx = i2s
        .i2s_tx
        .with_bclk(peripherals.GPIO39)
        .with_ws(peripherals.GPIO40)
        .with_dout(peripherals.GPIO41)
        .build(tx_descriptors);

    let used = fill_tone(tx_buffer);
    let tone: &[u8] = &tx_buffer[..used];
    let _transfer = i2s_tx
        .write_dma_circular(&tone)
        .expect("DMA transfer rejected");

    info!("switchhunt: 441 Hz on I2S1 (BCK 39, WS 40, DIN 41), and one candidate driven at a time");
    info!("switchhunt: turn the knob to step; stop the moment the sound changes");

    let mut selected = 0usize;
    let mut announced = usize::MAX;
    let mut level_high = false;
    let mut last_toggle = Instant::now();

    // No sleeping in this loop. The knob's pulses are shorter than a comfortable polling
    // interval, so the encoder is read as fast as the core can, and the level is timed off the
    // clock instead of off a lap count.
    loop {
        // The knob picks the candidate. Any turn, either way, moves one along -- the direction
        // does not matter here, only that the list can be walked at the speed of an ear.
        let moved = encoder.poll();
        if moved != 0 {
            candidates[selected].1.set_input_enable(true);
            candidates[selected].1.set_output_enable(false);
            selected = (selected + moved.unsigned_abs() as usize) % candidates.len();
            last_toggle = Instant::now() - Duration::from_millis(1500);
        }

        if selected != announced {
            info!("candidate {} of {}: GPIO{}", selected + 1, candidates.len(), candidates[selected].0);
            announced = selected;
            // As many taps as the number of the candidate. Turning during the taps is possible
            // but slower than a thumb usually is, so a step lost here is a step not taken.
            for _ in 0..=selected {
                if let Err(err) = haptic.play(&mut i2c, CLICK, &delay) {
                    error!("switchhunt: the acknowledgement could not be played: {err:?}");
                }
                delay.delay_millis(140);
            }
        }

        // Alternate the level under the selected pin every 1.5 s, so that whichever way round the
        // switch reads its input, one half of the cycle is ours.
        if last_toggle.elapsed() >= Duration::from_millis(1500) {
            last_toggle = Instant::now();
            level_high = !level_high;
            let (number, pin) = &mut candidates[selected];
            pin.set_level(if level_high { Level::High } else { Level::Low });
            pin.set_output_enable(true);
            info!("  GPIO{number} driven {}", if level_high { "high" } else { "low" });
        }
    }
}
