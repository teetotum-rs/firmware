//! A tone through the loudspeaker, and the pin that decides who gets to make it.
//!
//! The board's audio path: a **PCM5100A** DAC fed by plain I2S on **BCK GPIO39, LRCK/WS GPIO40,
//! DIN GPIO41**, with no master clock (the DAC's `SCK` pin is grounded and it runs its own PLL).
//! A **CH445P** analogue switch sits between the two microcontrollers and that DAC; **GPIO0**
//! selects which of them is connected, high for the S3.
//!
//! **The DAC's soft-mute pin, `XSMT`, is wired only to the classic ESP32's IO32.** Mute is active
//! low with no pull-up of its own, so the S3 can clock samples into a muted DAC and never know it.
//! Audio from the S3 alone therefore needs the other microcontroller's cooperation to unmute.
//!
//! This run alternates GPIO0 high (eight seconds, S3 plays four rising notes) and low (twenty
//! seconds, S3 silent, the ESP32's half). Pair a phone with `TAIJI_KNOB_AUDIO` and start music;
//! what is heard settles three things:
//!
//! - **music throughout** -- jack, DAC and its supply all work, and the ESP32 drives `XSMT` high
//!   while it plays;
//! - **music cut into bursts** by the low window -- the CH445P switch follows GPIO0, low is the
//!   ESP32's side;
//! - **the S3's notes stay inaudible even while its music plays** -- the mute is the only thing
//!   left in the way.
//!
//! GPIO0 is the boot strapping pin: a reset while it is held low puts the ROM into download mode.
//! This binary leaves it high between phases and never sleeps for long with it low.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma_buffers;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2s::master::{Channels, Config, DataFormat, I2s};
use esp_hal::time::Rate;
use log::info;

/// The rate the DAC is clocked at. 44.1 kHz keeps the note periods below whole numbers of
/// samples, which is what lets the DMA buffer loop without a click at the seam.
const SAMPLE_RATE: u32 = 44_100;

/// Bytes handed to the DMA in one lap: two channels of 16 bits, so four bytes a frame.
const BYTES_PER_FRAME: usize = 4;
const TX_BYTES: usize = 32_000;

/// A quarter of full scale. Line level into headphones with no volume control anywhere.
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

/// The four notes, as whole numbers of samples per period so that a buffer holds an exact
/// number of them: 441, 551, 668 and 882 Hz -- a rising figure and then the octave.
const NOTE_PERIODS: [u32; 4] = [100, 80, 66, 50];

/// Writes whole periods of a sine into `buf` and returns how many bytes were used.
///
/// Only whole periods: the DMA replays the buffer end to end forever, and a partial period at
/// the end would be a discontinuity at every lap -- a buzz on top of the note.
fn fill_tone(buf: &mut [u8], period_samples: u32) -> usize {
    let period_bytes = period_samples as usize * BYTES_PER_FRAME;
    let periods = buf.len() / period_bytes;
    let frames = periods * period_samples as usize;

    for frame in 0..frames {
        let phase = (frame as u32 % period_samples) * 256 / period_samples;
        let raw = SINE[(phase & 0xff) as usize] as i32;
        let sample = ((raw * AMPLITUDE as i32) >> 15) as i16;
        let bytes = sample.to_le_bytes();
        let at = frame * BYTES_PER_FRAME;
        // The same sample in both slots: the jack is stereo, the tone is not.
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

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    // The switch. High is Waveshare's own setting for "the S3 drives the DAC"; the run tries
    // both so that the claim is measured rather than repeated.
    let mut switch = Output::new(peripherals.GPIO0, Level::High, OutputConfig::default());

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

    // Philips framing, because the DAC's FMT pin is tied to GND. Waveshare's demo configures
    // left-justified instead, which would shift every sample by one bit position; if the note
    // sounds right here and their demo sounds right too, the DAC tolerates both.
    let mut i2s_tx = i2s
        .i2s_tx
        .with_bclk(peripherals.GPIO39)
        .with_ws(peripherals.GPIO40)
        .with_dout(peripherals.GPIO41)
        .build(tx_descriptors);

    info!("audio: PCM5100A over I2S1, BCK 39, WS 40, DIN 41, {SAMPLE_RATE} Hz, 16 bit stereo");
    info!("audio: plug something into the 3.5 mm jack and listen for four rising notes");

    let mut round: u32 = 1;
    loop {
        switch.set_level(Level::High);
        info!("round {round}: GPIO0 high, the S3 plays -- four notes");

        for (note, period) in NOTE_PERIODS.iter().enumerate() {
            let used = fill_tone(tx_buffer, *period);
            let hz = SAMPLE_RATE / period;
            info!("  note {} of 4: {hz} Hz", note + 1);

            let tone: &[u8] = &tx_buffer[..used];
            let transfer = i2s_tx
                .write_dma_circular(&tone)
                .expect("DMA transfer rejected");
            delay.delay_millis(1000);
            transfer.stop().ok();
        }

        info!("round {round}: GPIO0 low, the S3 silent -- twenty seconds that belong to the ESP32");
        switch.set_level(Level::Low);
        delay.delay_millis(20_000);

        // Back to high before anything else: a reset with this pin low lands in the ROM
        // downloader, and the pin should not be left there while nothing is watching.
        switch.set_level(Level::High);
        round += 1;
    }
}
