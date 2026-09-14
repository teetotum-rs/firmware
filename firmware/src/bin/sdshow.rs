//! A picture off the TF card, on the glass.
//!
//! Everything the card knows so far was printed: pins, registers, a partition table, a listing.
//! This is the first run that **uses** it. The factory demo keeps its clock backgrounds as
//! `*_360.bin` files of **259204 bytes**, which is 360x360 pixels of RGB565 with four bytes in
//! front -- exactly one screen of this panel, and exactly the shape of
//! [`Framebuffer::bytes_mut`]. So the file goes from the card into the picture with nothing in
//! between: no decoder, no scaling, no drawing.
//!
//! That makes it a test of three things at once, and each of them fails visibly:
//!
//! - **the filesystem**, because a path has to be resolved and a chain of clusters followed in
//!   the right order -- a chain read wrongly shows up as bands of picture in the wrong places,
//!   which is far easier to see than to check;
//! - **the throughput**, because the read is timed and 253 KiB is a whole screen;
//! - **the byte order**, which no file format here declares. Our framebuffer stores RGB565 high
//!   byte first because the panel wants it that way; a file written by a little-endian host may
//!   not. **Tap the glass to swap the two bytes of every pixel** and the eye decides in a
//!   second what a specification could not: one of the two orders looks like a photograph and
//!   the other looks like a fault.
//!
//! The name of the file is drawn over the picture in white, by us, in the order we know is
//! right. It is the reference: if the caption is legible and white while the picture is not, the
//! picture's bytes are the wrong way round and nothing else is.
//!
//! In the hand:
//!
//! * **turn the knob** -- the next background of the folder, re-read from the card and re-timed;
//! * **tap the glass** -- swap the byte order of the picture;
//! * **slide** -- turn the picture a quarter turn;
//! * **`r`** re-reads the same file, **`s`** swaps, **`0`** stands it upright.
//!
//! What it answers: **the picture is right as it is stored, and a tap turns it grey.** The
//! demo's backgrounds are RGB565 high byte first, which is the panel's order and this
//! firmware's, so a background costs one read and no pass over the pixels. The grey is worth a
//! sentence of its own: swapping the bytes of an RGB565 pixel does not permute the three
//! channels, it cuts across them, so neighbouring values of a photograph come out unrelated --
//! **a wrong byte order looks like fog, not like a colour error.**
//!
//! Run it in the monitor with a hand on it: `cargo run --release --bin sdshow`.

#![no_std]
#![no_main]

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::text::{Alignment, Text};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info, warn};
use teetotum::encoder::Encoder;
use teetotum::fat::Volume;
use teetotum::framebuffer::{BYTES, HEIGHT, WIDTH};
use teetotum::screen::{ORIENTATIONS, Screen, ScreenPins};
use teetotum::sd::{self, SdCard};
use teetotum::touch::{Gesture, Touch};

esp_bootloader_esp_idf::esp_app_desc!();

/// Where the factory demo keeps its full-screen backgrounds.
const FOLDER: &str = "/CLOCKBG";
/// How many of them this run will hold on to.
const MAX_PICTURES: usize = 8;
/// The longest file name kept, in bytes.
const NAME: usize = 64;
/// How often the glass is asked.
const TOUCH_PERIOD: Duration = Duration::from_millis(15);

/// One candidate file: a name in [`FOLDER`] whose size is a whole screen.
#[derive(Clone, Copy)]
struct Picture {
    name: [u8; NAME],
    length: usize,
    size: u32,
}

impl Picture {
    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.length]).unwrap_or("?")
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
    info!("--- sdshow: a picture off the card, straight into the framebuffer ---");

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

    // The card sits on its own SPI peripheral: SPI2 is the panel's, and the two run at different
    // clocks and different widths.
    let spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(sd::INIT_RATE),
    )
    .expect("the card SPI peripheral could not be configured")
    .with_sck(peripherals.GPIO4)
    .with_mosi(peripherals.GPIO3)
    .with_miso(peripherals.GPIO5);
    let cs = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());

    let card = match SdCard::new(spi, cs, delay) {
        Ok(card) => card,
        Err(err) => {
            error!("the card did not come up: {err:?} -- stopping");
            idle(delay);
        }
    };
    let mut volume = match Volume::mount(card) {
        Ok(volume) => volume,
        Err(err) => {
            error!("no filesystem on the card: {err:?} -- stopping");
            idle(delay);
        }
    };

    let mut pictures = [Picture {
        name: [0; NAME],
        length: 0,
        size: 0,
    }; MAX_PICTURES];
    let found = collect(&mut volume, &mut pictures);
    if found == 0 {
        error!("no full-screen picture in {FOLDER} -- stopping");
        idle(delay);
    }
    info!("{found} full-screen pictures in {FOLDER}");

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
        warn!("the touch controller does not answer: {err:?} -- no tap to swap");
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
    let mut swapped = false;
    show(&mut volume, &mut screen, &pictures[index], swapped);

    let mut next_touch = Instant::now();
    loop {
        let detents = encoder.poll();
        if detents != 0 {
            index = (index as i32 + detents).rem_euclid(found as i32) as usize;
            show(&mut volume, &mut screen, &pictures[index], swapped);
        }

        if let Ok(byte) = keys.read_byte() {
            match byte {
                b'r' | b'R' | b'\r' | b'\n' => {
                    show(&mut volume, &mut screen, &pictures[index], swapped)
                }
                b's' | b'S' => {
                    swapped = !swapped;
                    swap(&mut screen, swapped);
                }
                b'0' => {
                    screen.set_orientation(0);
                    present(&mut screen);
                }
                _ => {}
            }
        }

        if Instant::now() >= next_touch {
            next_touch = Instant::now() + TOUCH_PERIOD;
            match touch.read(&mut i2c) {
                Ok(report) => match report.gesture.in_picture_mount() {
                    Gesture::SingleTap => {
                        swapped = !swapped;
                        swap(&mut screen, swapped);
                    }
                    Gesture::SlideLeft | Gesture::SlideUp => {
                        turn(&mut screen, 1);
                    }
                    Gesture::SlideRight | Gesture::SlideDown => {
                        turn(&mut screen, -1);
                    }
                    _ => {}
                },
                Err(err) => warn!("the glass did not answer: {err:?}"),
            }
        }
    }
}

/// Every file in [`FOLDER`] whose size is one screen, with or without a four-byte header.
fn collect(volume: &mut Volume<'_>, into: &mut [Picture; MAX_PICTURES]) -> usize {
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
                if entry.directory || header_of(entry.size).is_none() {
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
                info!("{}: {} bytes", entry.name(), entry.size);
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

/// How many bytes come before the pixels, if this size can be a screen at all.
///
/// The demo's files are 259204 bytes for 259200 of picture. Four bytes is not enough to be a
/// format, but it is enough to be a width and a height, and the run prints them: if they read
/// as 360 and 360 the guess is the file's own.
fn header_of(size: u32) -> Option<u32> {
    match size as usize {
        n if n == BYTES => Some(0),
        n if n == BYTES + 4 => Some(4),
        _ => None,
    }
}

/// Read one picture off the card into the framebuffer and show it.
fn show(volume: &mut Volume<'_>, screen: &mut Screen<'_>, picture: &Picture, swapped: bool) {
    let mut path = [0u8; NAME + 16];
    let path = join(FOLDER, picture.name(), &mut path);

    let mut file = match volume.open(path) {
        Ok(file) => file,
        Err(err) => {
            error!("{path} could not be opened: {err:?}");
            return;
        }
    };

    let header = header_of(file.size()).unwrap_or(0);
    if header == 4 {
        let mut front = [0u8; 4];
        if let Err(err) = file.read_exact(volume, &mut front) {
            error!("{path}: the header did not come off: {err:?}");
            return;
        }
        info!(
            "{path}: header {:02x?}, which reads as {} x {}",
            front,
            u16::from_le_bytes([front[0], front[1]]),
            u16::from_le_bytes([front[2], front[3]])
        );
    }

    let began = Instant::now();
    let pixels = screen.frame().bytes_mut();
    let mut done = 0usize;
    while done < BYTES {
        match file.read(volume, &mut pixels[done..]) {
            Ok(0) => break,
            Ok(taken) => done += taken,
            Err(err) => {
                error!("{path}: the picture stopped after {done} bytes: {err:?}");
                return;
            }
        }
    }
    let elapsed = began.elapsed().as_micros().max(1);
    info!(
        "{path}: {done} bytes in {} ms, {} KiB/s",
        elapsed / 1000,
        (done as u64 * 1_000_000) / (elapsed * 1024)
    );
    if done < BYTES {
        warn!("{path}: only {done} of {BYTES} bytes -- the rest of the screen is stale");
    }

    if swapped {
        swap_pairs(screen.frame().bytes_mut());
    }
    caption(screen, picture.name(), swapped);
    present(screen);
}

/// Swap the two bytes of every pixel and show the result.
fn swap(screen: &mut Screen<'_>, swapped: bool) {
    let began = Instant::now();
    swap_pairs(screen.frame().bytes_mut());
    info!(
        "byte order {}, swapped in {} us",
        if swapped {
            "little-endian file"
        } else {
            "big-endian file, as the panel wants"
        },
        began.elapsed().as_micros()
    );
    present(screen);
}

/// High byte and low byte of each pixel exchanged, in place.
fn swap_pairs(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(2) {
        pixel.swap(0, 1);
    }
}

/// Turn the picture by `by` quarter turns and show it.
fn turn(screen: &mut Screen<'_>, by: i32) {
    let turned = screen.orientation() as i32 + by;
    screen.set_orientation(turned.rem_euclid(ORIENTATIONS as i32) as usize);
    info!("orientation {:3} deg", screen.orientation() * 90);
    present(screen);
}

/// The file's name across the bottom of the picture, in the byte order we know is right.
///
/// It is not decoration. The caption is drawn through `embedded-graphics`, which writes RGB565
/// the way the panel wants it, so white letters on a picture whose colours look wrong say the
/// fault is in the file's order and not in the panel, the bus or the blit.
fn caption(screen: &mut Screen<'_>, name: &str, swapped: bool) {
    let style = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);
    Text::with_alignment(
        name,
        Point::new(WIDTH as i32 / 2, HEIGHT as i32 - 24),
        style,
        Alignment::Center,
    )
    .draw(screen.frame())
    .ok();
    Text::with_alignment(
        if swapped { "swapped" } else { "as stored" },
        Point::new(WIDTH as i32 / 2, HEIGHT as i32 - 12),
        style,
        Alignment::Center,
    )
    .draw(screen.frame())
    .ok();
}

/// Send the picture, and time it, because it is the other half of what a background costs.
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
