//! The two filters on the same glass, one tap apart -- and the two ways of turning it, one
//! double-tap apart.
//!
//! `src/bin/render.rs` timed the rotation and showed it, but it showed nearest and bilinear
//! minutes and several prompts apart -- and a difference in sharpness cannot be seen that way.
//! This run puts the comparison where it belongs: **one picture, one angle, and a tap that
//! switches the filter under it.** Whatever the eye does not catch in that instant is not worth
//! four times the arithmetic.
//!
//! It is also the check on the free quarter turns. Four of the twelve detents are three bits in
//! the panel controller and cost nothing; which three bits is worked out on paper and can be
//! gotten backwards there. So **Enter in the monitor** switches the free path off and lets the
//! arithmetic draw the same angle. **At 0, 90, 180 and 270 degrees the picture must not move
//! when it is switched** -- if it jumps, the table in `src/screen.rs` is wrong, and the frame
//! time in the log says which path ran.
//!
//! The switch is Enter and not a double-tap: a control that only sometimes arrives is worse
//! than no control, because it makes the thing being judged look wrong.
//!
//! It is also the first thing here that behaves like the device is meant to behave:
//!
//! * **turn the knob** -- the picture turns with it, 30 degrees per detent, in the direction
//!   the knob went;
//! * **tap the glass** -- nearest and bilinear swap places, everything else held still;
//! * **Enter in the monitor** -- the free quarter turns off and on;
//! * **swipe the glass, or `0`** -- back to upright.
//!
//! Every redraw prints its own frame time, so the numbers accumulate while the judging happens.
//! Run it in the monitor and keep a hand on it: `cargo run --release --bin turn`.

#![no_std]
#![no_main]

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle, Rectangle};
use embedded_graphics::text::{Alignment, Text};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info, warn};
use teetotum::encoder::Encoder;
use teetotum::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use teetotum::rotate::{Filter, STEPS};
use teetotum::screen::{Screen, ScreenPins};
use teetotum::touch::{Gesture, Touch};

/// How often the glass is sampled.
const TOUCH_PERIOD: Duration = Duration::from_millis(20);

/// Where the two words that name the current path are written, in picture coordinates.
///
/// The label is part of the picture rather than something overlaid on the way out, so it is
/// turned and filtered like everything else -- which is the point: the words say what is
/// running *and* are drawn by it, so how cleanly they read is part of the answer.
const LABEL: Rectangle = Rectangle::new(Point::new(60, 232), Size::new(240, 46));

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();
    delay.delay_millis(500);
    info!("--- turn: two filters and two ways round, one finger apart ---");

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
    if let Err(e) = touch.chip_id(&mut i2c) {
        warn!("touch controller does not answer: {e:?} -- the filter cannot be switched");
    }

    // Enter arrives here. The filter switch stays on the touch tap, which reports reliably; the
    // path switch is a key because a comparison whose control is unreliable measures the
    // control instead of what it is meant to switch.
    let (mut keys, _) = UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow()).split();

    // GPIO8 is the clockwise direction, measured against a dot on the glass.
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    screen.frame().clear(Rgb565::BLACK).ok();
    draw_scene(screen.frame());
    label(&mut screen);

    info!(
        "turn the knob to rotate, tap to switch filter, Enter for the free quarters, swipe or 0 for upright"
    );
    show(&mut screen);

    let mut next_touch = Instant::now();
    loop {
        let mut changed = false;

        let detents = encoder.poll();
        if detents != 0 {
            // The knob's clockwise is the picture's clockwise: both are the direction
            // `rotate_rows` counts in, so the picture follows the hand rather than opposing it.
            let turned = screen.orientation() as i32 + detents;
            screen.set_orientation(turned.rem_euclid(STEPS as i32) as usize);
            changed = true;
        }

        if let Ok(byte) = keys.read_byte() {
            match byte {
                b'\r' | b'\n' | b' ' => {
                    screen.set_quarters(!screen.quarters());
                    changed = true;
                }
                b'0' => {
                    screen.set_orientation(0);
                    changed = true;
                }
                b'f' | b'F' => {
                    screen.set_filter(match screen.filter() {
                        Filter::Nearest => Filter::Bilinear,
                        Filter::Bilinear => Filter::Nearest,
                    });
                    changed = true;
                }
                _ => {}
            }
        }

        if Instant::now() >= next_touch {
            next_touch = Instant::now() + TOUCH_PERIOD;
            match touch.read(&mut i2c) {
                Ok(report) => match report.gesture {
                    Gesture::SingleTap => {
                        screen.set_filter(match screen.filter() {
                            Filter::Nearest => Filter::Bilinear,
                            Filter::Bilinear => Filter::Nearest,
                        });
                        changed = true;
                    }
                    Gesture::SlideLeft
                    | Gesture::SlideRight
                    | Gesture::SlideUp
                    | Gesture::SlideDown => {
                        screen.set_orientation(0);
                        changed = true;
                    }
                    _ => {}
                },
                Err(e) => warn!("the glass did not answer: {e:?}"),
            }
        }

        if changed {
            label(&mut screen);
            show(&mut screen);
        }
    }
}

/// Writes what is about to run into the picture, over whatever was there before.
///
/// Only the label's own rectangle is touched. Redrawing the whole scene would cost 6.5 ms and
/// change nothing else on the screen, and the point of a framebuffer is that it does not have
/// to be rebuilt to be changed.
fn label(screen: &mut Screen<'_>) {
    let free = screen.free();
    let filter = screen.filter();
    let quarters = screen.quarters();
    let frame = screen.frame();

    frame.fill_solid(&LABEL, Rgb565::BLACK).ok();
    let big = MonoTextStyle::new(&FONT_10X20, Rgb565::CSS_ORANGE);
    let small = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_LIGHT_GRAY);
    let _ = Text::with_alignment(
        if free {
            "madctl"
        } else if filter == Filter::Nearest {
            "nearest"
        } else {
            "bilinear"
        },
        Point::new(WIDTH as i32 / 2, 250),
        big,
        Alignment::Center,
    )
    .draw(frame);
    let _ = Text::with_alignment(
        if quarters {
            "quarters free"
        } else {
            "quarters computed"
        },
        Point::new(WIDTH as i32 / 2, 268),
        small,
        Alignment::Center,
    )
    .draw(frame);
}

/// Sends the picture and says what it cost.
fn show(screen: &mut Screen<'_>) {
    let started = Instant::now();
    let result = screen.present();
    let took = started.elapsed();
    if let Err(err) = result {
        error!("sending the picture failed: {err:?}");
    }
    info!(
        "{:3} deg {:8}: {} us",
        screen.orientation() * 30,
        if screen.free() {
            "madctl"
        } else if screen.filter() == Filter::Nearest {
            "nearest"
        } else {
            "bilinear"
        },
        took.as_micros()
    );
}

/// A scene made of what rotation is hard on: a thin ring, small text, and radial marks at
/// exactly the 30-degree steps the orientation setting offers.
fn draw_scene(frame: &mut Framebuffer) {
    let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
    let white = PrimitiveStyle::with_stroke(Rgb565::WHITE, 1);
    let dim = PrimitiveStyle::with_stroke(Rgb565::CSS_DIM_GRAY, 1);

    let _ = Circle::with_center(centre, 356)
        .into_styled(dim)
        .draw(frame);
    let _ = Circle::with_center(centre, 240)
        .into_styled(white)
        .draw(frame);

    const MARKS: [(i32, i32); STEPS] = [
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
    for (index, (sx, sy)) in MARKS.into_iter().enumerate() {
        let inner = 150;
        let outer = if index == 0 { 100 } else { 165 };
        let from = Point::new(centre.x + sx * inner / 1000, centre.y + sy * inner / 1000);
        let to = Point::new(centre.x + sx * outer / 1000, centre.y + sy * outer / 1000);
        let _ = Line::new(from, to)
            .into_styled(if index == 0 {
                PrimitiveStyle::with_stroke(Rgb565::CSS_ORANGE, 3)
            } else {
                white
            })
            .draw(frame);
    }

    let big = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let small = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_LIGHT_GRAY);
    let _ = Text::with_alignment("TeeToTum", centre, big, Alignment::Center).draw(frame);
    let _ = Text::with_alignment(
        "six by ten, the size rotation ruins first",
        Point::new(centre.x, centre.y + 24),
        small,
        Alignment::Center,
    )
    .draw(frame);
    let _ = Text::with_alignment(
        "tap: filter   enter: quarters   swipe: upright",
        Point::new(centre.x, centre.y - 30),
        small,
        Alignment::Center,
    )
    .draw(frame);
}
