//! The link to the classic ESP32 that shares this board.
//!
//! The board carries two microcontrollers, and the second one is not a peripheral to be
//! replaced: it owns the DAC, classic Bluetooth with its A2DP sink, and one of the two rotary
//! encoders. What it offers the ESP32-S3 is a serial line -- `UART_NUM_1`, **921600 baud, 8N1**,
//! TX on GPIO40 and RX on GPIO39 -- and a small vocabulary of framed commands over it.
//!
//! Everything this module knows was read out of both factory images and then
//! measured on the wire (`src/bin/uartframes.rs`, `src/bin/uarttalk.rs`, `src/bin/ec2.rs`). What
//! those runs established, and what this module turns into a driver:
//!
//! * The frame is `magic, cmd, len16, data`. **`0xA3` is us**, `0xBD` is the other chip, and
//!   every frame we send is eight bytes: four of header and four of data.
//! * `A3 08` is answered immediately with `BD 05`, two bytes: the **state byte** we ourselves
//!   wrote with `A3 09`, and the **volume, 0..127**, which the phone changes without telling
//!   anybody.
//! * That state byte is a **bitfield**, not a page number. Bit 0 enables the second encoder's
//!   callbacks at all; bits 1..3 are the mode, which is also the page the other chip believes
//!   our screen is on. With [`Mode::Events`] and the enable bit set, **every detent arrives as
//!   `BD 07` or `BD 08`** -- the second encoder becomes ours without replacing any firmware.
//! * `A3 04` carries HID consumer usage ids, so play/pause and track skipping are ours too, and
//!   a track change comes back unasked as a `BD 06` metadata frame naming the new title.
//!
//! # Two things this cannot do
//!
//! **It cannot play a sound.** No command hands over the DAC and none accepts audio; the S3's
//! own I2S pins do not reach the codec on this board. Sound from our own firmware means
//! replacing the other chip's firmware, which is a decision and not a call to this module.
//!
//! **It cannot ask for cover art.** The bulk transfer exists and is decoded here, but it is
//! pull-driven from our side and only ever starts when the other chip announces one with
//! `BD 01` -- which, across two runs with a correctly formed state byte, it never did.
//!
//! # Using it
//!
//! [`Companion::poll`] is non-blocking and returns at most one [`Event`] per call, so it belongs
//! in whatever loop already redraws the screen. It must be called often enough that the receive
//! FIFO does not overflow: 128 bytes fill in 1.4 ms at this baud rate while a cover transfer is
//! running, and in a comfortable eternity while it is not.
//!
//! ```ignore
//! let (rx, tx) = uart.split();
//! let mut companion = Companion::new(rx, tx);
//! companion.set_mode(Mode::Events, true);   // the second encoder is now ours
//! loop {
//!     while let Some(event) = companion.poll() {
//!         match event {
//!             Event::Encoder(Direction::Clockwise) => volume_up(),
//!             Event::Metadata => show(companion.metadata().title()),
//!             _ => {}
//!         }
//!     }
//! }
//! ```

use esp_hal::Blocking;
use esp_hal::uart::{UartRx, UartTx};

/// What both firmwares configure, measured as a clean byte stream.
pub const BAUD: u32 = 921_600;

/// `magic, cmd, len16`.
const HEADER: usize = 4;
/// The longest payload either side builds: a full cover art packet, four bytes of sub-header
/// and 1016 of image. The other chip caps its own sends here.
pub const MAX_PAYLOAD: usize = 1020;

/// The magic byte of a frame we send.
const FROM_S3: u8 = 0xA3;
/// The magic byte of a frame the classic ESP32 sends.
const FROM_CLASSIC: u8 = 0xBD;

// What we can say. The numbers are the other chip's dispatch table at `0x400db07e`.
const CMD_COVER_REQUEST: u8 = 1;
const CMD_COVER_COMPLETE: u8 = 2;
const CMD_QUEUE: u8 = 3;
const CMD_MEDIA_KEY: u8 = 4;
const CMD_CLEAR_BONDS: u8 = 6;
const CMD_FORGET_PEER: u8 = 7;
const CMD_STATUS_QUERY: u8 = 8;
const CMD_REPORT_STATE: u8 = 9;
const CMD_STREAM_START: u8 = 11;
const CMD_STREAM_SUSPEND: u8 = 12;

// What it says back. Its sender is our dispatch table at `0x42011a94` in the factory image.
const EVENT_COVER_BEGIN: u8 = 1;
const EVENT_COVER_PACKET: u8 = 2;
const EVENT_COVER_ABORT: u8 = 3;
const EVENT_COVER_NEED: u8 = 4;
const EVENT_STATUS: u8 = 5;
const EVENT_METADATA: u8 = 6;
const EVENT_TURN_CLOCKWISE: u8 = 7;
const EVENT_TURN_ANTICLOCKWISE: u8 = 8;

/// What `A3 03` carries: a code for the other chip's own dispatcher.
///
/// The handler accepts anything below 7 and hands it to a task with `xTaskNotify`. That task
/// is at `0x400dbdf8`, and it was read out of the factory image
/// branch by branch -- so these numbers are the whole vocabulary and not a guess
/// around the one value that had been watched working.
///
/// **Four of the six reach the phone unconditionally.** [`QueueKey::Next`],
/// [`QueueKey::Previous`] and [`QueueKey::Stop`] each sit in front of no test at all: they call
/// `send_passthrough(key, press)` at `0x400dbd5c` twice, two ticks apart, and that calls
/// `esp_avrc_ct_send_passthrough_cmd`. The volume pair is guarded -- a byte and a word in the
/// chip's own state have to agree -- and [`QueueKey::PlayPause`] is not a toggle but a state
/// machine, see there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueKey {
    /// Raises the phone's volume by 5, clamped to 127. Guarded: the chip drops it unless its
    /// stored volume is at most 126 **and** a word at `0x3ffcea30` equals 1.
    VolumeUp = 1,
    /// Lowers it by 5, floored at 0, behind the same guard.
    VolumeDown = 2,
    /// AVRCP `0x4B`, forward. No guard, and **measured working**: a swipe on
    /// the screen changed the track on the phone, and the other chip announced the new title as
    /// a `BD 06` of its own accord.
    Next = 3,
    /// AVRCP `0x4C`, backward. No guard, measured the same way and in the same run.
    Previous = 4,
    /// AVRCP `0x46` or `0x44` -- and **which one is decided by a byte we cannot see**.
    ///
    /// `0x400dbdb8` reads the AVRCP playback status at `0x3ffce91a` and sends *pause* when it
    /// is 1, *play* when it is 0 or 2, and **nothing at all** for any other value -- no key, no
    /// log line. A run that toggles a player and one that does nothing can both be this command,
    /// and the silent branch is the difference. For a command that cannot be swallowed, use
    /// [`QueueKey::Next`].
    PlayPause = 5,
    /// AVRCP `0x45`, stop. No guard.
    Stop = 6,
}

/// Where packet *n* of a cover image belongs in the buffer: at `(n - 1) * STRIDE`.
///
/// Both ends compute this identically, ours at `0x42011b49` and theirs at `0x400db204`.
pub const COVER_PACKET_STRIDE: usize = 1016;
/// The size both firmwares allocate for a whole cover image, and so the largest one that fits.
pub const COVER_MAX_BYTES: usize = 0xC000;

/// How much of a metadata string is kept.
///
/// The frames measured were 39 to 54 bytes for all three strings together, and
/// the other chip copies text in chunks of up to 80 bytes. A string longer than this is
/// truncated rather than dropped -- a title that does not fit on a 360 pixel circle anyway.
const TEXT_CAPACITY: usize = 128;

/// How many bytes are taken off the receive FIFO in one pass.
const STAGE: usize = 64;

/// Which way the knob went, seen from the front with the device flat on the table.
///
/// The names are the board's, not the library's: the other chip's `iot_knob` component calls
/// these directions left and right, and neither of its names survives the trip to a hand.
///
/// **A direction read off a single hand-turned run is not to be trusted; the raw command byte
/// has to be logged beside the name,** because a hand can turn against the instruction it
/// follows. Independent of any hand: one frame per detent, one kind per direction, no cross-talk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// `BD 07`.
    Clockwise,
    /// `BD 08`.
    Anticlockwise,
}

/// What the other chip does with a turn of the second encoder -- bits 1..3 of the state byte.
///
/// The same field is what the factory firmware used to report which page its screen was on,
/// which is why the other chip treats [`Mode::Events`] and "the user is looking at the player"
/// as the same fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Neither branch of the callbacks matches: the turn is counted and dropped.
    Idle = 0,
    /// The turn goes into the other chip's own queue and ends at the **phone's volume**, over
    /// AVRCP. Measured: no frame arrives, and the volume in the status falls.
    Volume = 1,
    /// The turn is sent to us as `BD 07` / `BD 08`. This is the mode that makes the second
    /// encoder ours.
    Events = 2,
}

impl Mode {
    fn from_bits(bits: u8) -> Option<Self> {
        match bits {
            0 => Some(Self::Idle),
            1 => Some(Self::Volume),
            2 => Some(Self::Events),
            _ => None,
        }
    }
}

/// A key press, as a HID consumer usage id -- which is what `A3 04` carries.
///
/// **These do not go out over AVRCP.** `A3 04` posts into a queue of its own (`0x400da56c`),
/// and the task draining it is named in the image: `ble_hid_task` at `0x400da514`. So a key
/// sent this way reaches the phone only while something is *connected* to the chip's BLE HID
/// device -- and a run with nothing connected saw `TAIJI_KNOB_HID` only advertising, in every
/// scan. A command sent here with no peer is dropped without a word.
///
/// **While the knob is the sound output, use [`QueueKey`] instead.** The same
/// swipe that did nothing here changed the track when it went out as [`QueueKey::Next`]: the
/// phone is already on the other chip's AVRCP link, and that is where a transport key belongs
/// as long as that link exists.
///
/// **These are the keys for the case where it does not.** A BLE HID link needs neither A2DP nor
/// AVRCP, so a phone paired with `TAIJI_KNOB_HID` can be driven from here **while it keeps
/// playing through its own speaker** -- the one situation in which [`QueueKey`] has nothing to
/// ride on. The reports are well formed: the descriptor at `0x3f41ef38` declares thirteen usages
/// as a four-bit array, and `0x400da420` writes exactly that index for each of the ten here --
/// Mute, Recall Last and Assign Selection are in the descriptor too, but their mapping was not
/// read. **None of the thirteen is a volume.** What is missing is only the pairing.
///
/// The readout is the price. Track, artist, volume and cover art all arrive over AVRCP, so a
/// knob holding only a HID link commands the player and can show nothing about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    /// `0xCD`. Measured: it both pauses and resumes.
    PlayPause = 0xCD,
    /// `0xB5`. The other chip answers with a fresh `BD 06` naming the new track -- unless the
    /// playlist has run out, in which case a working command looks exactly like a broken one.
    Next = 0xB5,
    /// `0xB6`.
    Previous = 0xB6,
    /// The rest are read in the mapping and not tried.
    Power = 0x30,
    Play = 0xB0,
    Pause = 0xB1,
    Record = 0xB2,
    FastForward = 0xB3,
    Rewind = 0xB4,
    Stop = 0xB7,
}

/// The two bytes of a `BD 05`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// The state byte, as [`Companion::set_state`] last left it -- plus three bits of the other
    /// chip's own, at positions 4, 5 and 6. Bit 6 is set on `ESP_HIDD_CONNECT_EVENT` and cleared
    /// on `ESP_HIDD_DISCONNECT_EVENT` (read in its image), bit 5 is [`Status::streaming`], and
    /// bit 4 is not read yet.
    pub state: u8,
    /// The phone's volume, 0..127, watched live while it was turned by hand.
    pub volume: u8,
}

impl Status {
    /// Whether the second encoder's callbacks look at anything at all.
    pub fn encoder_enabled(&self) -> bool {
        self.state & 1 == 1
    }

    /// Whether audio streams to the knob: bit 5, which the other chip's A2DP callback sets
    /// from the audio state and which mirrors its volume guard. **While it is clear, the other
    /// chip takes no volume step**, from us or from its own encoder.
    pub fn streaming(&self) -> bool {
        self.state & 0x20 != 0
    }

    /// Whether a phone is connected over BLE HID: bit 6, set and cleared by the other chip's
    /// `esp_hidd` handler.
    pub fn hid_connected(&self) -> bool {
        self.state & 0x40 != 0
    }
}

/// The bits of the state byte that the other chip sets itself: 4, 5 and 6.
const OWN_BITS: u8 = 0x70;

impl Status {
    /// The mode field, or `None` for a value the other chip would have rejected.
    pub fn mode(&self) -> Option<Mode> {
        Mode::from_bits((self.state >> 1) & 0x07)
    }
}

/// One string out of a `BD 06`, kept in place because this module allocates nothing.
struct Text {
    buf: [u8; TEXT_CAPACITY],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self {
            buf: [0; TEXT_CAPACITY],
            len: 0,
        }
    }

    /// Takes one NUL-terminated string out of a metadata frame, truncating at the buffer.
    fn set(&mut self, bytes: &[u8]) {
        let text = match bytes.iter().position(|&b| b == 0) {
            Some(nul) => &bytes[..nul],
            None => bytes,
        };
        self.len = text.len().min(TEXT_CAPACITY);
        self.buf[..self.len].copy_from_slice(&text[..self.len]);
    }

    /// The text, or the empty string if the other chip sent something that is not UTF-8.
    ///
    /// Nothing promises UTF-8 here; the strings come from the phone by way of AVRCP. Truncating
    /// at [`TEXT_CAPACITY`] can also cut a multi-byte character in half.
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

/// What the phone is playing, as the other chip last reported it.
///
/// A `BD 06` carries four NUL-terminated strings behind four one-byte lengths. Three of them
/// are filled in every frame seen so far and the fourth has always been empty; it is kept
/// because the frame reserves it, not because anything is known about it.
pub struct Metadata {
    title: Text,
    artist: Text,
    album: Text,
    fourth: Text,
}

impl Metadata {
    const fn new() -> Self {
        Self {
            title: Text::new(),
            artist: Text::new(),
            album: Text::new(),
            fourth: Text::new(),
        }
    }

    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    pub fn artist(&self) -> &str {
        self.artist.as_str()
    }

    pub fn album(&self) -> &str {
        self.album.as_str()
    }

    /// The slot the frame reserves and nothing has ever filled.
    pub fn fourth(&self) -> &str {
        self.fourth.as_str()
    }
}

/// One thing the other chip said.
///
/// The payload of the frame that produced an event stays available through
/// [`Companion::payload`] until the next call to [`Companion::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A detent of the second encoder, in [`Mode::Events`].
    Encoder(Direction),
    /// The answer to a status query, and also the first thing the other chip says after its own
    /// boot.
    Status(Status),
    /// A new track: [`Companion::metadata`] has been updated.
    Metadata,
    /// A cover image is on offer. Nothing arrives until we ask for packet 1 with
    /// [`Companion::request_cover_packet`]; packets are numbered from one.
    CoverBegin { id: u8, packets: u16 },
    /// One packet of that image, its bytes in [`Companion::payload`], belonging at
    /// `(packet - 1) * COVER_PACKET_STRIDE`.
    CoverPacket { id: u8, packet: u16, bytes: usize },
    /// The other chip gave up on the transfer.
    CoverAborted { reason: u8 },
    /// `BD 04`, which the factory firmware logs as "Need packet". Our own side never sends it,
    /// so the reading of its fields is inference from its neighbours.
    CoverPacketNeeded { id: u8, packet: u16 },
    /// A frame with a command byte nothing in either image explains.
    Unknown { cmd: u8, bytes: usize },
}

/// The serial link to the classic ESP32.
pub struct Companion<'d> {
    rx: UartRx<'d, Blocking>,
    tx: UartTx<'d, Blocking>,
    /// The frame being assembled, header included.
    frame: [u8; HEADER + MAX_PAYLOAD],
    /// Bytes of it seen so far.
    seen: usize,
    /// Its total length, known once the header is in.
    want: usize,
    /// Bytes taken off the FIFO and not yet parsed.
    stage: [u8; STAGE],
    stage_len: usize,
    stage_pos: usize,
    metadata: Metadata,
    status: Option<Status>,
    state: u8,
    rx_errors: u32,
    resyncs: u32,
}

impl<'d> Companion<'d> {
    /// Takes the two halves of a `UART1` configured for [`BAUD`], TX on GPIO40, RX on GPIO39.
    ///
    /// **GPIO39 is an output of the other chip** and must never be driven from here.
    pub fn new(rx: UartRx<'d, Blocking>, tx: UartTx<'d, Blocking>) -> Self {
        Self {
            rx,
            tx,
            frame: [0; HEADER + MAX_PAYLOAD],
            seen: 0,
            want: 0,
            stage: [0; STAGE],
            stage_len: 0,
            stage_pos: 0,
            metadata: Metadata::new(),
            status: None,
            state: 0,
            rx_errors: 0,
            resyncs: 0,
        }
    }

    /// Reads whatever has arrived and returns the next complete frame, if one is there.
    ///
    /// Never blocks: it returns `None` as soon as the receive FIFO is empty, so a caller that
    /// wants everything waiting calls it in a `while let`.
    pub fn poll(&mut self) -> Option<Event> {
        loop {
            if self.stage_pos == self.stage_len && !self.refill() {
                return None;
            }
            let byte = self.stage[self.stage_pos];
            self.stage_pos += 1;
            if let Some(event) = self.push(byte) {
                return Some(event);
            }
        }
    }

    /// The payload of the frame [`Companion::poll`] last returned, without its header.
    ///
    /// For a [`Event::CoverPacket`] the first four bytes are the packet's sub-header; the image
    /// bytes follow them.
    pub fn payload(&self) -> &[u8] {
        &self.frame[HEADER..self.want.max(HEADER)]
    }

    /// The last status the other chip reported, or `None` if it has not spoken yet.
    pub fn status(&self) -> Option<Status> {
        self.status
    }

    /// What the phone is playing, as of the last `BD 06`.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The state byte as this driver last wrote it.
    ///
    /// This is what *we* asked for, not what the other chip holds: it sets three bits of its own
    /// in that byte, and it keeps the previous mode if it dislikes the one we sent. The value it
    /// actually holds arrives with the next [`Event::Status`].
    pub fn requested_state(&self) -> u8 {
        self.state
    }

    /// How many read errors the UART has reported, and how many frames were thrown away for
    /// claiming an impossible length. Both stayed at zero across every measured run.
    pub fn error_counts(&self) -> (u32, u32) {
        (self.rx_errors, self.resyncs)
    }

    /// Asks for the status. The answer arrives as an [`Event::Status`], within a millisecond.
    ///
    /// Nothing about the volume is pushed -- it changes at the phone and no frame says so --
    /// so a screen that shows the volume has to ask. Once a second was enough to watch a hand
    /// turning it.
    pub fn request_status(&mut self) {
        self.send(CMD_STATUS_QUERY, [0, 0, 0, 0]);
    }

    /// Writes bits 0..3 of the state byte, and bits 4..6 back as the other chip last reported
    /// them.
    ///
    /// **`A3 09` stores the whole byte**, so writing only ours would wipe the other chip's own
    /// bits -- which it sets on events and never repeats, so they would stay wiped until the
    /// next one. The copy written back is as old as the last [`Event::Status`]: an event of
    /// the other chip's between that report and this call is lost from the byte, though not
    /// from whatever the bit mirrors.
    ///
    /// Prefer [`Companion::set_mode`], which builds our half.
    pub fn set_state(&mut self, state: u8) {
        self.state = state & !OWN_BITS;
        let own = self.status.map_or(0, |status| status.state & OWN_BITS);
        self.send(CMD_REPORT_STATE, [self.state | own, 0, 0, 0]);
    }

    /// Sets what the other chip does with the second encoder, and whether it looks at it at all.
    ///
    /// A write nobody confirms is a hope: the other chip validates the mode field itself and
    /// keeps its old value if it dislikes ours. Follow this with [`Companion::request_status`]
    /// and compare, which is exactly what the measuring runs did.
    pub fn set_mode(&mut self, mode: Mode, enabled: bool) {
        self.set_state(((mode as u8) << 1) | u8::from(enabled));
    }

    /// Sends a transport key to the phone.
    pub fn media_key(&mut self, key: MediaKey) {
        self.send(CMD_MEDIA_KEY, [key as u8, 0, 0, 0]);
    }

    /// Sends a command to the other chip's own dispatcher, which turns it into an AVRCP
    /// passthrough key.
    ///
    /// This is the path that ends at `esp_avrc_ct_send_passthrough_cmd`, and it is *not* the
    /// path [`Companion::media_key`] takes -- that one ends in a BLE HID report. It reaches that
    /// task by `xTaskNotify`, so a second command arriving before the task looks overwrites the first.
    pub fn queue_key(&mut self, key: QueueKey) {
        self.send(CMD_QUEUE, [key as u8, 0, 0, 0]);
    }

    /// Asks the other chip to flip between play and pause -- `A3 03` with 5.
    ///
    /// Kept as its own name because it reads better at a call site, but see [`QueueKey::PlayPause`]:
    /// it is the one code in the set that the chip may decide to swallow.
    pub fn toggle_playback(&mut self) {
        self.queue_key(QueueKey::PlayPause);
    }

    /// Asks the phone to start the A2DP stream -- `esp_a2d_media_ctrl(START)`.
    ///
    /// This is flow control, not a transport key: a sink may ask a source to stop, and a source
    /// is free to ignore a request to start again. That is why a suspend once paused a player
    /// and the matching start did not resume it. For a play button, use [`MediaKey::PlayPause`].
    pub fn stream_start(&mut self) {
        self.send(CMD_STREAM_START, [1, 0, 0, 0]);
    }

    /// Asks the phone to suspend the A2DP stream -- `esp_a2d_media_ctrl(SUSPEND)`.
    pub fn stream_suspend(&mut self) {
        self.send(CMD_STREAM_SUSPEND, [0, 0, 0, 0]);
    }

    /// Asks for one packet of a cover image, numbered from one.
    ///
    /// The other chip never runs ahead: each packet is sent because it was asked for, and the
    /// id from the [`Event::CoverBegin`] has to be echoed here or the request is refused as
    /// stale.
    pub fn request_cover_packet(&mut self, id: u8, packet: u16) {
        let [low, high] = packet.to_le_bytes();
        self.send(CMD_COVER_REQUEST, [id, low, high, 0]);
    }

    /// Says the image is complete, which lets the other chip drop its send state.
    pub fn cover_complete(&mut self) {
        self.send(CMD_COVER_COMPLETE, [0, 0, 0, 0]);
    }

    /// Makes the other chip forget the phone it is paired with -- the stored peer address in
    /// its NVS.
    ///
    /// Half of the factory interface's "forget everything", and **not** something to send while
    /// measuring: it takes a pairing with it, and pairing again needs the hand and the phone.
    pub fn forget_peer(&mut self) {
        self.send(CMD_FORGET_PEER, [0, 0, 0, 0]);
    }

    /// Clears every BLE bond the other chip holds. The other half of "forget everything", with
    /// the same warning.
    pub fn clear_ble_bonds(&mut self) {
        self.send(CMD_CLEAR_BONDS, [0, 0, 0, 0]);
    }

    /// Throws away a half-read frame.
    ///
    /// Worth doing once after our own boot, because the other chip has been running all along
    /// and we may have joined in the middle of a sentence.
    pub fn resync(&mut self) {
        self.seen = 0;
        self.want = 0;
    }

    /// Every frame we send is eight bytes: the header and four bytes of data.
    fn send(&mut self, cmd: u8, data: [u8; 4]) {
        let frame = [FROM_S3, cmd, 4, 0, data[0], data[1], data[2], data[3]];
        if self.tx.write(&frame).is_ok() {
            // Without the flush the bytes sit in the transmit FIFO, and a caller that then waits
            // for the answer is timing its own transmitter.
            let _ = self.tx.flush();
        }
    }

    /// One pass over the receive FIFO. `false` means nothing was waiting.
    fn refill(&mut self) -> bool {
        self.stage_pos = 0;
        self.stage_len = 0;
        match self.rx.read_buffered(&mut self.stage) {
            Ok(0) => false,
            Ok(n) => {
                self.stage_len = n;
                true
            }
            Err(_) => {
                self.rx_errors += 1;
                false
            }
        }
    }

    /// Feeds one byte into the frame being assembled.
    fn push(&mut self, byte: u8) -> Option<Event> {
        if self.seen < HEADER {
            // Anything before a magic byte is the tail of something we missed the start of.
            if self.seen == 0 && byte != FROM_CLASSIC && byte != FROM_S3 {
                return None;
            }
            self.frame[self.seen] = byte;
            self.seen += 1;
            if self.seen < HEADER {
                return None;
            }
            let len = u16::from_le_bytes([self.frame[2], self.frame[3]]) as usize;
            if len > MAX_PAYLOAD {
                // No sender builds a frame this long, so what looked like a header was payload.
                self.resyncs += 1;
                self.seen = 0;
                return None;
            }
            self.want = HEADER + len;
            return if self.want == HEADER {
                self.complete()
            } else {
                None
            };
        }

        self.frame[self.seen] = byte;
        self.seen += 1;
        if self.seen == self.want {
            self.complete()
        } else {
            None
        }
    }

    /// One finished frame. `self.want` is left alone so that [`Companion::payload`] still works.
    fn complete(&mut self) -> Option<Event> {
        self.seen = 0;
        let magic = self.frame[0];
        let cmd = self.frame[1];
        let len = self.want - HEADER;
        // Frames of our own magic can only be an echo of something on the line; they are parsed
        // to stay in step with the stream and then dropped.
        if magic != FROM_CLASSIC {
            return None;
        }
        let data = &self.frame[HEADER..HEADER + len];

        match cmd {
            EVENT_TURN_CLOCKWISE => Some(Event::Encoder(Direction::Clockwise)),
            EVENT_TURN_ANTICLOCKWISE => Some(Event::Encoder(Direction::Anticlockwise)),
            EVENT_STATUS if len >= 2 => {
                let status = Status {
                    state: data[0],
                    volume: data[1],
                };
                self.status = Some(status);
                Some(Event::Status(status))
            }
            EVENT_METADATA if len >= 4 => {
                self.take_metadata();
                Some(Event::Metadata)
            }
            EVENT_COVER_BEGIN if len >= 3 => Some(Event::CoverBegin {
                id: data[0],
                packets: u16::from_le_bytes([data[1], data[2]]),
            }),
            EVENT_COVER_PACKET if len >= 4 => Some(Event::CoverPacket {
                id: data[0],
                packet: u16::from_le_bytes([data[1], data[2]]),
                bytes: len - 4,
            }),
            EVENT_COVER_ABORT if len >= 1 => Some(Event::CoverAborted { reason: data[0] }),
            EVENT_COVER_NEED if len >= 3 => Some(Event::CoverPacketNeeded {
                id: data[0],
                packet: u16::from_le_bytes([data[1], data[2]]),
            }),
            _ => Some(Event::Unknown { cmd, bytes: len }),
        }
    }

    /// Splits a `BD 06` into its four strings.
    ///
    /// The sub-header is four one-byte lengths, each counting its string's trailing NUL, and
    /// `4 + l0 + l1 + l2 + l3 == len` held exactly in every frame measured. It is not trusted
    /// here: a length that would run past the end of the frame stops the parse, leaving the
    /// strings before it in place.
    fn take_metadata(&mut self) {
        let len = self.want - HEADER;
        let lengths = [
            self.frame[HEADER] as usize,
            self.frame[HEADER + 1] as usize,
            self.frame[HEADER + 2] as usize,
            self.frame[HEADER + 3] as usize,
        ];
        let mut at = HEADER + 4;
        for (slot, size) in lengths.iter().enumerate() {
            let end = at + size;
            if *size == 0 || end > HEADER + len {
                break;
            }
            let text = &self.frame[at..end];
            match slot {
                0 => self.metadata.title.set(text),
                1 => self.metadata.artist.set(text),
                2 => self.metadata.album.set(text),
                _ => self.metadata.fourth.set(text),
            }
            at = end;
        }
    }
}
