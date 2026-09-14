//! What the picture costs to send when the DMA reads it where it lies.
//!
//! `SpiDmaBus` copies every slice into its own DMA buffer before the transfer starts. For a
//! 253 KiB picture in external RAM that copy *is* the 7.9 ms a screen takes to read out of
//! there, and it is paid on every frame on top of the 6.5 ms the bus needs at 80 MHz. The GDMA
//! on this chip can read the external RAM itself, so the copy is avoidable --
//! [`Screen::set_path`](teetotum::screen::Screen::set_path) is the switch, and this run is
//! what says whether it is worth having.
//!
//! # What it is not free of
//!
//! The picture is written by the CPU through the data cache and read by the DMA off the
//! external bus, so **the cache has to be written back before every transfer**. That is a cost
//! the copying path does not have, and how large it is depends on how much of the picture was
//! touched since the last frame. So each path is timed three ways:
//!
//! * **still** -- nothing drawn between frames, so nothing is dirty;
//! * **part** -- one scene drawn over the standing picture, which is what a menu does;
//! * **whole** -- the picture cleared and redrawn, every line of it dirty.
//!
//! Only the `present` is timed in each; the drawing is outside the clock.
//!
//! The picture is timed upright only: a quarter turn is the panel's own, and the bytes go out
//! the same way at every orientation.
//!
//! # And then the eye
//!
//! Numbers cannot say what reached the panel. So the run ends in a loop that redraws a moving
//! hand and a frame counter, holds the path at [`Path::Direct`], and **swaps the clock every
//! two seconds** between [`SLOW`] and the panel's own 80 MHz, naming which is on the glass.
//!
//! An SPI transfer does not wait for its DMA, and at 80 MHz over four lines the bus takes
//! 40 MB/s where the external RAM gives about 32. A dry transmit FIFO sends what stood in it
//! last, which is thick bands of one colour. **At the glass: clean at 40 MHz, striped at 80.**
//! The direct path is therefore not a lever -- at the clock where it is right it saves eight
//! per cent, and at the clock where it saves half the frame it is wrong.
//!
//! The looking loop paints **every pixel** before it draws any of that: diagonal bands that
//! walk a step a frame, rather than thin strokes on black. A band left over from the frame
//! before, or from somewhere else entirely, is nearly invisible on a mostly-black scene, so a
//! sparse test scene can call two paths indistinguishable when a busy one would not.
//!
//! # Where the picture lies -- a question that turned out not to matter
//!
//! [`SKIP`] pushes the picture to the address it has in the firmware. The flash's read-only
//! data is mapped into the same address space ahead of the external RAM window, so the picture
//! is at `0x3c020000` in this run and `0x3c1a0000` in the firmware. The address does not decide
//! whether the direct path stripes: with the scene filled, the same picture stripes at both
//! addresses. Left at zero, and left in because the question is cheap to ask again.
//! Headless: `python3 tools/listen.py`, or in the monitor with
//! `cargo run --release --bin psramdma`.

#![no_std]
#![no_main]

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle};
use embedded_graphics::text::{Alignment, Text};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::time::Instant;
use esp_hal::time::Rate;
use log::{error, info};
use teetotum::framebuffer::{BYTES, Framebuffer, HEIGHT, WIDTH};
use teetotum::screen::{CLOCK, Path, Screen, ScreenPins};

/// Frames per measurement. Enough that the mean is not one outlier, short enough that the whole
/// table is over in a few seconds.
const FRAMES: u32 = 20;

/// How long each path stays on the glass in the looking loop, in frames.
const SWAP_AFTER: u32 = 20;

/// How far into the external RAM the picture is put, in bytes.
///
/// 1.5 MB, which is where the firmware's picture lies: `0x3c1a0000` against the `0x3c020000`
/// this run gets when it skips nothing. Set it to 0 to have the address this run had when it
/// measured the table above. See the module notes.
const SKIP: usize = 0;

/// The halved clock the looking loop asks its question at.
///
/// 40 MHz is 19.5 MB/s out of the bus, well under what the external RAM gives, where the full
/// 80 MHz asks for 39 MB/s and the external RAM reads at about 32. If the stripes are a bus
/// draining faster than the memory fills it, they are gone at this clock and back at the other.
const SLOW: Rate = Rate::from_mhz(40);

/// How long a frame stands in the looking loop.
///
/// The loop is not a speed test -- that is the table above -- it is a picture to look at, and a
/// counter that goes past at 130 frames a second cannot be read. Ten a second can, and a hand
/// that moves one step of twelve per frame makes a stale band obvious.
const FRAME_PERIOD: u32 = 100;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();
    delay.delay_millis(500);
    info!("--- psramdma: the picture sent without a copy in front of it ---");

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
    let mut screen = match Screen::new_skipping(
        peripherals.PSRAM,
        peripherals.SPI2,
        peripherals.DMA_CH0,
        pins,
        delay,
        SKIP,
    ) {
        Ok(screen) => screen,
        Err(err) => {
            error!("the screen did not come up: {err:?} -- stopping");
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    let started = Instant::now();
    let _ = screen.frame().clear(Rgb565::BLACK);
    info!(
        "clear: {} KiB of external RAM written in {} us",
        BYTES / 1024,
        started.elapsed().as_micros()
    );

    info!("path      still    part     whole   (us per present)");
    for direct in [false, true] {
        screen.set_path(if direct { Path::Direct } else { Path::Copied });
        // The direct path is off by default since it struck stripes in the firmware; this
        // run is what turns it on, and what says it is worth understanding.

        // Settle the panel and the cache: the first frame after a change of path pays for
        // whatever the one before it left dirty, and that is not what is being measured.
        draw_scene(screen.frame(), 0);
        let _ = screen.present();

        let still = time(&mut screen, FRAMES, |_frame, _n| {});
        let part = time(&mut screen, FRAMES, draw_scene);
        let whole = time(&mut screen, FRAMES, |frame, n| {
            let _ = frame.clear(Rgb565::BLACK);
            draw_scene(frame, n);
        });

        info!(
            "{:8}  {still:6}  {part:6}  {whole:6}",
            if direct { "direct" } else { "copied" },
        );
    }

    info!("---");
    info!("now the eye: the direct path swaps clock every two seconds");
    info!(
        "clean at {} MHz and striped at {} MHz means the bus outruns the memory",
        SLOW.as_mhz(),
        CLOCK.as_mhz()
    );

    let mut frames: u32 = 0;
    let mut slow = true;
    screen.set_path(Path::Direct);
    loop {
        if frames.is_multiple_of(SWAP_AFTER) {
            slow = !slow;
            let rate = if slow { SLOW } else { screen_clock() };
            if let Err(err) = screen.set_clock(rate) {
                error!("the bus refused {} MHz: {err:?}", rate.as_mhz());
            }
            info!("direct at {} MHz on the glass", rate.as_mhz());
        }
        draw_ground(screen.frame(), frames);
        draw_scene(screen.frame(), frames);
        if let Err(err) = screen.present() {
            error!("sending the picture failed: {err:?}");
        }
        frames = frames.wrapping_add(1);
        delay.delay_millis(FRAME_PERIOD);
    }
}

/// Runs `frames` frames, drawing with `draw` outside the clock, and returns the mean
/// microseconds one `present` took.
fn time(
    screen: &mut Screen<'static>,
    frames: u32,
    mut draw: impl FnMut(&mut Framebuffer, u32),
) -> u64 {
    let mut total = 0u64;
    for n in 0..frames {
        draw(screen.frame(), n);
        let started = Instant::now();
        let result = screen.present();
        total += started.elapsed().as_micros();
        if let Err(err) = result {
            error!("sending the picture failed: {err:?}");
            return 0;
        }
    }
    total / u64::from(frames)
}

/// Fills every pixel with diagonal bands that walk a step a frame.
///
/// This is the ground the looking loop judges on. Four colours far apart, bands wide enough to
/// see from a foot away and narrow enough that a stale band covers several of them, and a
/// diagonal so a band out of the frame before is out of step in both directions at once.
///
/// Written straight into the bytes rather than through `embedded-graphics`: this is 129 600
/// pixels a frame, and the looking loop is not a speed test but it should not crawl either.
fn draw_ground(frame: &mut Framebuffer, n: u32) {
    /// Big-endian RGB565, the order the panel wants and the framebuffer keeps.
    const BANDS: [[u8; 2]; 4] = [
        [0xF8, 0x00], // red
        [0x07, 0xE0], // green
        [0x00, 0x1F], // blue
        [0x00, 0x00], // black, so the white strokes on top stay readable
    ];
    /// How wide a band is, in pixels along the diagonal.
    const WIDE: usize = 12;

    let step = (n as usize).wrapping_mul(3);
    let bytes = frame.bytes_mut();
    for y in 0..HEIGHT {
        let row = y * WIDTH * 2;
        for x in 0..WIDTH {
            let band = BANDS[((x + y + step) / WIDE) % BANDS.len()];
            bytes[row + x * 2] = band[0];
            bytes[row + x * 2 + 1] = band[1];
        }
    }
}

/// A scene with a hand on it, so a frame that did not arrive can be told from one that did.
///
/// Fine detail on purpose: a stale band out of the external RAM shows as a piece of the
/// previous frame, and a ring and small text make that obvious where a flat fill would not.
fn draw_scene(frame: &mut Framebuffer, n: u32) {
    let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
    let white = PrimitiveStyle::with_stroke(Rgb565::WHITE, 1);
    let dim = PrimitiveStyle::with_stroke(Rgb565::CSS_DIM_GRAY, 1);

    let _ = Circle::with_center(centre, 356)
        .into_styled(dim)
        .draw(frame);
    let _ = Circle::with_center(centre, 240)
        .into_styled(white)
        .draw(frame);

    // A hand, one step of twelve per frame, drawn from a small table because this chip has no
    // floating point worth calling for twelve values.
    const SIN30: [(i32, i32); 12] = [
        (0, -1000),
        (500, -866),
        (866, -500),
        (1000, 0),
        (866, 500),
        (500, 866),
        (0, 1000),
        (-500, 866),
        (-866, 500),
        (-1000, 0),
        (-866, -500),
        (-500, -866),
    ];
    for (index, (sx, sy)) in SIN30.into_iter().enumerate() {
        let inner = if index == (n as usize) % 12 { 0 } else { 150 };
        let outer = 165;
        let from = Point::new(centre.x + sx * inner / 1000, centre.y + sy * inner / 1000);
        let to = Point::new(centre.x + sx * outer / 1000, centre.y + sy * outer / 1000);
        let style = if index == (n as usize) % 12 {
            PrimitiveStyle::with_stroke(Rgb565::CSS_ORANGE, 3)
        } else {
            white
        };
        let _ = Line::new(from, to).into_styled(style).draw(frame);
    }

    let big = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let small = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_LIGHT_GRAY);
    let mut number = [0u8; 12];
    let text = format_u32(&mut number, n);
    let _ = Text::with_alignment(
        text,
        Point::new(centre.x, centre.y - 6),
        big,
        Alignment::Center,
    )
    .draw(frame);
    let _ = Text::with_alignment(
        "psram dma",
        Point::new(centre.x, centre.y + 14),
        small,
        Alignment::Center,
    )
    .draw(frame);
}

/// `n` as decimal, in a buffer, because this has no allocator worth the name and `format!` is
/// not on the menu.
fn format_u32(buffer: &mut [u8; 12], mut n: u32) -> &str {
    let mut i = buffer.len();
    loop {
        i -= 1;
        buffer[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    core::str::from_utf8(&buffer[i..]).unwrap_or("?")
}

/// Keeps the height in use, so a panel of another size fails the build rather than the eye.
const _: () = assert!(HEIGHT == WIDTH);

/// The clock the screen came up at, named here so the looking loop can go back to it.
const fn screen_clock() -> Rate {
    CLOCK
}
