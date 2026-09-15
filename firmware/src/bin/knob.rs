//! The knob against a mark on the screen.
//!
//! Two things about the encoder could not be read off a pin: which of the two pulse lines is a
//! turn clockwise, and how many pulses one revolution of the knob makes. Both need the hand and
//! the screen in the same picture. So this draws a dot on a circle, moves it with the knob, and
//! logs every step with its running count.
//!
//! Reading it:
//!
//! - **Direction.** The dot is meant to follow the finger. If turning the knob clockwise walks
//!   the dot anticlockwise, the two lines are the other way round: swap the arguments to
//!   [`Encoder::new`] below, and that is the answer to write down.
//! - **Pulses per revolution.** Park the dot on the white index mark at twelve o'clock, note
//!   the count in the log, turn the knob a whole number of revolutions -- five, so that a
//!   swallowed pulse shows up as a count that no longer divides -- and read the count again.
//!
//! Measured: GPIO8 pulses for a turn clockwise, and five revolutions make exactly 150 pulses, so
//! the knob has 30 steps to the turn.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use log::{error, info};
use st77916::{ColorMode, DisplaySize, St77916};
use teetotum::display::{DisplayBus, DisplayReset};
use teetotum::encoder::{Encoder, PULSES_PER_REVOLUTION};
use teetotum::panel::{INIT_COMMANDS, POST_INIT_COMMANDS};

/// The panel is 360x360, and the visible screen is a circle inside it.
const PANEL_WIDTH: u16 = 360;
const PANEL_HEIGHT: u16 = 360;
const DISPLAY_SIZE: DisplaySize = DisplaySize::new(PANEL_WIDTH, PANEL_HEIGHT);

/// The centre of that circle, in the controller's coordinates.
const CENTRE_X: i32 = 180;
const CENTRE_Y: i32 = 180;

/// How far out the moving dot runs.
///
/// Well inside the index mark, so that erasing the dot never eats into it.
const DOT_RADIUS: i32 = 128;
/// Half the dot's edge; the dot is a square, which is what a rectangle fill can draw.
const DOT_HALF: i32 = 11;

/// How far the dot turns for one pulse of the knob.
///
/// At the measured step count this is 12 degrees, which makes the dot track the finger one to
/// one: a full turn of the knob brings it back to the index mark. That is also the cheapest way
/// to check the step count again -- a dot that drifts off the mark over a few turns means the
/// constant in the driver is wrong.
const DEGREES_PER_PULSE: i32 = 360 / PULSES_PER_REVOLUTION;

/// Background, RGB565: dark, but visibly not "off".
const BACKGROUND: u16 = 0x0009;
/// The fixed mark at twelve o'clock, which the dot is measured against.
const INDEX: u16 = 0xFFFF;
/// The dot that follows the knob.
const DOT: u16 = 0xFD20;
/// The hub, so that the centre is visible even when the dot is behind the index mark.
const HUB: u16 = 0x4208;

/// How often the pins are read.
///
/// A pulse stays low for 15 milliseconds at its shortest, so this sees every one many times
/// over, and the driver's own debounce does the rest.
const POLL_MS: u32 = 2;

/// Staging buffer for rectangle fills, in static memory. A fill larger than this repeats it.
const BUFFER_BYTES: usize = 21600;
static mut BUFFER: [u8; BUFFER_BYTES] = [0; BUFFER_BYTES];

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let _backlight = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());

    let mut delay = Delay::new();

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(1, BUFFER_BYTES);
    let dma_rx =
        DmaRxBuf::new(rx_descriptors, rx_buffer).expect("the DMA read buffer is malformed");
    let dma_tx =
        DmaTxBuf::new(tx_descriptors, tx_buffer).expect("the DMA write buffer is malformed");

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default().with_frequency(Rate::from_mhz(10)),
    )
    .expect("the display SPI peripheral could not be configured")
    .with_sck(peripherals.GPIO13)
    .with_sio0(peripherals.GPIO15)
    .with_sio1(peripherals.GPIO16)
    .with_sio2(peripherals.GPIO17)
    .with_sio3(peripherals.GPIO18)
    .with_dma(peripherals.DMA_CH0)
    .with_buffers(dma_rx, dma_tx);

    let reset = DisplayReset {
        pin: Output::new(peripherals.GPIO21, Level::High, OutputConfig::default()),
        delay,
    };
    let bus = DisplayBus::new(
        spi,
        Output::new(peripherals.GPIO14, Level::High, OutputConfig::default()),
    );

    let mut display = match St77916::builder(bus, reset, DISPLAY_SIZE)
        .with_init_commands(INIT_COMMANDS)
        .build(ColorMode::Rgb565, &mut delay)
    {
        Ok(display) => display,
        Err(err) => {
            error!("Display: initialisation failed: {err:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    delay.delay_millis(150);
    for &(cmd, data, wait) in POST_INIT_COMMANDS {
        if let Err(err) = display.send_command_with_data(cmd, data) {
            error!("Display: post-init command {cmd:#04x} failed: {err:?}");
        }
        delay.delay_millis(u32::from(wait));
    }

    // SAFETY: `main` runs once, and nothing else in this binary touches BUFFER.
    let buffer: &mut [u8; BUFFER_BYTES] = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };

    fill_rect(
        &mut display,
        buffer,
        0,
        0,
        PANEL_WIDTH,
        PANEL_HEIGHT,
        BACKGROUND,
    );
    // The index mark: a bar at twelve o'clock, outside the dot's reach.
    fill_rect(&mut display, buffer, 174, 10, 12, 34, INDEX);
    fill_rect(&mut display, buffer, 172, 172, 16, 16, HUB);

    // GPIO8 counts up here, GPIO7 down. Which of them is clockwise is exactly what this
    // binary asks: if the dot runs away from the finger, these two belong the other way round.
    let config = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, config),
        Input::new(peripherals.GPIO7, config),
    );

    info!("Knob: up = GPIO8, down = GPIO7, {DEGREES_PER_PULSE} degrees per pulse");
    info!("Knob: park the dot on the mark, then turn exactly one revolution and read the count");

    // The dot starts on the mark, at nought pulses. That makes the check for the step count a
    // thing to look at rather than to read: one whole revolution of the knob has to bring the
    // dot back here.
    let (start_x, start_y) = dot_corner(0);
    let side = (DOT_HALF * 2) as u16;
    fill_rect(&mut display, buffer, start_x, start_y, side, side, DOT);

    let mut drawn: Option<(u16, u16)> = Some((start_x, start_y));
    loop {
        if encoder.poll() != 0 {
            let position = encoder.position();
            let angle = position * DEGREES_PER_PULSE;
            info!("Knob: {position} pulses, {} degrees", angle.rem_euclid(360));

            let (x, y) = dot_corner(angle);
            if let Some((old_x, old_y)) = drawn.replace((x, y))
                && (old_x, old_y) != (x, y)
            {
                fill_rect(&mut display, buffer, old_x, old_y, side, side, BACKGROUND);
            }
            fill_rect(&mut display, buffer, x, y, side, side, DOT);
        }
        delay.delay_millis(POLL_MS);
    }
}

/// The top left corner of the dot for an angle in degrees, zero at twelve o'clock and growing
/// clockwise as the screen is seen.
fn dot_corner(angle: i32) -> (u16, u16) {
    let x = CENTRE_X + (DOT_RADIUS * sin_q10(angle)) / 1024 - DOT_HALF;
    let y = CENTRE_Y - (DOT_RADIUS * cos_q10(angle)) / 1024 - DOT_HALF;
    (x as u16, y as u16)
}

/// Sine of an angle in degrees, scaled by 1024.
///
/// Bhaskara's approximation, which is integer arithmetic and wrong by less than two parts in a
/// thousand -- a quarter of a pixel at this radius, and the dot is 22 pixels across. A lookup
/// table would be exact and would also be 360 entries of a constant nobody reads.
fn sin_q10(degrees: i32) -> i32 {
    let degrees = degrees.rem_euclid(360);
    let (x, sign) = if degrees <= 180 {
        (degrees, 1)
    } else {
        (degrees - 180, -1)
    };
    let product = x * (180 - x);
    sign * (4 * product * 1024) / (40500 - product)
}

/// Cosine, which is the sine a quarter turn along.
fn cos_q10(degrees: i32) -> i32 {
    sin_q10(degrees + 90)
}

/// Paints one rectangle in a solid colour, the window set first and CS held down for the whole
/// of it: the controller ends a pixel write when CS goes up, so a fill split across two
/// transactions would lose everything after the first.
fn fill_rect(
    display: &mut St77916<DisplayBus<'_>, DisplayReset<'_>>,
    buffer: &mut [u8],
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    colour: u16,
) {
    let total = usize::from(width) * usize::from(height) * 2;
    let staged = buffer.len().min(total);
    let [high, low] = colour.to_be_bytes();
    for pixel in buffer[..staged].chunks_exact_mut(2) {
        pixel[0] = high;
        pixel[1] = low;
    }

    if let Err(err) = display.set_window(x, y, x + width - 1, y + height - 1) {
        error!("Knob: window {x},{y} {width}x{height} failed: {err:?}");
        return;
    }
    if let Err(err) = display.interface_mut().fill_bytes(&buffer[..staged], total) {
        error!("Knob: fill at {x},{y} failed: {err:?}");
    }
}
