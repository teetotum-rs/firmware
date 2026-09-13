//! Pictures larger than the screen, on the glass, through three different scalers.
//!
//! `src/bin/sdshow.rs` put a picture on the panel by reading it straight into the framebuffer,
//! which worked because the factory demo stores its backgrounds in exactly the panel's shape.
//! Nothing that comes from outside the box is stored that way -- a plugin's artwork, a
//! photograph, cover art from a phone, anything a person puts on a card, arrives at whatever
//! size its source chose. So a picture from outside needs two things this firmware did not have
//! -- a decoder, and a way down to 360 pixels.
//!
//! # What it is here to settle
//!
//! **Which scaler a picture coming down to 360 pixels wants.** The rotating blit came out for
//! [`Scaler::Nearest`]: at 30 degrees, bilinear was softer *and* visibly darker, because a
//! one-pixel white line that falls between two output pixels goes to both at half brightness.
//! That judgement was about a picture staying the same size. Coming **down** the question turns
//! over: nearest now throws away most of the source, and the third option -- averaging every
//! source pixel an output pixel covers -- reads all of it.
//!
//! # Why the card cannot answer it
//!
//! The nineteen JPEGs in the factory demo's `/PIC` are all **360x360**, cut for this panel. At
//! 1:1 each output pixel lands on exactly one source pixel, so nearest, bilinear and box are not
//! three answers but one, the same way tapping changed nothing at 90 degrees in
//! `src/bin/turn.rs`. It is a free proof that the arithmetic sits on the grid, and it is not a
//! judgement about scaling.
//!
//! So the pictures that decide the question are compiled in ([`BUILTIN`]), and the card is
//! optional. Two motifs at two reductions: a **zone plate**, whose local frequency climbs past
//! what 360 pixels can carry, so a scaler that reads the whole source goes grey where one that
//! samples it invents rings; and a **fine** pattern of one-pixel rings, spokes, a frequency
//! sweep and text down to small sizes -- the artwork case. 1024 comes down by 2.8, 480 by 1.33,
//! which is nearer the size a phone hands over. `tools/scaler-pictures.py` makes them again.
//!
//! # In the hand
//!
//! * **turn the knob** -- the next picture: the four compiled-in ones first, then the card's;
//! * **tap the glass** -- the next scaler, on the picture already decoded;
//! * **slide** -- turn the picture 30 degrees, which is the rotating blit on top of a scaled
//!   picture: two samplings, one after the other, and the second one is the one already
//!   decided;
//! * **`s`** next scaler, **`r`** read and decode again, **`0`** upright.
//!
//! The green line reads `SOURCE>360 scaler`. Both halves matter: a scaler name over a picture
//! that is already 360 wide is the run that proved nothing.
//!
//! Run it in the monitor with a hand on it: `cargo run --release --bin jpegshow`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Text};
use esp_alloc::{HeapRegion, MemoryCapability};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use teetotum::encoder::Encoder;
use teetotum::fat::Volume;
use teetotum::framebuffer::{HEIGHT, WIDTH};
use teetotum::image::{self, Picture, Scaler};
use teetotum::rotate::STEPS;
use teetotum::screen::{Screen, ScreenPins};
use teetotum::sd::{self, SdCard};
use teetotum::touch::{Gesture, Taps, Touch};
use log::{error, info, warn};

esp_bootloader_esp_idf::esp_app_desc!();

/// Where the factory demo keeps its photographs.
const FOLDER: &str = "/PIC";
/// How many of them this run will hold on to.
const MAX_PICTURES: usize = 32;
/// The longest file name kept, in bytes.
const NAME: usize = 64;
/// How often the glass is asked.
const TOUCH_PERIOD: Duration = Duration::from_millis(15);

/// Pictures compiled into the firmware, ahead of whatever the card holds.
///
/// The card's own `/PIC` is no use for this question: all nineteen of its JPEGs are 360x360,
/// because the factory demo cut them for this panel. At 360x360 every output pixel lands on
/// exactly one source pixel and the three scalers are the same arithmetic -- which is worth
/// knowing, and was worth seeing, but it is not a judgement. These four are larger on purpose,
/// two motifs at two reductions; `tools/scaler-pictures.py` makes them again.
const BUILTIN: &[(&str, &[u8])] = &[
    (
        "zone1024",
        include_bytes!("../../assets/scaler/zone1024.jpg"),
    ),
    (
        "fine1024",
        include_bytes!("../../assets/scaler/fine1024.jpg"),
    ),
    ("zone480", include_bytes!("../../assets/scaler/zone480.jpg")),
    ("fine480", include_bytes!("../../assets/scaler/fine480.jpg")),
];

/// One file in [`FOLDER`], as the listing found it.
#[derive(Clone, Copy)]
struct Entry {
    name: [u8; NAME],
    length: usize,
    size: u32,
}

impl Entry {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.length]).unwrap_or("?")
    }
}

/// A picture that has been decoded, kept so the scalers can be compared without decoding again.
struct Decoded {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
    /// What the file was called, for the caption.
    name: heapless_name::Name,
    /// How long the decode took, in microseconds.
    micros: u64,
    /// How big the file was.
    bytes: usize,
}

/// A name kept by value, because the file it came from is closed by the time it is drawn.
mod heapless_name {
    use super::NAME;

    /// Up to [`NAME`] bytes of file name.
    #[derive(Clone, Copy)]
    pub struct Name {
        bytes: [u8; NAME],
        length: usize,
    }

    impl Name {
        /// As much of `text` as fits.
        pub fn new(text: &str) -> Self {
            let source = text.as_bytes();
            let length = source.len().min(NAME);
            let mut bytes = [0; NAME];
            bytes[..length].copy_from_slice(&source[..length]);
            Self { bytes, length }
        }

        /// What it says.
        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("?")
        }
    }
}

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();
    delay.delay_millis(500);
    info!("--- jpegshow: a JPEG off the card, decoded and fitted to the glass ---");

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
            idle(delay);
        }
    };

    // The two screens in the external RAM are the panel's; everything behind them becomes heap.
    // It goes in *after* the internal region, and `esp_alloc` walks the regions in the order it
    // was given them, so a small allocation still lands in the fast RAM and only what does not
    // fit there reaches the slow bus. A decoded 500x500 photograph is 750 KB and could not live
    // anywhere else on this board.
    match screen.take_spare() {
        Some(spare) => {
            let (start, size) = (spare.as_mut_ptr(), spare.len());
            info!("external RAM behind the pictures: {size} bytes, given to the allocator");
            // SAFETY: `take_spare` hands this out once, it is `'static`, and nothing else holds
            // a reference into it.
            unsafe {
                esp_alloc::HEAP.add_region(HeapRegion::new(
                    start,
                    size,
                    MemoryCapability::External.into(),
                ));
            }
        }
        None => warn!("no external RAM behind the pictures -- only small files will decode"),
    }

    // The card sits on its own SPI peripheral: SPI2 is the panel's.
    let spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(sd::INIT_RATE),
    )
    .expect("the card SPI peripheral could not be configured")
    .with_sck(peripherals.GPIO4)
    .with_mosi(peripherals.GPIO3)
    .with_miso(peripherals.GPIO5);
    let cs = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());

    // The card only adds pictures here; the four that decide the question are in flash, so a
    // card that is missing or unreadable costs this run some photographs and none of its point.
    let mut volume = match SdCard::new(spi, cs, delay) {
        Ok(card) => match Volume::mount(card) {
            Ok(volume) => Some(volume),
            Err(err) => {
                warn!("no filesystem on the card: {err:?} -- the compiled-in pictures only");
                None
            }
        },
        Err(err) => {
            warn!("the card did not come up: {err:?} -- the compiled-in pictures only");
            None
        }
    };

    let mut entries = [Entry {
        name: [0; NAME],
        length: 0,
        size: 0,
    }; MAX_PICTURES];
    let found = match volume.as_mut() {
        Some(volume) => collect(volume, &mut entries),
        None => 0,
    };
    let total = BUILTIN.len() + found;
    info!(
        "{total} pictures: {} compiled in, {found} in {FOLDER}",
        BUILTIN.len()
    );

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
    if let Err(err) = touch.chip_id(&mut i2c) {
        warn!("the touch controller does not answer: {err:?} -- no tap to change the scaler");
    }
    let (mut keys, _) = UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow()).split();
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    let mut index = 0usize;
    let mut scaler = 0usize;
    let mut decoded = None;
    swap_in(&mut decoded, volume.as_mut(), &entries[..found], index);
    show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);

    let mut next_touch = Instant::now();
    let mut taps = Taps::new();
    loop {
        let detents = encoder.poll();
        if detents != 0 {
            index = (index as i32 + detents).rem_euclid(total as i32) as usize;
            swap_in(&mut decoded, volume.as_mut(), &entries[..found], index);
            show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
        }

        if let Ok(byte) = keys.read_byte() {
            match byte {
                b's' | b'S' | b'\r' | b'\n' => {
                    scaler = (scaler + 1) % Scaler::ALL.len();
                    show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
                }
                b'r' | b'R' => {
                    swap_in(&mut decoded, volume.as_mut(), &entries[..found], index);
                    show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
                }
                b'0' => {
                    screen.set_orientation(0);
                    show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
                }
                _ => {}
            }
        }

        if Instant::now() >= next_touch {
            next_touch = Instant::now() + TOUCH_PERIOD;
            match touch.read(&mut i2c) {
                // Through `Taps`, so that one tap is one step. Reading `report.gesture` here
                // stepped the scaler twice per tap and hid nearest behind bilinear.
                Ok(report) => match taps
                    .feed(&report)
                    .unwrap_or(Gesture::None)
                    .in_picture_mount()
                {
                    Gesture::SingleTap => {
                        scaler = (scaler + 1) % Scaler::ALL.len();
                        show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
                    }
                    Gesture::SlideLeft | Gesture::SlideUp => {
                        turn(&mut screen, 1);
                        show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
                    }
                    Gesture::SlideRight | Gesture::SlideDown => {
                        turn(&mut screen, -1);
                        show(&mut screen, decoded.as_ref(), Scaler::ALL[scaler]);
                    }
                    _ => {}
                },
                Err(err) => warn!("the glass did not answer: {err:?}"),
            }
        }
    }
}

/// Every JPEG in [`FOLDER`], up to [`MAX_PICTURES`] of them.
fn collect(volume: &mut Volume<'_>, into: &mut [Entry; MAX_PICTURES]) -> usize {
    let dir = match volume.dir(FOLDER) {
        Ok(dir) => dir,
        Err(err) => {
            error!("{FOLDER} could not be opened: {err:?}");
            return 0;
        }
    };
    let mut found = 0;
    let mut entries = volume.entries(dir);
    loop {
        match entries.next(volume) {
            Ok(Some(entry)) => {
                if entry.directory || !is_jpeg(entry.name()) {
                    continue;
                }
                let name = entry.name().as_bytes();
                if name.len() > NAME {
                    warn!("{} has a name too long to keep", entry.name());
                    continue;
                }
                into[found].name[..name.len()].copy_from_slice(name);
                into[found].length = name.len();
                into[found].size = entry.size;
                found += 1;
                if found == MAX_PICTURES {
                    return found;
                }
            }
            Ok(None) => return found,
            Err(err) => {
                warn!("{FOLDER} could not be walked to the end: {err:?}");
                return found;
            }
        }
    }
}

/// Whether a name ends in one of the two spellings.
fn is_jpeg(name: &str) -> bool {
    let lower = name.as_bytes();
    let ends_with = |suffix: &[u8]| {
        lower.len() > suffix.len()
            && lower[lower.len() - suffix.len()..]
                .iter()
                .zip(suffix)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    };
    ends_with(b".jpg") || ends_with(b".jpeg")
}

/// One picture from the card, read and decoded.
fn load(volume: &mut Volume<'_>, entry: &Entry) -> Option<Decoded> {
    let mut buffer = [0u8; NAME + 16];
    let path = join(FOLDER, entry.name(), &mut buffer);

    let mut file = match volume.open(path) {
        Ok(file) => file,
        Err(err) => {
            error!("{path} could not be opened: {err:?}");
            return None;
        }
    };

    let began = Instant::now();
    let mut jpeg = vec![0u8; file.size() as usize];
    let mut done = 0usize;
    while done < jpeg.len() {
        match file.read(volume, &mut jpeg[done..]) {
            Ok(0) => break,
            Ok(taken) => done += taken,
            Err(err) => {
                error!("{path}: the file stopped after {done} bytes: {err:?}");
                return None;
            }
        }
    }
    let read_micros = began.elapsed().as_micros().max(1);
    jpeg.truncate(done);

    decode_into(&jpeg, entry.name(), read_micros)
}

/// Load the picture at `index` into `slot`, dropping what was in it first.
///
/// The order is the whole of this function. A 1024x1024 JPEG unpacks to 3 MiB, and evaluating
/// `slot = pick(..)` holds the old picture alive while the new one is asked for -- two of them
/// at once, out of the 7.4 MiB behind the framebuffers. That fits arithmetically and stops
/// fitting once the region has been cut up and handed back a few times.
fn swap_in(
    slot: &mut Option<Decoded>,
    volume: Option<&mut Volume<'_>>,
    entries: &[Entry],
    index: usize,
) {
    *slot = None;
    *slot = pick(volume, entries, index);
}

/// The picture at `index`: the compiled-in ones first, the card's after them.
///
/// The card is allowed to be absent. It carries the nineteen photographs, which are worth
/// looking at, but not the four that make the scalers differ -- so a missing card costs this
/// run some pictures and none of its purpose.
fn pick(volume: Option<&mut Volume<'_>>, entries: &[Entry], index: usize) -> Option<Decoded> {
    if let Some(&(name, jpeg)) = BUILTIN.get(index) {
        return decode_into(jpeg, name, 0);
    }
    let entry = entries.get(index - BUILTIN.len())?;
    load(volume?, entry)
}

/// Decode `jpeg`, wherever its bytes came from.
///
/// The decoder is asked twice: once with no buffer, which fails with the size it wants, and
/// once with a buffer that size. `read_micros` is nought for a picture that was already in
/// flash, because there was nothing to read.
fn decode_into(jpeg: &[u8], name: &str, read_micros: u64) -> Option<Decoded> {
    let needed = match image::decode(jpeg, &mut []) {
        Err(image::Error::NoRoom { needed, .. }) => needed,
        Err(err) => {
            error!("{name}: the headers did not read: {err:?}");
            return None;
        }
        // An empty buffer cannot hold a picture, so this cannot happen -- but a decoder that
        // says it did is not one to argue with.
        Ok(_) => return None,
    };

    // Before the allocation, not after: if it is the one that fails, this line is the last
    // thing in the log and it says by how much.
    info!(
        "{name}: {needed} bytes wanted, {} free, {} in use",
        esp_alloc::HEAP.free(),
        esp_alloc::HEAP.used()
    );
    let began = Instant::now();
    let mut rgb = vec![0u8; needed];
    let (width, height) = match image::decode(jpeg, &mut rgb) {
        Ok(picture) => (picture.width, picture.height),
        Err(err) => {
            error!("{name}: it did not decode: {err:?}");
            return None;
        }
    };
    let micros = began.elapsed().as_micros().max(1);
    info!(
        "{name}: {} bytes read in {} ms, {width}x{height} decoded into {needed} bytes in {} ms",
        jpeg.len(),
        read_micros / 1000,
        micros / 1000
    );

    Some(Decoded {
        width,
        height,
        rgb,
        name: heapless_name::Name::new(name),
        micros,
        bytes: jpeg.len(),
    })
}

/// Fit the decoded picture to the screen with `scaler`, caption it, and send it.
fn show(screen: &mut Screen<'_>, decoded: Option<&Decoded>, scaler: Scaler) {
    screen.frame().clear(Rgb565::BLACK).ok();
    let Some(decoded) = decoded else {
        caption(screen, "no picture", "", scaler.name());
        present(screen);
        return;
    };
    let Some(picture) = Picture::new(decoded.width, decoded.height, &decoded.rgb) else {
        caption(screen, decoded.name.as_str(), "short buffer", scaler.name());
        present(screen);
        return;
    };

    let began = Instant::now();
    let fit = picture.draw(screen.frame(), scaler);
    let micros = began.elapsed().as_micros().max(1);
    info!(
        "{}: {}x{} -> {}x{} with {} in {} ms",
        decoded.name.as_str(),
        decoded.width,
        decoded.height,
        fit.width,
        fit.height,
        scaler.name(),
        micros / 1000
    );

    let detail = format!(
        "{} KiB  decode {} ms  fit {} ms",
        decoded.bytes / 1024,
        decoded.micros / 1000,
        micros / 1000
    );
    // The source size sits in the large green line next to the scaler, because it is the other
    // half of the reading: at 360x360 the three scalers are the same arithmetic, and a run that
    // hid that number in the small white line was mistaken for a judgement once already.
    let headline = format!("{}>{} {}", decoded.width, fit.width, scaler.name());
    caption(screen, decoded.name.as_str(), &detail, &headline);
    present(screen);
}

/// The name and the numbers small and white, the `headline` large and green, over a dimmed band.
///
/// What has to be readable from where the knob is goes in the headline at twice the height and
/// in the colour that carries across a desk: which scaler is on, and what it is scaling from.
/// Those two together are the reading -- a scaler name over a picture that is already 360x360
/// says nothing, and the run that learnt this the hard way had the size in the small line. The
/// rest is for the terminal really, and is here so that a photograph of the glass says which
/// run made it.
fn caption(screen: &mut Screen<'_>, name: &str, detail: &str, headline: &str) {
    const BAND: usize = 46;
    let top = HEIGHT - BAND - 24;
    screen.frame().dim_rows(top, top + BAND);

    let small = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
    let large = MonoTextStyle::new(&FONT_10X20, Rgb565::GREEN);
    let middle = WIDTH as i32 / 2;
    Text::with_alignment(
        name,
        Point::new(middle, top as i32 + 12),
        small,
        Alignment::Center,
    )
    .draw(screen.frame())
    .ok();
    Text::with_alignment(
        detail,
        Point::new(middle, top as i32 + 22),
        small,
        Alignment::Center,
    )
    .draw(screen.frame())
    .ok();
    Text::with_alignment(
        headline,
        Point::new(middle, top as i32 + 42),
        large,
        Alignment::Center,
    )
    .draw(screen.frame())
    .ok();
}

/// Turn the picture by `by` detents of 30 degrees.
fn turn(screen: &mut Screen<'_>, by: i32) {
    let turned = screen.orientation() as i32 + by;
    screen.set_orientation(turned.rem_euclid(STEPS as i32) as usize);
    info!("orientation {:3} deg", screen.orientation() * 30);
}

/// Send the picture, and time it: a turned picture costs three times an upright one.
fn present(screen: &mut Screen<'_>) {
    let began = Instant::now();
    match screen.present() {
        Ok(()) => info!("shown in {} us", began.elapsed().as_micros()),
        Err(err) => error!("sending the picture failed: {err:?}"),
    }
}

/// `folder` and `name` with a separator between them, in a buffer the caller owns.
fn join<'a>(folder: &str, name: &str, buffer: &'a mut [u8]) -> &'a str {
    let mut length = 0;
    for part in [folder.as_bytes(), b"/", name.as_bytes()] {
        let take = part.len().min(buffer.len() - length);
        buffer[length..length + take].copy_from_slice(&part[..take]);
        length += take;
    }
    core::str::from_utf8(&buffer[..length]).unwrap_or("?")
}

/// Nothing else to do; keep the log readable rather than rebooting into it again.
fn idle(delay: Delay) -> ! {
    loop {
        delay.delay_millis(1000);
    }
}
