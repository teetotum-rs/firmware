//! Speaks to the other microcontroller for the first time, and waits to be answered.
//!
//! `src/bin/uartframes.rs` established the link by listening only: frames arrive on **GPIO39** at
//! **921600 baud, 8N1**, with the magic byte `0xBD`, exactly as both factory images were read to
//! say. Everything since has been one-directional. This run drives **GPIO40**, the S3's own
//! transmit line, and asks three questions whose answers are visible in three different places:
//!
//! 1. **`A3 08`, the status query.** The classic ESP32's dispatch table answers it immediately
//!    with `BD 05`. Nothing else on the board can produce that frame, so a `BD 05` within
//!    milliseconds of our byte is the proof that the send direction works -- read in the monitor.
//! 2. **`A3 04`, a media key.** `0xCD` is the HID consumer usage for play/pause. The answer to
//!    this one is not a frame at all: the music stops or starts, at the phone and in the
//!    headphones. It proves that the other chip *acts* on what we send, not just parses it.
//! 3. **`A3 09` then a cover art transfer.** The factory S3 reports which page its UI is on, and
//!    the listening run saw metadata arrive while cover art never did -- the suspicion being that a
//!    chip which never claims to be on page 2 is never offered a picture. So this says "page 2"
//!    and then plays the S3's half of the pull protocol: every `BD 01` is answered with `A3 01`
//!    requests until the packets are in, then `A3 02`. What comes back is not decoded, only
//!    counted and its first bytes shown -- `FF D8 FF` would be a JPEG.
//!
//! **GPIO38 is not involved.** The schematic places the link there, but it is on GPIO40/39
//! instead, and the haptics' enable line lives on GPIO38 -- so sending and buzzing do not share
//! a wire, and this run leaves the haptics alone entirely.
//!
//! A fourth step then sweeps the commands the factory S3 never sends at all, one per pause --
//! among them `A3 11` and `A3 12`, a matched on/off pair, which on this board is the most
//! promising candidate for the DAC's mute.
//!
//! While a step waits, the status query runs in the background every two seconds and only its
//! *changes* are logged -- turn the volume at the phone during a pause and the trace says which
//! byte follows it.
//!
//! The steps wait for a hand (`src/step.rs`): the play/pause answer is at the phone, and a cover
//! only exists while music with artwork is playing. Turn the knob for the next step, touch the
//! screen to repeat one, `q` in the monitor to let the rest run unattended.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart, UartRx, UartTx};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info, warn};
use teetotum::encoder::Encoder;
use teetotum::step::{Prompt, Step};
use teetotum::touch::Touch;

/// What both firmwares configure, read out of their images and confirmed by a clean byte stream.
const BAUD: u32 = 921_600;

/// `magic, cmd, len16` before every payload.
const HEADER: usize = 4;

/// The largest payload either side builds: 1016 bytes of cover art plus four of sub-header.
const MAX_LEN: usize = 1020;

/// How much of a payload is kept for the log. Long cover art packets are counted, not stored.
const KEEP: usize = 128;

/// Who is speaking.
const FROM_ESP32: u8 = 0xBD;
const FROM_S3: u8 = 0xA3;

/// The commands this run sends. Names and numbers from the dispatch table at `0x400db07e`.
const CMD_COVER_REQUEST: u8 = 1;
const CMD_COVER_DONE: u8 = 2;
const CMD_MEDIA_KEY: u8 = 4;
const CMD_STATUS_QUERY: u8 = 8;
const CMD_REPORT_STATE: u8 = 9;

/// HID consumer usage IDs, as the classic ESP32 queues them at `0x400da56c`.
const KEY_PLAY_PAUSE: u8 = 0xCD;
const KEY_NEXT: u8 = 0xB5;

/// The previous-track key, for the test of whether `0xB5` failing is about that one byte.
const KEY_PREVIOUS: u8 = 0xB6;

/// The commands the factory S3 never sends, and the ones it sends with values we have not tried.
///
/// Each row is `(cmd, data[0], what to listen or look for)`. This is the part of the vocabulary
/// that was read out of a dispatch table and has never been spoken. `11` and `12` are the reason
/// the sweep exists: they are a matched pair, on and off, calling `0x400dbedc(1)` and
/// `0x400dbedc(0)`, and **the S3's own firmware never sends either**. On a board whose DAC is
/// muted by a pin on the other chip, a pair like that is the first place to look for the switch.
const SWEEP: &[(u8, u8, &str)] = &[
    (
        CMD_MEDIA_KEY,
        KEY_PREVIOUS,
        "previous track -- does the track change this time?",
    ),
    (3, 3, "mode/transport 3 -- watch the music and the phone"),
    (3, 4, "mode/transport 4 -- watch the music and the phone"),
    (3, 5, "mode/transport 5 -- watch the music and the phone"),
    (6, 0, "command 6, meaning unread -- anything at all?"),
    (7, 0, "command 7, meaning unread -- anything at all?"),
    (
        11,
        0,
        "the pair, ON -- listen for the DAC: does silence become sound?",
    ),
    (12, 0, "the pair, OFF -- and does it go away again?"),
];

/// The state byte that says "the display is on the cover art page", reported with `A3 09`.
///
/// **Not the page number.** The classic ESP32 treats this byte as a bitfield: bit 0 is an enable
/// its own handlers test before doing anything, and the page sits in bits 1..3, validated as
/// `< 3`. Page 2 with the enable bit set is `0b0000_0101`, not the bare page number `2`.
const PAGE_COVER: u8 = 0b0000_0101;

/// How often a wait asks the other chip how it is.
const STATUS_PERIOD: Duration = Duration::from_secs(2);

/// Payload of packet *n* lands at `(n - 1) * STRIDE`; both firmwares compute it the same way.
const STRIDE: usize = 1016;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    // Nothing here allocates; the peripheral drivers this binary links do.
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();

    // The monitor needs a moment after a reset before it is attached to anything.
    delay.delay_millis(500);

    // The same two-pull probe as the listening run, repeated because this is the run that starts
    // driving one of these pins: if GPIO40 were held by something, it would have to be found
    // out before the first byte and not after it.
    info!("--- the two pins of the link, before anything is driven ---");
    probe(
        "GPIO39 (our RX, their TX)",
        peripherals.GPIO39.reborrow().into(),
        &delay,
    );
    probe(
        "GPIO40 (our TX, their RX)",
        peripherals.GPIO40.reborrow().into(),
        &delay,
    );

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
    // address. Sixteen clocks and a stop cost nothing and rule that out; the reasoning is in
    // `src/bin/haptic.rs`, where it was needed.
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

    // The screen is the only way to repeat a step here: `espflash monitor --non-interactive` has
    // no keyboard, and the knob only ever goes forwards. So it is pulsed rather than merely
    // attached, and asked who it is -- a step that cannot be repeated is worth one bus read to
    // find out about before the first question rather than after the last.
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
        Ok(id) => info!("  touch controller answers with id {id:#04x} (0xB6 is the CST816D here)"),
        Err(e) => warn!("  touch controller does not answer: {e:?} -- steps cannot be repeated"),
    }

    let mut io = Io::new(peripherals.IO_MUX);

    let mut prompt = Prompt::new()
        .with_encoder(Encoder::new(
            &mut io,
            Input::new(peripherals.GPIO8, pull_up),
            Input::new(peripherals.GPIO7, pull_up),
        ))
        .with_touch(touch)
        .with_keys(
            UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow())
                .split()
                .0,
        );

    // Whatever the other chip was in the middle of saying when we booted.
    info!("--- clearing the line ---");
    link.listen(Duration::from_millis(300));
    link.reset_sync();

    // The pair, thrown both ways, before anything else. `A3 12` was seen to pause the player on
    // its first and only try in the sweep below, and one throw of a switch is an anecdote: if
    // `A3 11` starts it again, and the two do it repeatedly, the pair is a transport control and
    // has a name. Ten throws is enough for that and short enough to sit through.
    info!("");
    info!("--- 0. the pair, alternating: every turn of the knob throws it the other way ---");
    let mut on = true;
    for _ in 0..10 {
        let cmd = if on { 11 } else { 12 };
        link.send(cmd, [0, 0, 0, 0]);
        link.listen(Duration::from_secs(1));
        let what = if on {
            "sent 11 -- does the music start?"
        } else {
            "sent 12 -- does the music stop?"
        };
        if !pumping_wait(&mut prompt, &mut i2c, &mut link, what) {
            on = !on;
        }
        if !prompt.is_attended() {
            break;
        }
    }

    // 1. The cheapest question there is, and the one that answers "does our TX reach them".
    loop {
        info!("");
        info!("--- 1. status query ---");
        link.send(CMD_STATUS_QUERY, [0, 0, 0, 0]);
        let answers = link.listen(Duration::from_millis(500));
        if answers == 0 {
            warn!("  nothing came back -- either the byte did not arrive or nobody answers it");
        }
        if !pumping_wait(
            &mut prompt,
            &mut i2c,
            &mut link,
            "read the answer: BD 05 means our side of the wire works",
        ) {
            break;
        }
    }

    // 2. The answer that is not a frame. Play something over Bluetooth first.
    loop {
        info!("");
        info!("--- 2. play/pause, sent as a media key ---");
        link.send(CMD_MEDIA_KEY, [KEY_PLAY_PAUSE, 0, 0, 0]);
        link.listen(Duration::from_millis(500));
        if !pumping_wait(
            &mut prompt,
            &mut i2c,
            &mut link,
            "listen: the music should stop, or start again",
        ) {
            break;
        }
    }

    // 3. Skipping a track is both a second key and the way to make a fresh cover art offer:
    // the classic chip pushes artwork when the track changes, not on a timer.
    loop {
        info!("");
        info!("--- 3. next track, and then page 2 ---");
        link.send(CMD_MEDIA_KEY, [KEY_NEXT, 0, 0, 0]);
        link.listen(Duration::from_millis(500));
        link.send(CMD_REPORT_STATE, [PAGE_COVER, 0, 0, 0]);
        info!(
            "  said we are on page 2 (state {PAGE_COVER:#04x}); listening 15 s for what a display would be sent"
        );
        link.listen(Duration::from_secs(15));
        if !pumping_wait(
            &mut prompt,
            &mut i2c,
            &mut link,
            "skip a track at the phone too -- BD 06 or BD 01 both prove the other chip is busy",
        ) {
            break;
        }
    }

    // 4. The unspoken half of the vocabulary, one command per pause, so that whatever a command
    // does has a hand and an ear on it while it happens.
    info!("");
    info!("--- 4. the commands the factory firmware never sends ---");
    for (cmd, arg, what) in SWEEP {
        loop {
            link.send(*cmd, [*arg, 0, 0, 0]);
            link.listen(Duration::from_secs(2));
            if !pumping_wait(&mut prompt, &mut i2c, &mut link, what) {
                break;
            }
        }
    }

    // Whatever else the link carries in normal operation. Metadata arrives on every track
    // change, and any cover art offer is still answered from here.
    info!("");
    info!("--- listening from now on; skip a track to make something happen ---");
    loop {
        link.listen(Duration::from_secs(5));
    }
}

/// Waits for the hand without letting the line run dry, and says whether to repeat the step.
///
/// [`Prompt::wait`] alone would leave nobody reading the UART while it waits, and every frame
/// the other chip sends during a pause -- a track change at the phone is exactly such a moment
/// -- would go into a 128-byte FIFO and then over the side. The pause is not dead time on this
/// link; it is when the interesting things happen, so it is spent draining the wire and polling
/// the hand in the same loop.
fn pumping_wait(
    prompt: &mut Prompt<'_>,
    i2c: &mut I2c<'_, Blocking>,
    link: &mut Link<'_>,
    what: &str,
) -> bool {
    if !prompt.is_attended() {
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

/// Reports whether something outside this chip is holding a pin.
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
        (Level::High, Level::High) => {
            info!("  {name}: driven high -- an idle transmit line looks like this")
        }
        (Level::Low, Level::Low) => info!("  {name}: driven low"),
        _ => info!("  {name}: floats, nobody drives it"),
    }
}

/// A cover art transfer in progress.
struct Cover {
    /// Echoed in every request; a stale transfer's frames are told apart by it.
    id: u8,
    /// How many packets the offer announced.
    total: u16,
    /// The packet currently asked for. Numbering starts at 1.
    asked: u16,
    /// Payload bytes accepted so far, sub-headers not counted.
    bytes: usize,
    /// The first bytes of the image, which say what format it is.
    head: [u8; 8],
    head_len: usize,
    started: Instant,
}

/// The link: both halves of the UART, the reassembler, and the S3's half of the cover protocol.
///
/// Frames are put together exactly as in `src/bin/uartframes.rs` -- anything that is not a magic
/// byte where a frame should start is dropped one byte at a time until the stream makes sense.
/// What is new here is that some frames are answered, and that the answer has to be quick: the
/// sender never runs ahead, so a cover art transfer moves at the speed of our requests.
struct Link<'d> {
    rx: UartRx<'d, Blocking>,
    tx: UartTx<'d, Blocking>,
    buf: [u8; HEADER + KEEP],
    seen: usize,
    want: usize,
    dropped: u32,
    errors: u32,
    /// Frames received since the last `listen` began.
    answered: u32,
    cover: Option<Cover>,
    /// The last status payload seen, so that a repeated one can be left out of the log.
    last_status: [u8; 4],
    /// Whether the current exchange is the background trace rather than a step of its own.
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
            dropped: 0,
            errors: 0,
            answered: 0,
            cover: None,
            last_status: [0xFF; 4],
            tracing: false,
            next_probe: Instant::now(),
        }
    }

    /// Sends one eight-byte frame, which is every frame the S3's own firmware ever sends.
    fn send(&mut self, cmd: u8, data: [u8; 4]) {
        let frame = [
            FROM_S3,
            cmd,
            data.len() as u8,
            0,
            data[0],
            data[1],
            data[2],
            data[3],
        ];
        match self.tx.write(&frame) {
            Ok(_) => {
                // Without the flush the bytes sit in the FIFO, and a run that then waits for an
                // answer would be timing its own transmitter.
                let _ = self.tx.flush();
                if self.cover.is_none() && !self.tracing {
                    info!(
                        "  S3    -> ESP32  cmd {cmd:2} ({}): {:02X?}",
                        name(FROM_S3, cmd),
                        &frame[HEADER..]
                    );
                }
            }
            Err(e) => error!("  sending cmd {cmd} failed: {e:?}"),
        }
    }

    /// Asks for the status now and then, and reports it only when it has changed.
    ///
    /// The two bytes of `BD 05` are the only state this chip offers, and nothing is pushed: a
    /// volume changed at the phone produces no frame at all until somebody asks. Naming those
    /// bytes therefore needs the question repeated *while* a hand is changing something, which
    /// is exactly what a wait is. Only changes are logged, so the trace costs one line per event
    /// rather than one per question.
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
    ///
    /// The unit both [`Link::listen`] and the waiting between steps are built out of, so that a
    /// caller with something else to do can still keep the line drained.
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

    /// Reads for `how_long`, assembling and answering whatever arrives.
    ///
    /// Returns how many frames were received, so a caller can say "nothing came back" rather
    /// than leaving silence to be interpreted.
    fn listen(&mut self, how_long: Duration) -> u32 {
        self.answered = 0;
        let deadline = Instant::now() + how_long;
        // No delay between passes. A cover art packet is 1020 bytes, the receive FIFO holds 128,
        // and at this baud rate it fills in 1.4 ms -- a loop that sleeps between reads loses
        // bytes out of the middle of a packet.
        while Instant::now() < deadline {
            self.pump();
        }
        self.answered
    }

    /// Throws away a half-read frame, for use after a deliberate pause in the conversation.
    fn reset_sync(&mut self) {
        self.seen = 0;
        self.want = 0;
        self.dropped = 0;
    }

    fn push(&mut self, byte: u8) {
        if self.seen < HEADER {
            if self.seen == 0 && byte != FROM_ESP32 && byte != FROM_S3 {
                self.dropped += 1;
                return;
            }
            self.buf[self.seen] = byte;
            self.seen += 1;
            if self.seen == HEADER {
                let len = u16::from_le_bytes([self.buf[2], self.buf[3]]) as usize;
                if len > MAX_LEN {
                    warn!("  frame claims {len} bytes, which no sender builds -- resyncing");
                    self.dropped += HEADER as u32;
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

    /// One finished frame: report it, and where the protocol expects a reply, reply.
    fn complete(&mut self) {
        let magic = self.buf[0];
        let cmd = self.buf[1];
        let len = self.want - HEADER;
        let kept = self.seen.min(self.buf.len()).max(HEADER);
        let mut data = [0u8; KEEP];
        let data_len = kept - HEADER;
        data[..data_len].copy_from_slice(&self.buf[HEADER..kept]);
        self.seen = 0;
        self.want = 0;
        self.answered += 1;

        // A status that says the same as the last one is the normal case while tracing, and
        // saying so every two seconds would bury what did change.
        let repeated_status = magic == FROM_ESP32 && cmd == 5 && data[..4] == self.last_status;
        if magic == FROM_ESP32 && cmd == 5 {
            self.last_status.copy_from_slice(&data[..4]);
        }

        // A cover art transfer is answered, not narrated: every log line during it is time the
        // receive FIFO spends filling up unattended.
        let quiet = (self.cover.is_some() && (cmd == 2 || cmd == 4)) || repeated_status;
        if !quiet {
            let who = if magic == FROM_ESP32 {
                "ESP32 ->  S3"
            } else {
                "S3    -> ESP32"
            };
            info!("  {who}  cmd {cmd:2} ({}), {len} bytes", name(magic, cmd));
            dump(&data[..data_len.min(len)]);
        }

        if magic != FROM_ESP32 {
            // Our own bytes, echoed back by nothing that should exist. Worth seeing.
            warn!(
                "  a frame with our own magic byte arrived -- is something looping the line back?"
            );
            return;
        }

        match cmd {
            // "Here comes a cover, N packets." The sender waits: nothing arrives until asked.
            1 if data_len >= 3 => {
                let id = data[0];
                let total = u16::from_le_bytes([data[1], data[2]]);
                info!(
                    "  cover art offered: id {id}, {total} packets, up to {} bytes",
                    total as usize * STRIDE
                );
                self.cover = Some(Cover {
                    id,
                    total,
                    asked: 1,
                    bytes: 0,
                    head: [0; 8],
                    head_len: 0,
                    started: Instant::now(),
                });
                self.send(CMD_COVER_REQUEST, [id, 1, 0, 0]);
            }
            // A packet. Its sub-header repeats id and number; the payload follows.
            2 if data_len >= 3 => self.cover_packet(&data, data_len, len),
            // The transfer was given up on at the other end.
            3 => {
                warn!("  cover art aborted, reason {}", data[0]);
                self.cover = None;
            }
            // "Which packet do you need" -- the request is simply repeated.
            4 => {
                if let Some(cover) = self.cover.as_ref() {
                    let (id, asked) = (cover.id, cover.asked);
                    self.send(CMD_COVER_REQUEST, [id, asked as u8, (asked >> 8) as u8, 0]);
                }
            }
            _ => {}
        }
    }

    /// One cover art packet: count it, keep the first bytes, and ask for the next.
    fn cover_packet(&mut self, data: &[u8; KEEP], data_len: usize, len: usize) {
        let Some(cover) = self.cover.as_mut() else {
            warn!("  a cover packet arrived with no transfer open");
            return;
        };
        let id = data[0];
        let packet = u16::from_le_bytes([data[1], data[2]]);
        if id != cover.id {
            warn!(
                "  packet for transfer {id}, but {} is the open one -- ignored",
                cover.id
            );
            return;
        }
        // `len` counts the four bytes of sub-header as well, so the payload is what is left.
        let payload = len.saturating_sub(HEADER);
        cover.bytes += payload;
        if packet == 1 && cover.head_len == 0 {
            let head = data_len.saturating_sub(HEADER).min(cover.head.len());
            cover.head[..head].copy_from_slice(&data[HEADER..HEADER + head]);
            cover.head_len = head;
        }

        if packet >= cover.total {
            let (bytes, total, head, head_len, took) = (
                cover.bytes,
                cover.total,
                cover.head,
                cover.head_len,
                cover.started.elapsed(),
            );
            self.cover = None;
            self.send(CMD_COVER_DONE, [0, 0, 0, 0]);
            info!(
                "  cover art complete: {bytes} bytes in {total} packets, {} ms",
                took.as_millis()
            );
            info!(
                "  first bytes: {:02X?} -- FF D8 FF is a JPEG, 89 50 4E 47 a PNG",
                &head[..head_len]
            );
        } else {
            let next = packet + 1;
            cover.asked = next;
            let (id, next) = (cover.id, next);
            self.send(CMD_COVER_REQUEST, [id, next as u8, (next >> 8) as u8, 0]);
        }
    }
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
