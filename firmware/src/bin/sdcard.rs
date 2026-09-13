//! Read the TF card: who it is, how it is partitioned, what the factory demo keeps on it, and
//! how fast any of it comes off.
//!
//! `sdprobe` found the four pins by asking the card for a CMD0 answer. This goes the rest of the
//! way: a full initialisation over the hardware SPI peripheral, then the questions that turn
//! "the wires are right" into "the data is right".
//!
//! - **Does the card describe itself consistently?** CID gives a manufacturer, a product name and
//!   a date; CSD gives a capacity. A wrong wire produces neither -- it produces noise that fails
//!   the CRC-checked handshake long before this.
//! - **Is there a filesystem, and is it the demo's?** The `spiffs` partition in flash is empty
//!   and the demo's media live on the card, but the card is inside a closed case with no reader
//!   to check its contents any other way.
//! - **What does a read cost?** Four hundred megabytes of media are worth nothing to a firmware
//!   that needs a minute to fetch a background. So: one block at a time against a run in one
//!   command, at two bus clocks, over the same sectors.
//!
//! The FAT reader is no longer here. Once it had proven the block device underneath works, that
//! was worth turning into [`teetotum::fat`], which this now uses like any other caller would. A
//! listing is a good test of a filesystem layer: it opens nothing and touches everything.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::{Instant, Rate};
use teetotum::fat::{Dir, Volume};
use teetotum::sd::{self, Kind, SdCard};
use log::{error, info, warn};

esp_bootloader_esp_idf::esp_app_desc!();

/// How deep to descend into subdirectories. The demo's media are one or two levels down; a cap
/// keeps a corrupt chain from recursing forever.
const MAX_DEPTH: usize = 4;
/// How many entries to print before stopping. A card full of MP3s would otherwise fill the log.
const MAX_ENTRIES: usize = 300;
/// How much is read for each throughput figure. Large enough that the command overhead of the
/// first block does not show, small enough that a slow combination still finishes.
const MEASURE_BYTES: usize = 256 * 1024;
/// The buffer a measured read goes into: one cluster of this card's filesystem, which is also
/// the largest run [`teetotum::fat::File::read`] will ever ask the card for.
const CHUNK: usize = 4096;
/// The bus clocks compared. 20 MHz is what `sd::FAST_RATE` picked without evidence; 25 MHz is
/// what the SD specification allows in SPI mode; 40 is over it and asked anyway, because the
/// panel took eight times its documented-by-nobody first value.
const RATES: [u32; 3] = [20, 25, 40];

/// A full-screen background of the factory demo: 360x360 pixels of RGB565 behind a four-byte
/// header, which is why it is 259204 bytes. It is the file this project actually wants off the
/// card, so it is the one whose read is timed.
const SAMPLE: &str = "/CLOCKBG/star_bg_360.bin";

/// What the walk found, carried along so it can be reported once at the end.
struct Tally {
    files: usize,
    directories: usize,
    bytes: u64,
    printed: usize,
    truncated: bool,
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // Nothing here allocates; the heap exists because the crate graph insists on an allocator.
    esp_alloc::heap_allocator!(size: 8 * 1024);
    let delay = Delay::new();

    info!("SD: bringing up the card on CLK 4, MOSI 3, MISO 5, CS 2");

    let spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(sd::INIT_RATE),
    )
    .expect("the card SPI peripheral could not be configured")
    .with_sck(peripherals.GPIO4)
    .with_mosi(peripherals.GPIO3)
    .with_miso(peripherals.GPIO5);

    let cs = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());

    let mut card = match SdCard::new(spi, cs, delay) {
        Ok(card) => card,
        Err(err) => {
            error!("SD: the card did not come up: {:?}", err);
            idle(delay);
        }
    };

    match card.kind() {
        Kind::HighCapacity => info!("SD: high capacity card, addressed in blocks"),
        Kind::StandardCapacity => info!("SD: standard capacity card, addressed in bytes"),
    }

    report_identity(&mut card);

    let mut volume = match Volume::mount(card) {
        Ok(volume) => volume,
        Err(err) => {
            error!("SD: no filesystem: {:?}", err);
            idle(delay);
        }
    };
    let layout = *volume.layout();
    info!(
        "SD: {} volume, {} sectors per cluster, {} clusters, tables at LBA {}, data at LBA {}",
        if layout.fat32 { "FAT32" } else { "FAT16" },
        layout.sectors_per_cluster,
        layout.clusters,
        layout.fat_start,
        layout.data_start
    );

    let mut tally = Tally {
        files: 0,
        directories: 0,
        bytes: 0,
        printed: 0,
        truncated: false,
    };
    info!("SD: root directory");
    let root = volume.root();
    walk(&mut volume, root, 0, &mut tally);
    info!(
        "SD: {} files in {} directories, {} bytes in total{}",
        tally.files,
        tally.directories,
        tally.bytes,
        if tally.truncated {
            " (listing truncated)"
        } else {
            ""
        }
    );

    measure(&mut volume);

    idle(delay)
}

/// Print what the card says about itself.
fn report_identity(card: &mut SdCard<'_>) {
    match card.cid() {
        Ok(cid) => {
            info!("SD: CID {:02x?}", cid);
            let name = core::str::from_utf8(&cid[3..8]).unwrap_or("?????");
            let serial = u32::from_be_bytes([cid[9], cid[10], cid[11], cid[12]]);
            let date = u16::from_be_bytes([cid[13], cid[14]]) & 0x0FFF;
            info!(
                "SD: manufacturer 0x{:02x}, product {:?}, revision {}.{}, serial {:08x}, made {}-{:02}",
                cid[0],
                name,
                cid[8] >> 4,
                cid[8] & 0x0F,
                serial,
                2000 + (date >> 4),
                date & 0x0F
            );
        }
        Err(err) => warn!("SD: CID unreadable: {:?}", err),
    }
    match card.csd() {
        Ok(csd) => info!("SD: CSD version {}, {:02x?}", (csd[0] >> 6) + 1, csd),
        Err(err) => warn!("SD: CSD unreadable: {:?}", err),
    }
    match card.capacity_blocks() {
        Ok(blocks) => info!(
            "SD: {} blocks of 512 bytes, {} MiB",
            blocks,
            blocks / 2048
        ),
        Err(err) => warn!("SD: capacity unreadable: {:?}", err),
    }
}

/// Print one directory and, up to [`MAX_DEPTH`], everything under it.
fn walk(volume: &mut Volume<'_>, dir: Dir, depth: usize, tally: &mut Tally) {
    let mut entries = volume.entries(dir);
    loop {
        let entry = match entries.next(volume) {
            Ok(Some(entry)) => entry,
            Ok(None) => return,
            Err(err) => {
                warn!("SD: directory unreadable: {:?}", err);
                return;
            }
        };

        if tally.printed < MAX_ENTRIES {
            tally.printed += 1;
            let indent = &"                "[..(depth * 2).min(16)];
            if entry.directory {
                info!("SD: {}{}/", indent, entry.name());
            } else {
                info!("SD: {}{}  {} bytes", indent, entry.name(), entry.size);
            }
        } else {
            tally.truncated = true;
        }

        if entry.directory {
            tally.directories += 1;
            if depth + 1 < MAX_DEPTH
                && let Some(inner) = entry.as_dir()
            {
                walk(volume, inner, depth + 1, tally);
            }
        } else {
            tally.files += 1;
            tally.bytes += u64::from(entry.size);
        }
    }
}

/// What a read costs: one block per command against a run per command, at three bus clocks.
///
/// The two are asked over the **same sectors** at each rate, and every run is checksummed. A
/// bus clock the card does not like does not usually fail -- it returns different bytes, and a
/// checksum that moves between rates is the only thing that says so.
fn measure(volume: &mut Volume<'_>) {
    let start = volume.layout().data_start;
    let blocks = (MEASURE_BYTES / 512) as u32;
    let mut buffer = [0u8; CHUNK];

    for rate in RATES {
        if let Err(err) = volume.card().set_rate(Rate::from_mhz(rate)) {
            warn!("SD: {} MHz refused: {:?}", rate, err);
            continue;
        }

        let began = Instant::now();
        let mut sum = 0u32;
        let mut failed = None;
        for block in 0..blocks {
            if let Err(err) = volume.card().read_block(start + block, (&mut buffer[..512]).try_into().unwrap()) {
                failed = Some(err);
                break;
            }
            sum = checksum(sum, &buffer[..512]);
        }
        report(rate, "one block per command", began, failed.is_none(), sum);

        let began = Instant::now();
        let mut sum = 0u32;
        let mut failed = None;
        let mut block = 0;
        while block < blocks {
            let run = (blocks - block).min((CHUNK / 512) as u32);
            let bytes = run as usize * 512;
            if let Err(err) = volume.card().read_blocks(start + block, &mut buffer[..bytes]) {
                failed = Some(err);
                break;
            }
            sum = checksum(sum, &buffer[..bytes]);
            block += run;
        }
        report(rate, "eight blocks per command", began, failed.is_none(), sum);
    }

    // Back to what the driver hands out by default, so the file read below is measured at the
    // rate the rest of the firmware will actually see.
    if let Err(err) = volume.card().set_rate(sd::FAST_RATE) {
        warn!("SD: could not return to the default rate: {:?}", err);
    }

    // The same again through the filesystem, which adds the chain walk and the copy out of the
    // scratch sector to whatever the block device costs.
    match volume.open(SAMPLE) {
        Ok(mut file) => {
            let size = file.size();
            let began = Instant::now();
            let mut sum = 0u32;
            let mut read = 0usize;
            loop {
                match file.read(volume, &mut buffer) {
                    Ok(0) => break,
                    Ok(taken) => {
                        sum = checksum(sum, &buffer[..taken]);
                        read += taken;
                    }
                    Err(err) => {
                        warn!("SD: {} failed after {} bytes: {:?}", SAMPLE, read, err);
                        return;
                    }
                }
            }
            let elapsed = began.elapsed().as_micros().max(1);
            info!(
                "SD: {} is {} bytes, read {} in {} ms, {} KiB/s, checksum {:08x}",
                SAMPLE,
                size,
                read,
                elapsed / 1000,
                (read as u64 * 1_000_000) / (elapsed * 1024),
                sum
            );
        }
        Err(err) => warn!("SD: {} could not be opened: {:?}", SAMPLE, err),
    }
}

/// One line of the throughput table.
fn report(rate: u32, how: &str, began: Instant, whole: bool, sum: u32) {
    let elapsed = began.elapsed().as_micros().max(1);
    if !whole {
        warn!("SD: {} MHz, {}: the read failed", rate, how);
        return;
    }
    info!(
        "SD: {} MHz, {}: {} KiB in {} ms, {} KiB/s, checksum {:08x}",
        rate,
        how,
        MEASURE_BYTES / 1024,
        elapsed / 1000,
        (MEASURE_BYTES as u64 * 1_000_000) / (elapsed * 1024),
        sum
    );
}

/// A checksum that notices a byte in the wrong place, which a plain sum does not.
fn checksum(seed: u32, bytes: &[u8]) -> u32 {
    let mut sum = seed;
    for &byte in bytes {
        sum = (sum ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    sum
}

/// Nothing else to do; keep the log readable rather than rebooting into it again.
fn idle(delay: Delay) -> ! {
    loop {
        delay.delay_millis(1000);
    }
}
