//! Listens on GPIO39 for the other microcontroller, and reads what it says as frames.
//!
//! Both factory images were disassembled, and they agree about a link neither the schematic
//! nor this project had looked at:
//!
//! - `UART_NUM_1`, **921600 baud, 8N1**, no flow control, on both chips.
//! - The S3's factory firmware puts it on **GPIO40 (TX) and GPIO39 (RX)** -- not on GPIO38 and
//!   GPIO48, which is where Waveshare's schematic draws it. The classic ESP32 has it on IO23/IO18.
//! - Every message is `magic, cmd, len16, data[len]`, with `0xBD` from the classic ESP32 and
//!   `0xA3` from the S3.
//!
//! That is all read out of two files, so this run exists to make it a measurement. It **drives
//! nothing**: GPIO39 is an output of the other chip, and until this run confirms that, the
//! polite assumption is that it is. Even GPIO40, our own transmit line, is left alone -- the
//! answer to "does it talk" does not need us to talk first.
//!
//! Reading it:
//!
//! - **The pull probe at the top is the pin test.** A line held high against a pull-down is
//!   driven by something; a line that follows whichever pull is applied is connected to nothing
//!   that drives. If GPIO39 is driven and GPIO48 is not, the images are right about the pins.
//! - **Frames with plausible commands and no framing errors** confirm the baud rate as well;
//!   921600 is far enough from its neighbours that a wrong guess produces garbage, not near-misses.
//! - **Silence is not failure.** The classic ESP32 talks when it has something to say: it greets
//!   the display once at its own boot, answers a status query, and streams cover art while
//!   Bluetooth music is playing. Resetting the S3 does not reset it -- to catch the greeting,
//!   power-cycle the whole board; to make it talk at any time, play music over Bluetooth.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Pull};
use esp_hal::time::{Duration, Instant};
use esp_hal::uart::{Config as UartConfig, UartRx};
use log::{error, info, warn};

/// What both firmwares configure, read out of their images.
const BAUD: u32 = 921_600;

/// The header is four bytes; a frame is that plus its `len`.
const HEADER: usize = 4;

/// The largest frame either side builds: 1016 bytes of cover art plus four of sub-header.
const MAX_LEN: usize = 1020;

/// The classic ESP32 speaks with this first byte, the S3 with `0xA3`.
const FROM_ESP32: u8 = 0xBD;
const FROM_S3: u8 = 0xA3;

/// Nothing on the line for this long ends a burst.
const GAP: Duration = Duration::from_millis(200);

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    let delay = Delay::new();

    // The monitor needs a moment after a reset before it is attached to anything.
    delay.delay_millis(500);
    info!("--- who drives which pin ---");
    info!("a pin that keeps its level against both pulls has something else driving it;");
    info!("a pin that follows the pull is connected to nothing that drives.");

    probe("GPIO39 (S3 RX, per the images)", peripherals.GPIO39.reborrow().into(), &delay);
    probe("GPIO40 (S3 TX, per the images)", peripherals.GPIO40.reborrow().into(), &delay);
    probe("GPIO38 (S3 TX, per the schematic)", peripherals.GPIO38.reborrow().into(), &delay);
    probe("GPIO48 (S3 RX, per the schematic)", peripherals.GPIO48.reborrow().into(), &delay);

    let mut rx = match UartRx::new(
        peripherals.UART1,
        UartConfig::default().with_baudrate(BAUD),
    ) {
        Ok(rx) => rx.with_rx(peripherals.GPIO39),
        Err(e) => {
            error!("UART1 refused {BAUD} baud: {e:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    info!("--- listening on GPIO39 at {BAUD} baud, 8N1, until reset ---");
    info!("play music over Bluetooth to give the other chip something to say;");
    info!("power-cycle the board to catch its greeting at boot.");

    let mut frame = Frame::default();
    let mut buf = [0u8; 256];
    let mut errors = 0u32;
    let mut last_byte: Option<Instant> = None;

    loop {
        match rx.read_buffered(&mut buf) {
            Ok(0) => {
                if let Some(seen) = last_byte
                    && seen.elapsed() > GAP
                {
                    frame.gap();
                    last_byte = None;
                }
                delay.delay_millis(2);
            }
            Ok(n) => {
                last_byte = Some(Instant::now());
                for byte in &buf[..n] {
                    frame.push(*byte);
                }
            }
            Err(e) => {
                errors += 1;
                // A wrong baud rate shows up as a flood of these, so they are counted rather
                // than each one written out.
                if errors <= 8 || errors % 100 == 0 {
                    warn!("rx error #{errors}: {e:?}");
                    if errors == 8 {
                        warn!("errors in numbers would mean {BAUD} baud is not the rate after all");
                    }
                }
            }
        }
    }
}

/// Reports whether something outside this chip is holding a pin.
///
/// The pin is only ever an input here. Reading it against a pull-down and then against a pull-up
/// costs nothing and cannot disturb a driver on the other end.
fn probe(name: &str, pin: esp_hal::gpio::AnyPin<'_>, delay: &Delay) {
    let mut pin = Input::new(pin, InputConfig::default().with_pull(Pull::Down));
    // An internal pull is some tens of kiloohms against the pin's own capacitance, so the level
    // is not there the instant the pull is: reading too early reports every floating pin as low.
    delay.delay_millis(2);
    let down = pin.level();
    pin.apply_config(&InputConfig::default().with_pull(Pull::Up));
    delay.delay_millis(2);
    let up = pin.level();
    match (down, up) {
        (Level::High, Level::High) => info!("  {name}: driven high -- an idle transmit line looks exactly like this"),
        (Level::Low, Level::Low) => info!("  {name}: driven low"),
        _ => info!("  {name}: floats, nobody drives it"),
    }
}

/// Reassembles the byte stream into frames and says what each one is.
///
/// Resynchronisation is the whole problem here: the listener joins a conversation already in
/// progress, so the first bytes are usually the tail of a message. Anything that is not a magic
/// byte where a magic byte belongs is counted and dropped, one byte at a time, until the stream
/// makes sense again.
struct Frame {
    /// Header plus as much payload as is worth keeping; longer payloads are counted, not stored.
    ///
    /// Wide enough for a whole metadata frame -- that is the one whose content has to be read
    /// rather than counted. Cover art packets are twelve times this and only their headers matter.
    buf: [u8; HEADER + 128],
    /// How many bytes of the current frame have arrived, payload included.
    seen: usize,
    /// How long the current frame is, header included; zero until the header is complete.
    want: usize,
    /// Bytes thrown away because they were not where a frame could start.
    dropped: u32,
    /// Frames read since the last resynchronisation, for the log line after a gap.
    frames: u32,
}

impl Default for Frame {
    fn default() -> Self {
        Self { buf: [0; HEADER + 128], seen: 0, want: 0, dropped: 0, frames: 0 }
    }
}

impl Frame {
    fn push(&mut self, byte: u8) {
        if self.seen < HEADER {
            // Only a magic byte may open a frame; anything else is the tail of a lost message.
            if self.seen == 0 && byte != FROM_ESP32 && byte != FROM_S3 {
                self.dropped += 1;
                return;
            }
            self.buf[self.seen] = byte;
            self.seen += 1;
            if self.seen == HEADER {
                let len = u16::from_le_bytes([self.buf[2], self.buf[3]]) as usize;
                if len > MAX_LEN {
                    warn!("frame claims {len} bytes of payload, which no sender builds -- resyncing");
                    self.seen = 0;
                    self.dropped += HEADER as u32;
                    return;
                }
                self.want = HEADER + len;
                if self.want == HEADER {
                    self.complete();
                }
            }
            return;
        }

        if self.seen < self.buf.len() {
            self.buf[self.seen] = byte;
        }
        self.seen += 1;
        if self.seen == self.want {
            self.complete();
        }
    }

    /// Prints one finished frame, then starts the next.
    fn complete(&mut self) {
        let magic = self.buf[0];
        let cmd = self.buf[1];
        let len = self.want - HEADER;
        let who = if magic == FROM_ESP32 { "ESP32 ->  S3" } else { "S3    -> ESP32" };
        let kept = self.seen.min(self.buf.len());
        let data = &self.buf[HEADER..kept.max(HEADER)];

        info!(
            "  {who}  cmd {cmd:2} ({}), {len} bytes{}",
            name(magic, cmd),
            if len > data.len() { ", truncated:" } else { ":" }
        );
        dump(data);

        self.frames += 1;
        self.seen = 0;
        self.want = 0;
    }

    /// A pause on the line: report what the burst amounted to and start clean.
    fn gap(&mut self) {
        if self.frames > 0 || self.dropped > 0 || self.seen > 0 {
            info!(
                "  -- gap after {} frames, {} bytes dropped, {} bytes of an unfinished frame --",
                self.frames, self.dropped, self.seen
            );
        }
        self.frames = 0;
        self.dropped = 0;
        self.seen = 0;
        self.want = 0;
    }
}

/// Bytes as hex and as text, sixteen to the line.
///
/// The text column is what makes a metadata frame readable at a glance: its payload is a run of
/// NUL-separated strings, and the separators show up as gaps between words.
fn dump(bytes: &[u8]) {
    for chunk in bytes.chunks(16) {
        let mut text = [b'.'; 16];
        for (slot, byte) in text.iter_mut().zip(chunk) {
            if byte.is_ascii_graphic() || *byte == b' ' {
                *slot = *byte;
            }
        }
        let text = core::str::from_utf8(&text[..chunk.len()]).unwrap_or("");
        info!("      {:02X?}  {}", chunk, text);
    }
}

/// What the images say a command means. Anything unnamed there stays unnamed here.
fn name(magic: u8, cmd: u8) -> &'static str {
    if magic == FROM_ESP32 {
        match cmd {
            1 => "cover art begins",
            2 => "cover art packet",
            3 => "cover art aborted",
            4 => "which packet do you need",
            5 => "status",
            6 => "metadata text",
            7 | 8 => "UI event",
            _ => "not in the image",
        }
    } else {
        match cmd {
            1 => "send that cover packet",
            2 => "cover art complete",
            3 => "mode or transport",
            4 => "media key",
            5 => "restart or teardown",
            6 | 7 => "unread",
            8 => "status query",
            9 => "S3 reports its state",
            11 | 12 => "a pair, on and off",
            _ => "not in the image",
        }
    }
}
