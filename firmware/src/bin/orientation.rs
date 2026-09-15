//! Which way round is the panel mounted?
//!
//! Draws a letter F: asymmetric in both axes, so no flip of MADCTL (36h) can hide as "no
//! flip". Upright and the right way round means MADCTL is correct; any of the other seven
//! ways it can appear names the flip that is missing. A red square marks the corner the
//! controller calls (0,0), which says the same thing a second time.
//!
//! `MADCTL` below is the only thing to change: set it, flash, look at the screen. What the F
//! shows is the answer. `0xC0` is that answer, carried in `src/panel.rs`'s init table -- this
//! binary stays as the way to check that claim again.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use log::{error, info};
use st77916::{ColorMode, DisplaySize, St77916};
use teetotum::display::{DisplayBus, DisplayReset};
use teetotum::panel::{INIT_COMMANDS, POST_INIT_COMMANDS};

/// The panel is 360x360 pixels.
const PANEL_WIDTH: u16 = 360;
const PANEL_HEIGHT: u16 = 360;
const DISPLAY_SIZE: DisplaySize = DisplaySize::new(PANEL_WIDTH, PANEL_HEIGHT);

/// Memory access control, sent after the init table has run.
///
/// Bit 7 mirrors Y, bit 6 mirrors X, bit 5 exchanges the two axes, bit 3 swaps the colour
/// order to BGR. The init table leaves this at `0x00`, which is what put row 0 at the bottom
/// of the screen; `0xC0` is both mirrors, which is a 180 degree turn and the guess this run
/// tests. Bit 3 stays clear -- the colours are already right.
const MADCTL: u8 = 0xC0;

/// Background, RGB565: a dark blue that is visibly not "off".
const BACKGROUND: u16 = 0x0009;
/// The letter itself.
const INK: u16 = 0xFFFF;
/// The marker at the controller's origin.
const ORIGIN: u16 = 0xF800;

/// The letter F, as rectangles in the controller's own coordinates: x to the right, y down,
/// (0,0) the first pixel written after RAMWR.
///
/// The screen is round -- a circle of radius 180 about (180,180) -- so every corner of every
/// rectangle here stays well inside it, or the bezel would eat the evidence.
const STROKES: [(u16, u16, u16, u16, u16); 4] = [
    // x, y, width, height, colour
    (130, 90, 40, 180, INK),  // the stem
    (170, 90, 90, 40, INK),   // the top arm
    (170, 165, 60, 40, INK),  // the middle arm
    (90, 40, 40, 40, ORIGIN), // the corner the controller calls (0,0)
];

/// Staging buffer, in static memory. Sized at the largest transfer the chunk measurement
/// tried; a fill longer than this repeats it.
const BUFFER_BYTES: usize = 21600;
static mut BUFFER: [u8; BUFFER_BYTES] = [0; BUFFER_BYTES];

/// Fills a buffer with one repeated RGB565 colour, big-endian as it goes on the wire.
fn paint(buffer: &mut [u8], colour: u16) {
    let [high, low] = colour.to_be_bytes();
    for pixel in buffer.chunks_exact_mut(2) {
        pixel[0] = high;
        pixel[1] = low;
    }
}

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

    // After the table, so that this overrides the 0x36 the table itself carries.
    if let Err(err) = display.send_command_with_data(0x36, &[MADCTL]) {
        error!("Orientation: MADCTL {MADCTL:#04x} failed: {err:?}");
    }
    info!("Orientation: MADCTL set to {MADCTL:#04x}");

    // SAFETY: `main` runs once, and nothing else in this binary touches BUFFER.
    let buffer: &mut [u8; BUFFER_BYTES] = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };

    fill_rect(
        &mut display,
        buffer,
        (0, 0, PANEL_WIDTH, PANEL_HEIGHT, BACKGROUND),
    );
    for &stroke in &STROKES {
        fill_rect(&mut display, buffer, stroke);
    }

    info!("Orientation: F drawn, holding");
    loop {
        delay.delay_millis(5000);
    }
}

/// Paints one rectangle in a solid colour.
///
/// The window is set first, then the pixels go out as one write with CS held down for the
/// whole rectangle: the buffer is repeated as often as the rectangle needs and cut short at
/// the end, because splitting the remainder into a second transaction would lose it.
fn fill_rect(
    display: &mut St77916<DisplayBus<'_>, DisplayReset<'_>>,
    buffer: &mut [u8],
    (x, y, width, height, colour): (u16, u16, u16, u16, u16),
) {
    let total = usize::from(width) * usize::from(height) * 2;
    let staged = buffer.len().min(total);
    paint(&mut buffer[..staged], colour);

    if let Err(err) = display.set_window(x, y, x + width - 1, y + height - 1) {
        error!("Orientation: window {x},{y} {width}x{height} failed: {err:?}");
        return;
    }
    match display.interface_mut().fill_bytes(&buffer[..staged], total) {
        Ok(()) => info!("Orientation: {width}x{height} at {x},{y} in {colour:#06x}"),
        Err(err) => error!("Orientation: fill at {x},{y} failed: {err:?}"),
    }
}
