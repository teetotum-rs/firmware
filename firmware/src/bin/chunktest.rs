//! Which SPI transfer sizes actually reach the panel?
//!
//! The DMA buffer and the staging array are fixed at the largest size under test for every band,
//! and only the length of a single `half_duplex_write` varies -- so the transfer length is the
//! only thing changing between bands.
//!
//! The panel is painted black first, then six 60-row bands top to bottom, each in its own
//! colour and each written with a different chunk size. The window is set per band, so a band
//! that fails cannot displace the one after it. A band that stays black did not arrive; where
//! the picture stops is the answer.

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
use teetotum::display::{DisplayBus, DisplayReset};
use teetotum::panel::{INIT_COMMANDS, POST_INIT_COMMANDS};
use log::{error, info};
use st77916::{ColorMode, DisplaySize, St77916};

/// The panel is 360x360.
const PANEL_WIDTH: usize = 360;
const PANEL_HEIGHT: usize = 360;
const DISPLAY_SIZE: DisplaySize = DisplaySize::new(PANEL_WIDTH as u16, PANEL_HEIGHT as u16);
/// Bytes in one row of RGB565 pixels.
const ROW_BYTES: usize = PANEL_WIDTH * 2;

/// Rows in one test band; six bands cover the 360 rows exactly.
const BAND_HEIGHT: usize = 60;
/// Bytes in one band. Every chunk size below divides it exactly.
const BAND_BYTES: usize = BAND_HEIGHT * ROW_BYTES;

/// The largest chunk under test, and therefore the size of both the DMA buffer and the staging
/// array -- held constant across all bands so that only the transfer length varies.
const MAX_CHUNK: usize = 21600;

/// The staging buffer, in static memory rather than on the stack: at these sizes a stack buffer
/// is no longer reasonable. Band 0 repeats the smallest known-good chunk (720 bytes) so a
/// regression from the static location, not just from size, would still show up.
static mut BUFFER: [u8; MAX_CHUNK] = [0; MAX_CHUNK];

/// Chunk size in bytes, and the RGB565 colour the band written with it gets.
///
/// Everything up to 4320 bytes (including the 4092-byte DMA descriptor boundary) is known to
/// arrive; these carry that upwards to half a band in a single transfer.
const BANDS: [(usize, u16); 6] = [
    (720, 0xF800),   // red -- the control, the one size known to arrive
    (7200, 0xFFE0),  // yellow
    (8640, 0x07E0),  // green
    (10800, 0x07FF), // cyan
    (14400, 0x001F), // blue
    (21600, 0xF81F), // magenta -- half a band in one transfer
];

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

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(1, MAX_CHUNK);
    let dma_rx = DmaRxBuf::new(rx_descriptors, rx_buffer).expect("the DMA read buffer is malformed");
    let dma_tx = DmaTxBuf::new(tx_descriptors, tx_buffer).expect("the DMA write buffer is malformed");

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
    let buffer: &mut [u8; MAX_CHUNK] = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };
    let last_column = PANEL_WIDTH as u16 - 1;
    let last_row = PANEL_HEIGHT as u16 - 1;

    // Black over everything first, in the one-row transfers that are known to arrive. Anything
    // still black at the end is a band that did not make it.
    paint(&mut buffer[..ROW_BYTES], 0x0000);
    match display.set_window(0, 0, last_column, last_row) {
        Ok(()) => match display
            .interface_mut()
            .fill_repeating(&buffer[..ROW_BYTES], PANEL_HEIGHT)
        {
            Ok(()) => info!("Chunk test: panel cleared to black"),
            Err(err) => error!("Chunk test: clearing failed: {err:?}"),
        },
        Err(err) => error!("Chunk test: could not set the full window: {err:?}"),
    }

    for (index, &(chunk, colour)) in BANDS.iter().enumerate() {
        let top = (index * BAND_HEIGHT) as u16;
        let bottom = top + BAND_HEIGHT as u16 - 1;
        let repeats = BAND_BYTES / chunk;

        paint(&mut buffer[..chunk], colour);
        match display.set_window(0, top, last_column, bottom) {
            Ok(()) => match display
                .interface_mut()
                .fill_repeating(&buffer[..chunk], repeats)
            {
                Ok(()) => info!(
                    "Chunk test: band {index} rows {top}..={bottom} colour {colour:#06x} \
                     chunk {chunk} bytes x{repeats}: sent"
                ),
                Err(err) => error!("Chunk test: band {index} chunk {chunk} bytes failed: {err:?}"),
            },
            Err(err) => error!("Chunk test: band {index} window failed: {err:?}"),
        }
        delay.delay_millis(300);
    }

    info!("Chunk test: all bands sent, holding");
    loop {
        delay.delay_millis(5000);
    }
}
