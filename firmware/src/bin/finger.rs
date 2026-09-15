//! Draw on the picture with a finger, then turn the picture and draw some more.
//!
//! Everything up to here turned pixels. This turns the other direction: the screen says where a
//! finger is in **its** frame, the panel is mounted half a turn round, and the picture on top of
//! that may stand at any of four quarter turns -- so a touch has to come back through both turns
//! before anything drawn into the framebuffer can claim to be under the fingertip.
//!
//! The two halves belong to two different modules.
//! [`Contact::in_view`](teetotum::touch::Contact::in_view) undoes the mount, and
//! [`Screen::picture_point`](teetotum::screen::Screen::picture_point) undoes the orientation.
//! Neither knows about the other; this run is where they meet.
//!
//! **The ink is the check.** A stroke is drawn into the picture at the point the two turns say
//! the finger was, so it is turned back out again on its way to the screen. If the mapping is
//! right, the line grows under the fingertip and then **stays where it was put**: turn the knob
//! and the drawing turns with the picture, mark unchanged, and drawing over an old stroke at a
//! new angle lands on it. If the mapping is wrong, the ink appears somewhere else -- mirrored,
//! or a quarter turn off -- and the shape of the error names the mistake.
//!
//! In the hand:
//!
//! * **draw on the screen** -- ink, in the picture's coordinates;
//! * **turn the knob** -- the picture and everything drawn on it turn, a quarter turn per detent;
//! * **`c` in the monitor** -- clear the ink and start over;
//! * **`0`** -- back upright.
//!
//! Slides are logged, not acted on, and logged three times over: as the controller codes them,
//! as the viewer made them, and as the picture sees them at this orientation.
//!
//! **The ink lands under the fingertip at a turned picture too**, not only upright, and it stays
//! where it was put when the knob moves on. That the drawing turns with the picture proves
//! nothing by itself -- it lies in the framebuffer and would turn whatever the mapping did; the
//! check is drawing at a quarter turn that is not zero and seeing the stroke grow under the
//! finger rather than a quarter turn away from it.
//!
//! Run it in the monitor with a hand on it: `cargo run --release --bin finger`.

#![no_std]
#![no_main]

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle};
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
use teetotum::screen::{ORIENTATIONS, Screen, ScreenPins};
use teetotum::touch::{Event, Gesture, Touch};

/// How often the screen is asked. The controller has nothing to say most of the time, and a
/// finger crossing the screen in half a second wants more samples than that.
const TOUCH_PERIOD: Duration = Duration::from_millis(15);

/// Radius of the ink dot, in picture pixels. Wide enough to see against the ring, narrow enough
/// that a stroke is a stroke and not a smear.
const NIB: u32 = 7;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();
    delay.delay_millis(500);
    info!("--- finger: the touch, turned back into the picture ---");

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
    match touch.chip_id(&mut i2c) {
        Ok(id) => info!("touch controller answers {id:#04x}"),
        Err(e) => error!("touch controller does not answer: {e:?} -- there will be no ink"),
    }

    // The keys, for everything that is not drawing. A run whose only control is the screen loses
    // the control the moment the screen is the thing under suspicion.
    let (mut keys, _) = UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow()).split();

    // GPIO8 is the clockwise direction, measured against a dot on the screen.
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    screen.frame().clear(Rgb565::BLACK).ok();
    draw_scene(screen.frame());
    if let Err(err) = screen.present() {
        error!("sending the picture failed: {err:?}");
    }
    info!("draw on the screen, turn the knob, 'c' clears the ink, '0' stands it upright");

    let mut next_touch = Instant::now();
    let mut down = false;
    loop {
        let mut changed = false;

        let detents = encoder.poll();
        if detents != 0 {
            let turned = screen.orientation() as i32 + detents;
            screen.set_orientation(turned.rem_euclid(ORIENTATIONS as i32) as usize);
            info!("orientation {:3} deg", screen.orientation() * 90);
            changed = true;
        }

        if let Ok(byte) = keys.read_byte() {
            match byte {
                b'c' | b'C' | b'\r' | b'\n' => {
                    screen.frame().clear(Rgb565::BLACK).ok();
                    draw_scene(screen.frame());
                    info!("cleared");
                    changed = true;
                }
                b'0' => {
                    screen.set_orientation(0);
                    changed = true;
                }
                _ => {}
            }
        }

        if Instant::now() >= next_touch {
            next_touch = Instant::now() + TOUCH_PERIOD;
            match touch.read(&mut i2c) {
                Ok(report) => {
                    if report.gesture != Gesture::None {
                        info!(
                            "gesture {:#04x}: controller {:?}, viewer {:?}, picture {:?} at {} deg",
                            report.gesture_code,
                            report.gesture,
                            report.gesture.in_picture_mount(),
                            report
                                .gesture
                                .in_picture_mount()
                                .in_picture(screen.picture_quarter()),
                            screen.orientation() * 90,
                        );
                    }

                    match report.contact {
                        Some(contact) => {
                            let (vx, vy) = contact.in_view();
                            let (px, py) = screen.picture_point(vx, vy);
                            if contact.event == Event::Down || !down {
                                info!(
                                    "finger at {:3},{:3} -> viewer {:3},{:3} -> picture {:4},{:4} at {} deg",
                                    contact.x,
                                    contact.y,
                                    vx,
                                    vy,
                                    px,
                                    py,
                                    screen.orientation() * 90
                                );
                                down = true;
                            }
                            if ink(screen.frame(), px, py) {
                                changed = true;
                            } else {
                                warn!("picture {px},{py} is off the picture -- no ink");
                            }
                        }
                        None => down = false,
                    }
                }
                Err(e) => warn!("the screen did not answer: {e:?}"),
            }
        }

        if changed && let Err(err) = screen.present() {
            error!("sending the picture failed: {err:?}");
        }
    }
}

/// Puts one dot of ink into the picture, and says whether it landed on it.
///
/// Every point on the screen maps inside the picture, so a miss means the mapping is wrong.
fn ink(frame: &mut Framebuffer, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
        return false;
    }
    let _ = Circle::with_center(Point::new(x, y), NIB)
        .into_styled(PrimitiveStyle::with_fill(Rgb565::CSS_ORANGE))
        .draw(frame);
    true
}

/// The picture to draw on: a ring, twelve marks, and a plain north so that the angle can be
/// read off the screen without asking the log.
fn draw_scene(frame: &mut Framebuffer) {
    let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
    let white = PrimitiveStyle::with_stroke(Rgb565::WHITE, 1);
    let dim = PrimitiveStyle::with_stroke(Rgb565::CSS_DIM_GRAY, 1);

    let _ = Circle::with_center(centre, 356)
        .into_styled(dim)
        .draw(frame);
    let _ = Circle::with_center(centre, 240)
        .into_styled(dim)
        .draw(frame);

    const MARKS: [(i32, i32); 12] = [
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
        let inner = if index == 0 { 120 } else { 160 };
        let outer = 176;
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

    let small = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_LIGHT_GRAY);
    let _ = Text::with_alignment(
        "draw on the screen",
        Point::new(centre.x, centre.y - 6),
        small,
        Alignment::Center,
    )
    .draw(frame);
    let _ = Text::with_alignment(
        "the ink stays with the picture",
        Point::new(centre.x, centre.y + 8),
        small,
        Alignment::Center,
    )
    .draw(frame);
}
