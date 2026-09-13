//! The second rotary encoder, asked for over the link to the other chip.
//!
//! The board's datasheet claims two rotary encoders and only one of them has ever answered here:
//! the knob on GPIO8/7. The other one, `EC2_A`/`EC2_B` in the schematic, is wired to the classic
//! ESP32. Its factory image shows what it does with it:
//!
//! * `iot_knob_create` is called with a four-byte constant in DROM, `default_direction = 0`,
//!   `gpio_encoder_a = 19`, `gpio_encoder_b = 22` -- exactly the schematic's two pins.
//! * Both callbacks read one state byte first, do nothing unless **bit 0** is set, and then look
//!   at **bits 1..3**: on 1 a turn goes into the chip's own event queue, on 2 it is sent to us as
//!   **`BD 07`** (`iot_knob`'s "right", **clockwise** on this board) or **`BD 08`** ("left",
//!   anticlockwise).
//! * That state byte is the one we write with `A3 09`, and it comes back as `data[0]` of `BD 05`.
//!
//! So the second encoder can be ours without replacing anything, and this run is the test of
//! that: `0x00` (off), `0x05` (bit 0 set, mode 2 -- to us), `0x03` (bit 0 set, mode 1 -- to the
//! chip's own queue), each with the knob turned by hand and the frames counted.
//!
//! **The knob does not step this run forward**, because the knob is what the run measures.
//! **Swipe** the glass to go on, **tap** it to repeat a step; Enter and `r` in the monitor do the
//! same, and `q` lets the rest run unattended. Since no key is needed, the run can be logged:
//! `cargo run --release --bin ec2 | tee /tmp/ec2.log`.
//!
//! **What it measures.** Under `0x00` a turn produces no frame at all; under `0x05` a turn
//! produces one `BD 07` (clockwise) or `BD 08` (anticlockwise) per detent, so the knob the hand
//! turns is the same knob this encoder sits on. Under `0x03` no frame arrives and the volume in
//! `BD 05` falls as the knob turns: mode 1 is the volume, sent to the phone by the other chip.
//!
//! It also answers a second question in passing: whether a correctly formed `0x05` makes cover
//! art arrive is the last step here.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart, UartRx, UartTx};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info, warn};
use teetotum::step::{Prompt, Step};
use teetotum::touch::Touch;

/// What both firmwares configure, measured as a clean byte stream.
const BAUD: u32 = 921_600;
/// `magic, cmd, len16`.
const HEADER: usize = 4;
/// The longest frame either side builds, a cover art packet.
const MAX_LEN: usize = 1020;
/// How much of a frame's payload is kept for the log.
const KEEP: usize = 32;

const FROM_ESP32: u8 = 0xBD;
const FROM_S3: u8 = 0xA3;

const CMD_MEDIA_KEY: u8 = 4;
const CMD_STATUS_QUERY: u8 = 8;
const CMD_REPORT_STATE: u8 = 9;

/// Skip to the next track: a HID consumer usage id, and the only way to provoke a fresh cover
/// art offer, because the classic pushes artwork on a track change and not on a timer.
const KEY_NEXT: u8 = 0xB5;

/// A turn of the second encoder in mode 2. `iot_knob` calls this direction "right"; on this
/// board it is the **clockwise** one. A direction name is an interpretation -- log the raw
/// command byte beside it, as `src/companion.rs` does, rather than trusting the name alone.
const EVENT_CLOCKWISE: u8 = 7;
/// The other direction, `iot_knob`'s "left", which is **anticlockwise** here.
const EVENT_ANTICLOCKWISE: u8 = 8;

/// Builds the state byte: bit 0 is the enable the callbacks test first, bits 1..3 the mode.
const fn state(mode: u8, on: bool) -> u8 {
    (mode << 1) | (on as u8)
}

/// How often a wait asks the other chip how it is, so that a volume changed by hand shows up.
const STATUS_PERIOD: Duration = Duration::from_secs(1);

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();

    // The monitor needs a moment after a reset before it is attached to anything.
    delay.delay_millis(500);

    let uart = match Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(BAUD)) {
        Ok(uart) => uart
            .with_tx(peripherals.GPIO40.reborrow())
            .with_rx(peripherals.GPIO39.reborrow()),
        Err(e) => {
            error!("UART1 refused {BAUD} baud: {e:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };
    let (rx, tx) = uart.split();
    let mut link = Link::new(rx, tx);

    // A slave left mid-transfer by the last binary holds the bus and answers to the wrong
    // address; sixteen clocks and a stop cost nothing. The reasoning is in `src/bin/haptic.rs`.
    {
        let mut scl = Output::new(
            peripherals.GPIO12.reborrow(),
            Level::High,
            OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
        );
        let mut sda = Output::new(
            peripherals.GPIO11.reborrow(),
            Level::High,
            OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
        );
        for _ in 0..16 {
            scl.set_low();
            delay.delay_micros(5);
            scl.set_high();
            delay.delay_micros(5);
        }
        sda.set_low();
        delay.delay_micros(5);
        scl.set_high();
        delay.delay_micros(5);
        sda.set_high();
        delay.delay_micros(5);
    }

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11.reborrow())
    .with_scl(peripherals.GPIO12.reborrow());

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut touch = Touch::new(
        Output::new(
            peripherals.GPIO10.reborrow(),
            Level::High,
            OutputConfig::default(),
        ),
        Input::new(peripherals.GPIO9.reborrow(), pull_up),
        &delay,
    );
    match touch.chip_id(&mut i2c) {
        Ok(id) => info!("touch controller answers with id {id:#04x} (0xB6 is the CST816D here)"),
        Err(e) => warn!("touch controller does not answer: {e:?} -- steps cannot be repeated"),
    }

    // Deliberately without the knob: see the module comment.
    let mut prompt = Prompt::new().with_touch(touch).with_keys(
        UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow())
            .split()
            .0,
    );

    // Whatever the other chip was in the middle of saying when we booted.
    link.listen(Duration::from_millis(300));
    link.reset_sync();

    info!("");
    info!("--- 0. what the state byte says before we touch it ---");
    link.send(CMD_STATUS_QUERY, [0, 0, 0, 0]);
    link.listen(Duration::from_millis(500));

    // 1. The control. With bit 0 clear the callbacks return before they look at anything else,
    // so a turn must produce nothing at all -- and if it does produce something, the reading of
    // those callbacks is wrong and everything after this step is worthless.
    turn_step(
        &mut prompt,
        &mut i2c,
        &mut link,
        state(0, false),
        "control: turn the knob a few detents -- nothing should arrive",
    );

    // 2. The measurement. Mode 2 with the enable bit set is what the image says sends the turns
    // to us. Slowly and counted, because the hand hears two mechanical clicks per detent and the
    // open question is whether the wire agrees with the fingers.
    turn_step(
        &mut prompt,
        &mut i2c,
        &mut link,
        state(2, true),
        "turn the knob CLOCKWISE, five detents, slowly -- expecting BD 07, five times",
    );
    turn_step(
        &mut prompt,
        &mut i2c,
        &mut link,
        state(2, true),
        "now five detents ANTICLOCKWISE, slowly -- expecting BD 08, five times",
    );

    // 3. Mode 1 sends the same turns into the classic's own queue instead. Nothing should reach
    // us -- but the volume in `data[1]` of the status is polled throughout, so if that queue is
    // the volume control, this step says so without a single new frame type.
    turn_step(
        &mut prompt,
        &mut i2c,
        &mut link,
        state(1, true),
        "mode 1: turn the knob both ways -- watch for the volume in the status changing",
    );

    // 4. The question this bitfield reopened. Play something over Bluetooth first.
    loop {
        info!("");
        info!("--- 4. page 2, spelled correctly this time, and a fresh track ---");
        link.set_state(state(2, true));
        link.send(CMD_MEDIA_KEY, [KEY_NEXT, 0, 0, 0]);
        info!("  listening 15 s: BD 01 would be a cover art offer");
        link.listen(Duration::from_secs(15));
        if !waiting(
            &mut prompt,
            &mut i2c,
            &mut link,
            "skip a track at the phone as well, and watch for BD 01",
        ) {
            break;
        }
    }

    info!("");
    info!(
        "--- listening from now on, state left on {:#04x} ---",
        state(2, true)
    );
    loop {
        link.listen(Duration::from_secs(5));
    }
}

/// One turn-the-knob step: set the state byte, prove it took, then count what arrives.
fn turn_step(
    prompt: &mut Prompt<'_>,
    i2c: &mut I2c<'_, Blocking>,
    link: &mut Link<'_>,
    state: u8,
    what: &str,
) {
    loop {
        info!("");
        info!(
            "--- state {state:#04x} = {:#010b}: bit 0 {}, mode {} ---",
            state,
            if state & 1 == 1 { "set" } else { "clear" },
            (state >> 1) & 7,
        );
        link.set_state(state);
        link.events = [0; 2];
        let started = Instant::now();
        let repeat = waiting(prompt, i2c, link, what);
        let seconds = (Instant::now() - started).as_secs();
        info!(
            "  in {seconds} s: {} x BD 07 (clockwise), {} x BD 08 (anticlockwise)",
            link.events[0], link.events[1]
        );
        if !repeat {
            return;
        }
    }
}

/// Waits for the hand without letting the line run dry, and says whether to repeat the step.
///
/// The pause is not dead time on this link: it is exactly when the other chip talks, because it
/// talks about what the hand is doing. So the wait drains the wire and polls the glass in one
/// loop -- the reasoning `src/bin/uarttalk.rs` had to learn the hard way.
fn waiting(
    prompt: &mut Prompt<'_>,
    i2c: &mut I2c<'_, Blocking>,
    link: &mut Link<'_>,
    what: &str,
) -> bool {
    if !prompt.is_attended() {
        link.listen(Duration::from_secs(5));
        return false;
    }
    prompt.announce(what);
    loop {
        link.pump();
        link.trace_status();
        if let Some(step) = prompt.poll(i2c) {
            return step == Step::Repeat;
        }
    }
}

/// The link to the other chip: eight-byte frames out, anything up to 1020 bytes in.
struct Link<'d> {
    rx: UartRx<'d, Blocking>,
    tx: UartTx<'d, Blocking>,
    buf: [u8; HEADER + KEEP],
    seen: usize,
    want: usize,
    errors: u32,
    /// `BD 07` and `BD 08` since the current step began.
    events: [u32; 2],
    /// The last status payload seen, so that a repeated one stays out of the log.
    last_status: [u8; 2],
    /// Whether the frame being sent is the background status trace rather than a step of its own.
    tracing: bool,
    /// When the trace may ask again.
    next_probe: Instant,
}

impl<'d> Link<'d> {
    fn new(rx: UartRx<'d, Blocking>, tx: UartTx<'d, Blocking>) -> Self {
        Self {
            rx,
            tx,
            buf: [0; HEADER + KEEP],
            seen: 0,
            want: 0,
            errors: 0,
            events: [0; 2],
            last_status: [0xFF; 2],
            tracing: false,
            next_probe: Instant::now(),
        }
    }

    /// Writes the state byte and reads it straight back, because a write nobody confirms is a
    /// hope: the classic validates bits 1..3 itself and keeps the old value if it dislikes them.
    fn set_state(&mut self, state: u8) {
        self.send(CMD_REPORT_STATE, [state, 0, 0, 0]);
        self.last_status = [0xFF; 2];
        self.send(CMD_STATUS_QUERY, [0, 0, 0, 0]);
        self.listen(Duration::from_millis(300));
    }

    fn send(&mut self, cmd: u8, data: [u8; 4]) {
        let frame = [FROM_S3, cmd, 4, 0, data[0], data[1], data[2], data[3]];
        match self.tx.write(&frame) {
            Ok(_) => {
                // Without the flush the bytes sit in the FIFO, and a run that then waits for an
                // answer would be timing its own transmitter.
                let _ = self.tx.flush();
                if !self.tracing {
                    info!("  S3    -> ESP32  cmd {cmd:2}: {:02X?}", &frame[HEADER..]);
                }
            }
            Err(e) => error!("  sending cmd {cmd} failed: {e:?}"),
        }
    }

    /// Asks for the status now and then, and reports it only when it has changed.
    ///
    /// Nothing about the volume is pushed: it changes at the phone and no frame says so until
    /// somebody asks. Only changes are logged, so this costs a line per event, not per question.
    fn trace_status(&mut self) {
        let now = Instant::now();
        if now < self.next_probe {
            return;
        }
        self.next_probe = now + STATUS_PERIOD;
        self.tracing = true;
        self.send(CMD_STATUS_QUERY, [0, 0, 0, 0]);
        self.tracing = false;
    }

    /// One non-blocking pass over the receive FIFO.
    fn pump(&mut self) {
        let mut buf = [0u8; 64];
        match self.rx.read_buffered(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                for byte in &buf[..n] {
                    self.push(*byte);
                }
            }
            Err(e) => {
                self.errors += 1;
                if self.errors <= 8 || self.errors.is_multiple_of(100) {
                    warn!("  rx error #{}: {e:?}", self.errors);
                }
            }
        }
    }

    /// Reads for `how_long`, assembling whatever arrives.
    fn listen(&mut self, how_long: Duration) {
        let deadline = Instant::now() + how_long;
        // No delay between passes: the receive FIFO holds 128 bytes and fills in 1.4 ms at this
        // baud rate, so a loop that sleeps loses bytes out of the middle of a packet.
        while Instant::now() < deadline {
            self.pump();
        }
    }

    /// Throws away a half-read frame, for use after a deliberate pause in the conversation.
    fn reset_sync(&mut self) {
        self.seen = 0;
        self.want = 0;
    }

    fn push(&mut self, byte: u8) {
        if self.seen < HEADER {
            if self.seen == 0 && byte != FROM_ESP32 && byte != FROM_S3 {
                return;
            }
            self.buf[self.seen] = byte;
            self.seen += 1;
            if self.seen == HEADER {
                let len = u16::from_le_bytes([self.buf[2], self.buf[3]]) as usize;
                if len > MAX_LEN {
                    warn!("  frame claims {len} bytes, which no sender builds -- resyncing");
                    self.seen = 0;
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

    /// One finished frame: count it if it is a turn, and report it unless it repeats itself.
    fn complete(&mut self) {
        let magic = self.buf[0];
        let cmd = self.buf[1];
        let len = self.want - HEADER;
        let kept = self.seen.min(self.buf.len()).max(HEADER);
        let data = &self.buf[HEADER..kept];
        self.seen = 0;
        self.want = 0;

        if magic != FROM_ESP32 {
            return;
        }

        match cmd {
            EVENT_CLOCKWISE => {
                self.events[0] += 1;
                info!(
                    "  ESP32 ->  S3    BD 07  second encoder, clockwise     (#{})",
                    self.events[0]
                );
            }
            EVENT_ANTICLOCKWISE => {
                self.events[1] += 1;
                info!(
                    "  ESP32 ->  S3    BD 08  second encoder, anticlockwise (#{})",
                    self.events[1]
                );
            }
            5 if data.len() >= 2 => {
                // The two status bytes: `data[0]` is the state byte this run writes, with three
                // bits of the classic's own in it; `data[1]` is the volume, 0..127.
                let status = [data[0], data[1]];
                if status != self.last_status {
                    self.last_status = status;
                    info!(
                        "  ESP32 ->  S3    BD 05  state {:#04x} = {:#010b} (bit 0 {}, mode {}), volume {}",
                        status[0],
                        status[0],
                        if status[0] & 1 == 1 { "set" } else { "clear" },
                        (status[0] >> 1) & 7,
                        status[1],
                    );
                }
            }
            1 => info!("  ESP32 ->  S3    BD 01  COVER ART OFFER, {len} bytes: {data:02X?}"),
            _ => info!(
                "  ESP32 ->  S3    BD {cmd:02}  {len} bytes: {:02X?}",
                &data[..data.len().min(16)]
            ),
        }
    }
}
