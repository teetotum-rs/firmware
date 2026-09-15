//! Watch the other chip's side of the link on the screen, because the cable can only be in one
//! place at a time.
//!
//! The classic ESP32 prints its Bluetooth log on its own UART0, and that port is only reachable
//! with the Type-C plug turned the other way round -- which takes the USB away from the S3. So
//! the two halves of the cover art question cannot be read on the same screen: the console says
//! whether the phone offered a picture, and only the S3 can say whether one ever arrived.
//!
//! This run gives the S3 half a display of its own. It counts every frame the other chip sends,
//! names the last track, and -- when a `BD 01` finally comes -- pulls the whole image packet by
//! packet the way the protocol requires, then puts its size and its first bytes on the screen.
//! Nothing here needs the monitor; the answer is legible from across the desk. A `BD 01` on the
//! screen tells apart what the console alone cannot: the console shows the cover art client
//! connecting and never failing a get, but silence there means either success or a phone that
//! returned no handle -- the two look identical from that side.
//!
//! The other chip builds its metadata request as `0xA7` -- title, artist, album, genre **and
//! cover art** -- but only when its cover art client is already connected, and it asks again on
//! every track change. So the first request after a connection never asks for a picture, and
//! skipping tracks is the way to get one asked for.
//!
//! It also **puts the cover on the screen**: the finished transfer is decoded once, scaled to the
//! panel and kept as the backdrop, so every frame after that is a 21 ms copy rather than the
//! scaler again. The green line carries the two things the run is worked by -- **which filter,
//! and how big the picture arrived**. **Cover art comes in at 200x200**, so it is scaled *up* by
//! 1.8 to fill the panel -- a different operation from any scaler judgement made on the way down.
//!
//! In the hand:
//!
//! * **turn the knob** -- next or previous track, so a fresh metadata request goes out without
//!   touching the phone;
//! * **tap the screen** -- play/pause;
//! * **Enter in the monitor** -- step the filter through the picture's own choice, nearest,
//!   bilinear and box. The keyboard rather than the screen, because the screen is what is being
//!   judged and the tap is already spoken for.
//!
//! Run it with the plug on the S3 (`cargo run --release --bin coverwatch`), then turn the plug
//! round to read the other chip's console at the same time.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Text};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info};
use teetotum::companion::{BAUD, COVER_MAX_BYTES, Companion, Event as Frame, MediaKey};
use teetotum::cover::{self, Art, Cover, NO_ANSWER, Step};
use teetotum::encoder::Encoder;
use teetotum::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use teetotum::image::Scaler;
use teetotum::screen::{Screen, ScreenPins};
use teetotum::touch::{Gesture, Taps, Touch};

/// How often the screen is asked for a finger.
const TOUCH_PERIOD: Duration = Duration::from_millis(20);
/// How often the picture is rebuilt when nothing has happened. A whole frame costs 14 ms at the
/// clock the panel was measured to take, so this is cheap enough to leave running for an hour.
const REDRAW_PERIOD: Duration = Duration::from_millis(400);
/// How often the state byte and volume are refreshed.
const STATUS_PERIOD: Duration = Duration::from_secs(5);

/// The filters to step through with the keyboard, starting with the picture's own choice.
///
/// `None` is [`Picture::scaler`] -- what the judgement at the screen gives this picture -- and
/// the other three are it overruled. Four states rather than three, because a run that can only
/// force a filter can never be asked what it would have done.
const FILTERS: [Option<Scaler>; 4] = [
    None,
    Some(Scaler::Nearest),
    Some(Scaler::Bilinear),
    Some(Scaler::Box),
];

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    // The image is 48 KiB at most and the strings on top of it are small; this is that plus
    // room to breathe. There is no radio in this run to share it with.
    esp_alloc::heap_allocator!(size: 96 * 1024);
    let delay = Delay::new();
    delay.delay_millis(500);
    info!("--- coverwatch: the other chip's frames, on the screen ---");

    let pins = ScreenPins {
        sck: peripherals.GPIO13.into(),
        sio0: peripherals.GPIO15.into(),
        sio1: peripherals.GPIO16.into(),
        sio2: peripherals.GPIO17.into(),
        sio3: peripherals.GPIO18.into(),
        cs: peripherals.GPIO14.into(),
        reset: peripherals.GPIO21.into(),
        backlight: Some(peripherals.GPIO47.into()),
    };
    let mut screen = match Screen::new(
        peripherals.PSRAM,
        peripherals.SPI2,
        peripherals.DMA_CH0,
        pins,
        delay,
    ) {
        Ok(screen) => screen,
        Err(err) => {
            error!("the screen did not come up: {err:?} -- stopping");
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    // Both halves of a cover live in the external RAM, and the screen is what owns it: the
    // bytes as they arrive, and the picture they unpack into. Nothing here goes through the
    // allocator -- a cover is 48 KiB at most and its pixels are three bytes each, which is
    // more than the internal heap has and less than a hundredth of what is spare. Without it
    // this run has nothing to do, so it says so and stops rather than counting frames.
    let Some(spare) = screen
        .take_spare()
        .filter(|spare| spare.len() > COVER_MAX_BYTES)
    else {
        error!("no external RAM to spare -- a cover could not be decoded, stopping");
        loop {
            delay.delay_millis(1000);
        }
    };
    let (jpeg, pixels) = spare.split_at_mut(COVER_MAX_BYTES);
    info!(
        "{} KiB of external RAM for the decoded picture",
        pixels.len() / 1024
    );
    let mut cover = Cover::new(jpeg);

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11.reborrow())
    .with_scl(peripherals.GPIO12.reborrow());

    let mut touch = Touch::new(
        Output::new(
            peripherals.GPIO10.reborrow(),
            Level::High,
            OutputConfig::default(),
        ),
        Input::new(
            peripherals.GPIO9.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        ),
        &delay,
    );

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    let uart = Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(BAUD))
        .expect("UART1 could not be configured");
    let (rx, tx) = uart
        .with_tx(peripherals.GPIO40)
        .with_rx(peripherals.GPIO39)
        .split();
    let mut companion = Companion::new(rx, tx);
    companion.request_status();

    // One counter per command byte the other chip can send, plus one for anything else. The
    // index is the command byte, so `counts[1]` is `BD 01` and `counts[0]` is the unknown pile.
    let mut counts = [0u32; 9];
    let mut art: Option<Art> = None;
    let mut taps = Taps::new();
    let (mut keys, _) = UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow()).split();
    let mut filter = 0usize;
    let mut last_action = String::from("waiting");
    let started = Instant::now();
    let mut next_touch = started;
    let mut next_redraw = started;
    let mut next_status = started + STATUS_PERIOD;
    let mut dirty = true;

    loop {
        // The link first and without a budget: a cover transfer is pull-driven, so the other
        // chip never runs ahead of us, but it does answer instantly and the FIFO is 128 bytes.
        while let Some(frame) = companion.poll() {
            // The transfer answers its own frames, and this run only watches: the pacing, the
            // retries and the packet numbering all live in `cover`, because a rule that is
            // only in one tool is not written down.
            if let Some(step) = cover.feed(frame, &mut companion) {
                if let Step::Offered { .. } = step {
                    art = None;
                }
                last_action = describe(step);
                dirty = true;
            }
            // One counter per command byte the other chip can send. This is the whole of what
            // is left to do here for a cover frame, and all of it for the others.
            match frame {
                Frame::CoverBegin { .. } => counts[1] += 1,
                Frame::CoverPacket { .. } => counts[2] += 1,
                Frame::CoverAborted { .. } => counts[3] += 1,
                Frame::CoverPacketNeeded { .. } => {
                    counts[4] += 1;
                    dirty = true;
                }
                Frame::Status(_) => {
                    counts[5] += 1;
                    dirty = true;
                }
                Frame::Metadata => {
                    counts[6] += 1;
                    last_action = String::from("new track");
                    dirty = true;
                }
                Frame::Encoder(_) => {
                    counts[7] += 1;
                    dirty = true;
                }
                Frame::Unknown { cmd, .. } => {
                    counts[0] += 1;
                    last_action = format!("unknown frame {cmd:#04x}");
                    dirty = true;
                }
            }
        }

        // Every request the transfer still owes goes out from here, held back by the rules
        // the module measured: the first packet, a retry after "not in sending", and one that
        // never came back at all.
        if let Some(step) = cover.tick(&mut companion) {
            last_action = describe(step);
            dirty = true;
        }

        // The picture is made **once**, and only after the last packet is in: decoding and
        // scaling a cover costs up to 587 ms, which is forty redraws' worth of silence on the
        // link. Nothing is outstanding at this point -- the transfer ended with a
        // `cover_complete` -- so the only thing that can be missed is an unsolicited metadata
        // frame, and the next track change sends another one.
        if art.is_none()
            && let Some(bytes) = cover.image()
        {
            art = cover::show(
                &mut screen,
                bytes,
                pixels,
                cover::CoverSize::Screen,
                FILTERS[filter],
            );
            last_action = match art.as_ref() {
                Some(art) => format!(
                    "{}x{} with {} in {} ms",
                    art.width,
                    art.height,
                    art.scaler.name(),
                    art.millis
                ),
                None => String::from("the cover did not decode"),
            };
            dirty = true;
        }

        // **The filter is switched from the keyboard**, because the screen is what is being
        // judged and the tap is already play/pause. Dropping the picture is the whole of it:
        // the block above makes another one, from the same bytes, with the next filter.
        if let Ok(byte) = keys.read_byte()
            && matches!(byte, b'\r' | b'\n' | b's' | b'S')
        {
            filter = (filter + 1) % FILTERS.len();
            art = None;
            last_action = match FILTERS[filter] {
                Some(scaler) => format!("filter forced to {}", scaler.name()),
                None => String::from("filter back to the picture's own"),
            };
            dirty = true;
        }

        // The knob asks the phone for another track, which is the only way to make the other
        // chip send a fresh metadata request -- and a fresh request is the only chance of a
        // cover art handle.
        let turns = encoder.poll();
        if turns != 0 {
            if turns > 0 {
                companion.media_key(MediaKey::Next);
                last_action = String::from("asked for the next track");
            } else {
                companion.media_key(MediaKey::Previous);
                last_action = String::from("asked for the previous track");
            }
            dirty = true;
        }

        // **Nothing slow runs while a packet is on its way.** The receive FIFO holds 128 bytes
        // and fills in 1.4 ms at this baud rate; a packet is 1024 bytes and a redrawn screen
        // costs 14 ms. Drawing on the way to an answer therefore eats the answer. A transfer is
        // under half a second, so the screen simply holds still for it.
        let busy = cover.busy();

        if !busy && Instant::now() >= next_touch {
            next_touch = Instant::now() + TOUCH_PERIOD;
            // **The gesture is answered on the lift, not while the finger is down.** The
            // controller reports a tap for as long as the contact lasts, so reading it raw
            // sends one play/pause every 20 ms -- ten of them for an ordinary tap, which
            // toggles back to where it started and looks like a dead screen. `Taps` holds the
            // rule so it is not relearned per binary.
            if let Ok(report) = touch.read(&mut i2c)
                && taps.feed(&report) == Some(Gesture::SingleTap)
            {
                companion.media_key(MediaKey::PlayPause);
                last_action = String::from("play/pause");
                dirty = true;
            }
        }

        if Instant::now() >= next_status {
            next_status = Instant::now() + STATUS_PERIOD;
            companion.request_status();
        }

        if !busy && (dirty || Instant::now() >= next_redraw) {
            dirty = false;
            next_redraw = Instant::now() + REDRAW_PERIOD;
            // A cover in the backdrop replaces the clear. `restore` says whether there is a
            // backdrop at all, not whether anything was ever put in it -- that is what `art`
            // is for, and without both the first frame would be uninitialised memory.
            let on_art = art.is_some() && screen.restore();
            draw(
                screen.frame(),
                &counts,
                &cover,
                art.as_ref().filter(|_| on_art),
                &companion,
                &last_action,
                started.elapsed(),
            );
            screen.present().ok();
        }
    }
}

/// A draw target that turns every pixel into a 2x2 block.
///
/// `embedded-graphics` stops at 10x20 for its built-in fonts, and 10x20 on a 360 pixel circle is
/// small print. Doubling costs one wrapper: the text is laid out in a 180x180 space and every
/// pixel it produces is stamped as a square, so the same font arrives as 20x40. It is only worth
/// it for the few lines that are meant to be read out loud from across the desk.
struct Doubled<'a>(&'a mut Framebuffer);

impl Dimensions for Doubled<'_> {
    fn bounding_box(&self) -> Rectangle {
        Rectangle::new(
            Point::zero(),
            Size::new(WIDTH as u32 / 2, HEIGHT as u32 / 2),
        )
    }
}

impl DrawTarget for Doubled<'_> {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, colour) in pixels {
            Rectangle::new(Point::new(point.x * 2, point.y * 2), Size::new(2, 2))
                .into_styled(PrimitiveStyle::with_fill(colour))
                .draw(self.0)
                .ok();
        }
        Ok(())
    }
}

/// Everything the run knows, on one round face.
///
/// **What is meant to be read back is green and twice as tall.** This run is worked with a hand
/// on the knob and the monitor on the other chip, so the numbers that decide the question are
/// spoken across the desk, not read off a serial log. Everything else -- the track, the link,
/// the last thing that happened -- stays small and grey, because it only matters once the green
/// lines have already said something surprising.
///
/// Once a cover has arrived the frame comes in with the picture already in it, and the text
/// goes on top. **The bands are dimmed to the lines that are actually there**, in full width
/// and not as boxes: the screen is round, so a box would put two more corners into the picture
/// while a band runs off the edge -- the same rule the status screen follows.
fn draw(
    frame: &mut Framebuffer,
    counts: &[u32; 9],
    cover: &Cover<'_>,
    art: Option<&Art>,
    companion: &Companion<'_>,
    last_action: &str,
    uptime: Duration,
) {
    // A cover is already in the frame, put there by `restore`. Without one the frame is stale.
    if art.is_none() {
        frame.clear(Rgb565::BLACK).ok();
    }

    let green = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_LIME_GREEN);
    let white = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let grey = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_DIM_GRAY);

    // The three lines to read out. Their coordinates are halved, because the wrapper doubles
    // everything it is given; 90 is the middle of a 360 pixel face.
    let (verdict, detail) = match cover.progress() {
        None => (String::from("no BD 01 yet"), String::new()),
        Some(state) if state.done => (
            format!("{} bytes", state.bytes),
            match art {
                // Once there is a picture, its own size is worth more than the packet count:
                // it is the number that says which way the scaling went.
                Some(art) => format!("{}x{}", art.width, art.height),
                None => format!("{} packets", state.packets),
            },
        ),
        Some(state) if state.aborted == Some(NO_ANSWER) => {
            (String::from("no answer"), format!("at {}", state.bytes))
        }
        Some(state) if state.aborted.is_some() => (
            String::from("refused"),
            format!("reason {}", state.aborted.unwrap_or(0)),
        ),
        Some(state) => (
            format!("{} of {}", state.got, state.packets),
            String::from("packets"),
        ),
    };
    let counters = format!("01:{} 02:{} 03:{}", counts[1], counts[2], counts[3]);
    // **A cover on the screen takes the middle of it back.** The three doubled lines cover the
    // heart of a 360 pixel face, which is exactly where a picture is worth looking at; once
    // there is one, the same numbers go to the small lines below and the middle stays clear.
    let big_lines = if art.is_some() {
        [("", 0); 3]
    } else {
        [
            (verdict.as_str(), 56),
            (detail.as_str(), 78),
            (counters.as_str(), 116),
        ]
    };

    // What the bytes are, once there are any -- or, once they have been through the decoder,
    // **which filter made the picture and how far it had to come**. That is the line the run
    // is worked by, so it is the one in green: the size says which way the scaling went, and
    // the star says whether the filter was chosen or pressed on it from the keyboard.
    let head = cover.received();
    let bytes_line = match (art, head.len() >= 3) {
        (Some(art), _) => Some(format!(
            "{}{}  {}x{}",
            art.scaler.name(),
            if art.forced { "*" } else { "" },
            art.width,
            art.height
        )),
        (None, true) => {
            let kind = if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
                "JPEG"
            } else if head.starts_with(&[0x89, 0x50]) {
                "PNG"
            } else {
                "unknown"
            };
            Some(format!(
                "{:02X} {:02X} {:02X}  {kind}",
                head[0], head[1], head[2]
            ))
        }
        (None, false) => None,
    };
    let track = clip(companion.metadata().title(), 28);
    let (framing, overrun) = companion.error_counts();
    let status = match companion.status() {
        Some(status) => format!(
            "state {:#04x}  volume {}  rx {framing}/{overrun}  up {} s",
            status.state,
            status.volume,
            uptime.as_secs()
        ),
        None => String::from("the other chip has not answered"),
    };
    let action = clip(last_action, 52);
    // The verdict and the counters only lose the middle of the face, not the run.
    let numbers = match art {
        Some(art) => format!("{verdict}  {counters}  {} ms", art.millis),
        None => String::new(),
    };
    let head_style = if art.is_some() { green } else { white };
    let small_lines = [
        (bytes_line.as_deref().unwrap_or(""), head_style, 268, 15, 5),
        (track.as_str(), white, 292, 15, 5),
        (numbers.as_str(), grey, 306, 8, 3),
        (status.as_str(), grey, 316, 8, 3),
        (action.as_str(), grey, 326, 8, 3),
    ];

    // The lines are laid out first and the bands dimmed to them, so a line that is not there
    // does not darken the picture behind it.
    if art.is_some() {
        let mut bands: Vec<(i32, i32)> = Vec::new();
        for (text, y) in big_lines {
            if !text.is_empty() {
                // The wrapper doubles the font and the coordinate alike: a 10x20 glyph on a
                // 15 pixel baseline arrives 30 above the line and 10 below it.
                bands.push((y * 2 - 30, y * 2 + 10));
            }
        }
        for (text, _, y, up, down) in small_lines {
            if !text.is_empty() {
                bands.push((y - up, y + down));
            }
        }
        // Overlapping bands would be dimmed twice, and twice as dark reads as a black stripe.
        bands.sort_unstable();
        let mut merged: Option<(i32, i32)> = None;
        for (top, bottom) in bands {
            merged = Some(match merged {
                Some((was_top, was_bottom)) if top <= was_bottom => {
                    (was_top, was_bottom.max(bottom))
                }
                Some((was_top, was_bottom)) => {
                    frame.dim_rows(was_top.max(0) as usize, was_bottom.max(0) as usize);
                    (top, bottom)
                }
                None => (top, bottom),
            });
        }
        if let Some((top, bottom)) = merged {
            frame.dim_rows(top.max(0) as usize, bottom.max(0) as usize);
        }
    }

    {
        let mut big = Doubled(frame);
        for (text, y) in big_lines {
            if !text.is_empty() {
                Text::with_alignment(text, Point::new(90, y), green, Alignment::Center)
                    .draw(&mut big)
                    .ok();
            }
        }
    }

    for (text, style, y, _, _) in small_lines {
        if !text.is_empty() {
            Text::with_alignment(text, Point::new(180, y), style, Alignment::Center)
                .draw(frame)
                .ok();
        }
    }
}

/// One line about what just happened to the transfer.
fn describe(step: Step) -> String {
    match step {
        Step::Offered { id, packets } => format!("offered: id {id}, {packets} packets"),
        Step::Packet { packet, of } => format!("packet {packet} of {of}"),
        Step::Complete { bytes } => format!("cover complete: {bytes} bytes"),
        Step::Refused { reason } => format!("refused, reason {reason}"),
        Step::Silent { bytes } => format!("no answer to the request, at {bytes} bytes"),
    }
}

/// As much of a string as fits on a round face, and no panic on a character boundary.
fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return String::from(text);
    }
    text.chars()
        .take(chars.saturating_sub(1))
        .chain(['~'])
        .collect()
}
