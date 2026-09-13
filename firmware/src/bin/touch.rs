//! The finger against the picture.
//!
//! The touch controller was identified over the bus (`src/bin/probe.rs`) and it reports
//! coordinates, but nothing so far says how those coordinates stand to what is drawn. They
//! need not agree: the panel is mounted upside down in this case (`PANEL_MOUNT_MADCTL` is
//! `0xC0`), and the controller knows nothing about that.
//!
//! So this draws the picture with two landmarks a finger cannot mistake -- a white bar at
//! twelve o'clock and an orange one at three o'clock, as the picture is seen -- and answers
//! every touch with **two** markers:
//!
//! - a **green** square at the raw coordinates, drawn straight onto the panel, and
//! - a **blue** square at the same point turned half a turn, `359 - x` and `359 - y`.
//!
//! Reading it: put a finger on the white bar at twelve o'clock. Whichever marker appears under
//! the fingertip names the transform. Green means the controller and the picture already agree
//! and touch needs no correction; blue means the controller reports in the panel's mounting
//! frame and every coordinate has to be turned to match the picture.
//!
//! The log carries the raw numbers besides, along with the event and the raw gesture code, so
//! that the datasheet's names for the swipes can be checked against the picture's own up and
//! down while the hand is already on the glass.
//!
//! What it answers:
//!
//! - The blue marker is the one under the finger. **Touch reports in the panel's mounting
//!   frame**, half a turn from the picture, exactly as `PANEL_MOUNT_MADCTL` describes for the
//!   pixels.
//! - The gestures are turned the same way. A swipe from the picture's left edge to its right
//!   reports `0x03`, the reverse `0x04`; a swipe down the picture reports `0x01`, so in the
//!   mounting frame `0x01` is up.
//! - **The gesture arrives while the finger is still down**, in a read that also carries a
//!   contact -- logging the gesture only when a contact-suppressing branch lets it through
//!   drops every gesture that occurs while a finger is on the glass.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use teetotum::display::{DisplayBus, DisplayReset};
use teetotum::panel::{INIT_COMMANDS, POST_INIT_COMMANDS};
use teetotum::touch::{Event, Touch};
use log::{error, info, warn};
use st77916::{ColorMode, DisplaySize, St77916};

/// The panel is 360x360 of visible glass, and the glass is a circle inside it.
const PANEL_WIDTH: u16 = 360;
const PANEL_HEIGHT: u16 = 360;
const DISPLAY_SIZE: DisplaySize = DisplaySize::new(PANEL_WIDTH, PANEL_HEIGHT);

/// Half the edge of a marker square; a rectangle fill is what the panel draws cheaply.
const MARKER_HALF: u16 = 9;

/// Background, RGB565: dark, but visibly not "off".
const BACKGROUND: u16 = 0x0009;
/// The bar at twelve o'clock of the picture.
const NORTH: u16 = 0xFFFF;
/// The bar at three o'clock, so that left and right are told apart as well as up and down.
const EAST: u16 = 0xFD20;
/// The marker drawn where the controller says the finger is.
const RAW: u16 = 0x07E0;
/// The marker drawn half a turn away from that.
const TURNED: u16 = 0x049F;

/// How often the controller is asked, in milliseconds.
const POLL_MS: u32 = 20;

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
    let dma_rx = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("the DMA read buffer is malformed");
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
    let bus = DisplayBus::new(spi, Output::new(peripherals.GPIO14, Level::High, OutputConfig::default()));

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

    fill_rect(&mut display, buffer, 0, 0, PANEL_WIDTH, PANEL_HEIGHT, BACKGROUND);
    // Twelve o'clock and three o'clock of the picture, which is what a finger aims at.
    fill_rect(&mut display, buffer, 174, 10, 12, 34, NORTH);
    fill_rect(&mut display, buffer, 316, 174, 34, 12, EAST);

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11)
    .with_scl(peripherals.GPIO12);

    // The pulse on GPIO10 is what the vendor drivers do, and it costs sixty milliseconds. A
    // run with `Touch::attached`, which never touches the pin, answered just as well -- so the
    // reset is a courtesy here, not a requirement.
    let mut touch = Touch::new(
        Output::new(peripherals.GPIO10, Level::High, OutputConfig::default()),
        Input::new(
            peripherals.GPIO9,
            InputConfig::default().with_pull(Pull::Up),
        ),
        &delay,
    );

    match touch.chip_id(&mut i2c) {
        Ok(id) => info!(
            "Touch: chip id {id:#04x}{}",
            if id == 0xB6 { " (CST816D)" } else { "" }
        ),
        Err(err) => error!("Touch: the controller does not answer: {err:?}"),
    }
    if let Ok(version) = touch.firmware(&mut i2c) {
        info!("Touch: firmware version {version:#04x}");
    }

    // The motion mask rests at zero and slides are reported without it -- measured by holding
    // it there through thirty seconds of continuous swiping and writing it with the hand still
    // going, which changed nothing that could be seen. It is written here for the double
    // click, which has not been seen without it.
    match touch.gesture_registers(&mut i2c) {
        Ok((mask, irq)) => info!("Touch: motion mask {mask:#04x}, irq control {irq:#04x} at reset"),
        Err(err) => warn!("Touch: gesture registers unreadable: {err:?}"),
    }
    if let Err(err) = touch.enable_gestures(&mut i2c) {
        error!("Touch: enabling gestures failed: {err:?}");
    }

    info!("Touch: put a finger on the white bar at twelve o'clock");
    info!("Touch: green marker under the finger = raw coordinates match the picture");
    info!("Touch: blue marker under the finger = coordinates are half a turn out");

    let mut drawn: Option<(u16, u16, u16, u16)> = None;
    let mut down = false;

    loop {
        match touch.read(&mut i2c) {
            Ok(report) => {
                // The gesture is logged whether or not a finger is on the glass: the
                // controller names a slide in the same read that reports the finger gone.
                if report.gesture_code != 0 {
                    info!(
                        "Touch: gesture {:?} ({:#04x}){}",
                        report.gesture,
                        report.gesture_code,
                        if report.contact.is_some() { ", finger still down" } else { "" }
                    );
                }

                match report.contact {
                    Some(contact) => {
                        if contact.event != Event::Contact || !down {
                            info!(
                                "Touch: {:?} at {},{} -- turned {},{} -- int {}",
                                contact.event,
                                contact.x,
                                contact.y,
                                turned(contact.x),
                                turned(contact.y),
                                if touch.is_asserted() { "low" } else { "high" }
                            );
                        }
                        down = true;

                        let raw_x = corner(contact.x);
                        let raw_y = corner(contact.y);
                        let turned_x = corner(turned(contact.x));
                        let turned_y = corner(turned(contact.y));
                        let side = MARKER_HALF * 2;

                        if let Some((old_raw_x, old_raw_y, old_turned_x, old_turned_y)) =
                            drawn.replace((raw_x, raw_y, turned_x, turned_y))
                        {
                            fill_rect(
                                &mut display, buffer, old_raw_x, old_raw_y, side, side, BACKGROUND,
                            );
                            fill_rect(
                                &mut display,
                                buffer,
                                old_turned_x,
                                old_turned_y,
                                side,
                                side,
                                BACKGROUND,
                            );
                        }
                        fill_rect(&mut display, buffer, raw_x, raw_y, side, side, RAW);
                        fill_rect(&mut display, buffer, turned_x, turned_y, side, side, TURNED);
                        // The landmarks are what the finger aims at, so they are drawn back
                        // over any marker that has just eaten into them.
                        fill_rect(&mut display, buffer, 174, 10, 12, 34, NORTH);
                        fill_rect(&mut display, buffer, 316, 174, 34, 12, EAST);
                    }
                    None => {
                        if down {
                            info!("Touch: released");
                            down = false;
                        }
                    }
                }
            }
            Err(err) => warn!("Touch: read failed: {err:?}"),
        }

        delay.delay_millis(POLL_MS);
    }
}

/// The same point half a turn away, which is what a panel mounted upside down asks for.
fn turned(value: u16) -> u16 {
    (PANEL_WIDTH - 1).saturating_sub(value.min(PANEL_WIDTH - 1))
}

/// The top left corner of a marker centred on `value`, kept inside the panel.
fn corner(value: u16) -> u16 {
    let side = MARKER_HALF * 2;
    value
        .saturating_sub(MARKER_HALF)
        .min(PANEL_WIDTH - side)
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
        error!("Touch: window {x},{y} {width}x{height} failed: {err:?}");
        return;
    }
    if let Err(err) = display.interface_mut().fill_bytes(&buffer[..staged], total) {
        error!("Touch: fill at {x},{y} failed: {err:?}");
    }
}
