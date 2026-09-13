//! How far into the external RAM the DMA can read, asked without a pair of eyes.
//!
//! `src/bin/psramdma.rs` moved the picture to the firmware's own address for it --
//! `0x3c1a0000` instead of `0x3c020000` -- with nothing else changed, and it struck stripes at
//! that address too. So the question left is *which* address the DMA actually reads.
//!
//! The display's own channel cannot answer that: it writes to a panel that cannot be read back.
//! This uses the other end of the same peripheral -- `Mem2Mem` on `DMA_CH1`, which nothing else
//! in this project uses -- to have the DMA read a block of external RAM into internal RAM,
//! where the CPU can look at it.
//!
//! # What the pattern says
//!
//! Every block is filled with **its own address**: word `i` of a block at `base` holds
//! `base ^ i`. So a block that comes back wrong does not merely say "wrong", it says where the
//! bytes came from -- `word ^ i` is the base address the DMA actually read. A read that lands
//! nowhere at all shows as zeros or as the same word over and over.
//!
//! # What it cannot say
//!
//! This is a different channel from the display's. A clean read here does not clear the
//! display's read, and a boundary found here is a boundary of *this* channel -- but both are
//! the same GDMA reading the same external RAM through the same cache, so a boundary here is
//! the strongest cheap evidence there is, and it costs no hand at all.
//!
//! Headless: `python3 tools/listen.py --seconds 40`.

#![no_std]
#![no_main]

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma::{BurstConfig, Mem2Mem, SimpleMem2Mem};
use esp_hal::dma_buffers;
use log::{error, info};
use teetotum::display::write_back;
use teetotum::screen::{Screen, ScreenPins};

/// How much is read in one go, in bytes.
///
/// Small enough that a sweep of the whole window is over in seconds, large enough that it is a
/// real multi-descriptor transfer and not a single burst.
const BLOCK: usize = 4096;

/// How far apart the blocks of the coarse sweep are, in bytes.
///
/// 64 KiB is the page the cache's MMU maps in, so a boundary that belongs to the mapping falls
/// on one of these.
const STRIDE: usize = 64 * 1024;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();
    delay.delay_millis(500);
    info!("--- psramreach: how far into the external RAM the DMA reads ---");

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
    // The screen is here only because it is what brings the external RAM up and hands out the
    // rest of it; nothing is sent to the panel.
    let mut screen = match Screen::new(
        peripherals.PSRAM,
        peripherals.SPI2,
        peripherals.DMA_CH0,
        pins,
        delay,
    ) {
        Ok(screen) => screen,
        Err(err) => stop(delay, format_args!("the screen did not come up: {err:?}")),
    };
    let _ = screen.frame().clear(Rgb565::BLACK);
    let Some(spare) = screen.take_spare() else {
        stop(delay, format_args!("the external RAM has no spare to sweep"));
    };
    let from = spare.as_ptr() as usize;
    info!(
        "the spare is {} KiB, {:#x} to {:#x}",
        spare.len() / 1024,
        from,
        from + spare.len()
    );

    let (rx_buffer, rx_descriptors, _tx_buffer, tx_descriptors) = dma_buffers!(BLOCK, BLOCK);
    // The channel needs a peripheral slot to borrow; SPI3 is not wired to anything on this
    // board, and the display has SPI2.
    let mem2mem = Mem2Mem::new(peripherals.DMA_CH1, peripherals.SPI3);
    let mut engine = match SimpleMem2Mem::new(
        mem2mem,
        rx_descriptors,
        tx_descriptors,
        BurstConfig::default(),
    ) {
        Ok(engine) => engine,
        Err(err) => stop(delay, format_args!("the DMA refused the descriptors: {err:?}")),
    };

    info!("block {BLOCK} bytes, stride {} KiB", STRIDE / 1024);
    info!("addr        read        verdict");

    let mut last_good: Option<usize> = None;
    let mut first_bad: Option<usize> = None;
    let mut offset = 0usize;
    while offset + BLOCK <= spare.len() {
        let base = from + offset;
        let good = read_block(&mut engine, &mut spare[offset..offset + BLOCK], rx_buffer, base);
        if good {
            if first_bad.is_none() {
                last_good = Some(base);
            }
        } else if first_bad.is_none() {
            first_bad = Some(base);
        }
        offset += STRIDE;
    }

    match (last_good, first_bad) {
        (_, None) => info!("every block of the window came back whole -- no boundary here"),
        (None, Some(bad)) => info!("the very first block at {bad:#x} came back wrong"),
        (Some(good), Some(bad)) => {
            info!("last whole block {good:#x}, first wrong block {bad:#x} -- narrowing");
            let boundary = narrow(&mut engine, spare, rx_buffer, from, good, bad);
            info!("the boundary is between {:#x} and {:#x}", boundary.0, boundary.1);
            info!(
                "that is {} KiB and {} KiB into the window",
                (boundary.0 - from) / 1024,
                (boundary.1 - from) / 1024
            );
        }
    }

    info!("--- done ---");
    loop {
        delay.delay_millis(1000);
    }
}

/// Fills a block with its own address, has the DMA read it back, and says whether it arrived.
///
/// Logs a line for every block that did not, with what the returned words say about where the
/// bytes came from.
fn read_block(
    engine: &mut SimpleMem2Mem<'_, esp_hal::Blocking>,
    block: &mut [u8],
    landing: &mut [u8],
    base: usize,
) -> bool {
    for (i, word) in block.chunks_exact_mut(4).enumerate() {
        word.copy_from_slice(&((base as u32) ^ (i as u32)).to_le_bytes());
    }
    write_back(block);
    landing.fill(0);

    match engine.start_transfer(landing, block) {
        Ok(transfer) => {
            if let Err(err) = transfer.wait() {
                error!("{base:#x}  the transfer failed: {err:?}");
                return false;
            }
        }
        Err(err) => {
            error!("{base:#x}  the transfer would not start: {err:?}");
            return false;
        }
    }

    let mut wrong = 0usize;
    let mut said: Option<u32> = None;
    for (i, word) in landing.chunks_exact(4).enumerate() {
        let value = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        if value != (base as u32) ^ (i as u32) {
            wrong += 1;
            if said.is_none() {
                said = Some(value ^ (i as u32));
            }
        }
    }
    if wrong == 0 {
        return true;
    }
    match said {
        Some(came) => info!(
            "{base:#x}  {came:#x}  {wrong} of {} words wrong",
            landing.len() / 4
        ),
        None => info!("{base:#x}  -           {wrong} words wrong"),
    }
    false
}

/// Halves the gap between the last whole block and the first wrong one until they are adjacent.
///
/// Returns the two addresses the boundary lies between.
fn narrow(
    engine: &mut SimpleMem2Mem<'_, esp_hal::Blocking>,
    spare: &mut [u8],
    landing: &mut [u8],
    from: usize,
    good: usize,
    bad: usize,
) -> (usize, usize) {
    let mut good = good;
    let mut bad = bad;
    while bad - good > BLOCK {
        // Kept on a whole block, so every probe is the same shape as the sweep's.
        let middle = good + ((bad - good) / 2) / BLOCK * BLOCK;
        if middle == good {
            break;
        }
        let offset = middle - from;
        if read_block(engine, &mut spare[offset..offset + BLOCK], landing, middle) {
            good = middle;
        } else {
            bad = middle;
        }
    }
    (good, bad)
}

/// Says why and stands still, because a probe that goes on after its ground is gone says
/// nothing worth reading.
fn stop(delay: Delay, why: core::fmt::Arguments<'_>) -> ! {
    error!("{why} -- stopping");
    loop {
        delay.delay_millis(1000);
    }
}
