//! Write to the TF card where nothing lives: a run of clusters the allocation table marks free.
//!
//! Before a filesystem writes to the card, the block layer underneath has to be shown to put
//! bytes where it was asked and nowhere else. This picks free clusters at the top of the volume,
//! keeps what their sectors hold, writes known patterns one block at a time and as a run, reads
//! them back, times both, and writes the original bytes back. The table is checked before and
//! after, and a root listing afterwards shows the volume still mounts.
//!
//! The last line is `sdwrite: PASS` or `sdwrite: FAIL`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Instant;
use log::{error, info};
use teetotum::fat::{Layout, SECTOR, Volume};
use teetotum::sd::{self, SdCard};

esp_bootloader_esp_idf::esp_app_desc!();

/// Sectors in the test run. Three copies of it live on the heap at once.
const RUN_SECTORS: u32 = 64;
const RUN_BYTES: usize = RUN_SECTORS as usize * SECTOR;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(size: 128 * 1024);
    let delay = Delay::new();

    let spi = Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(sd::INIT_RATE),
    )
    .expect("the card SPI peripheral could not be configured")
    .with_sck(peripherals.GPIO4)
    .with_mosi(peripherals.GPIO3)
    .with_miso(peripherals.GPIO5);
    let cs = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());

    let passed = match SdCard::new(spi, cs, delay) {
        Ok(card) => match Volume::mount(card) {
            Ok(mut volume) => run(&mut volume),
            Err(err) => {
                error!("SDW: no filesystem: {:?}", err);
                false
            }
        },
        Err(err) => {
            error!("SDW: the card did not come up: {:?}", err);
            false
        }
    };
    info!("sdwrite: {}", if passed { "PASS" } else { "FAIL" });

    loop {
        delay.delay_millis(1000);
    }
}

fn run(volume: &mut Volume<'_>) -> bool {
    let layout = *volume.layout();
    info!(
        "SDW: {} sectors per cluster, {} clusters, tables at LBA {}, data at LBA {}",
        layout.sectors_per_cluster, layout.clusters, layout.fat_start, layout.data_start
    );
    let needed = RUN_SECTORS.div_ceil(layout.sectors_per_cluster);

    let mut table = Table::new(layout);
    let first = match free_run(volume.card(), &mut table, needed) {
        Ok(Some(first)) => first,
        Ok(None) => {
            error!("SDW: no run of {} free clusters", needed);
            return false;
        }
        Err(err) => {
            error!("SDW: table unreadable: {:?}", err);
            return false;
        }
    };
    let lba = layout.data_start + (first - 2) * layout.sectors_per_cluster;
    info!(
        "SDW: clusters {}..{} are free, sectors {}..{}",
        first,
        first + needed - 1,
        lba,
        lba + RUN_SECTORS - 1
    );

    let card = volume.card();
    let mut original = vec![0u8; RUN_BYTES];
    if let Err(err) = card.read_blocks(lba, &mut original) {
        error!("SDW: could not keep the run's contents: {:?}", err);
        return false;
    }

    let written = patterns(card, lba);

    let mut restored = true;
    let mut back = vec![0u8; RUN_BYTES];
    match card
        .write_blocks(lba, &original)
        .and_then(|()| card.read_blocks(lba, &mut back))
    {
        Ok(()) if back == original => info!("SDW: original contents back in place"),
        Ok(()) => {
            error!("SDW: restore read back different bytes");
            restored = false;
        }
        Err(err) => {
            error!("SDW: restore failed: {:?}", err);
            restored = false;
        }
    }

    let mut still_free = true;
    table.forget();
    for cluster in first..first + needed {
        match table.entry(volume.card(), cluster) {
            Ok(0) => {}
            Ok(entry) => {
                error!("SDW: cluster {} now links to {:#x}", cluster, entry);
                still_free = false;
            }
            Err(err) => {
                error!("SDW: table unreadable afterwards: {:?}", err);
                still_free = false;
            }
        }
    }
    if still_free {
        info!("SDW: the table still marks the run free");
    }

    let listed = list_root(volume);
    written && restored && still_free && listed
}

/// Write three patterns and read each back: one block, the run in one command, the run block by
/// block. Timings are for the write alone.
fn patterns(card: &mut SdCard<'_>, lba: u32) -> bool {
    let mut back = vec![0u8; RUN_BYTES];

    let mut block = [0u8; SECTOR];
    fill(&mut block, lba, 0xA5);
    let mut one = [0u8; SECTOR];
    match card
        .write_block(lba, &block)
        .and_then(|()| card.read_block(lba, &mut one))
    {
        Ok(()) if one == block => info!("SDW: single block written and read back"),
        Ok(()) => {
            error!("SDW: single block read back different bytes");
            return false;
        }
        Err(err) => {
            error!("SDW: single block failed: {:?}", err);
            return false;
        }
    }

    let mut run = vec![0u8; RUN_BYTES];
    for (index, sector) in run.chunks_exact_mut(SECTOR).enumerate() {
        fill(sector, lba + index as u32, 0x3C);
    }
    let began = Instant::now();
    let result = card.write_blocks(lba, &run);
    let micros = began.elapsed().as_micros().max(1);
    match result.and_then(|()| card.read_blocks(lba, &mut back)) {
        Ok(()) if back == run => info!(
            "SDW: run of {} sectors in one command: {} ms, {} KiB/s",
            RUN_SECTORS,
            micros / 1000,
            rate(micros)
        ),
        Ok(()) => {
            error!("SDW: run read back different bytes");
            return false;
        }
        Err(err) => {
            error!("SDW: run failed: {:?}", err);
            return false;
        }
    }

    for (index, sector) in run.chunks_exact_mut(SECTOR).enumerate() {
        fill(sector, lba + index as u32, 0x5A);
    }
    let began = Instant::now();
    let mut result = Ok(());
    for (index, sector) in run.chunks_exact(SECTOR).enumerate() {
        let sector: &[u8; SECTOR] = sector.try_into().unwrap();
        result = card.write_block(lba + index as u32, sector);
        if result.is_err() {
            break;
        }
    }
    let micros = began.elapsed().as_micros().max(1);
    match result.and_then(|()| card.read_blocks(lba, &mut back)) {
        Ok(()) if back == run => info!(
            "SDW: run of {} sectors block by block: {} ms, {} KiB/s",
            RUN_SECTORS,
            micros / 1000,
            rate(micros)
        ),
        Ok(()) => {
            error!("SDW: block-by-block run read back different bytes");
            return false;
        }
        Err(err) => {
            error!("SDW: block-by-block run failed: {:?}", err);
            return false;
        }
    }
    true
}

/// Bytes that differ from sector to sector and carry their own address, so a block landing in
/// the wrong place reads back wrong.
fn fill(sector: &mut [u8], lba: u32, seed: u8) {
    for (index, byte) in sector.iter_mut().enumerate() {
        *byte = (lba.wrapping_mul(31) ^ index as u32) as u8 ^ seed;
    }
    sector[..4].copy_from_slice(&lba.to_le_bytes());
}

fn rate(micros: u64) -> u64 {
    (RUN_BYTES as u64 * 1_000_000) / (micros * 1024)
}

/// The highest run of `needed` consecutive free clusters, by its first cluster.
fn free_run(
    card: &mut SdCard<'_>,
    table: &mut Table,
    needed: u32,
) -> Result<Option<u32>, sd::Error> {
    let mut run = 0;
    let mut cluster = table.layout.clusters + 1;
    while cluster >= 2 {
        if table.entry(card, cluster)? == 0 {
            run += 1;
            if run == needed {
                return Ok(Some(cluster));
            }
        } else {
            run = 0;
        }
        cluster -= 1;
    }
    Ok(None)
}

/// The first allocation table, read a sector at a time.
struct Table {
    layout: Layout,
    sector: [u8; SECTOR],
    lba: Option<u32>,
}

impl Table {
    fn new(layout: Layout) -> Table {
        Table {
            layout,
            sector: [0u8; SECTOR],
            lba: None,
        }
    }

    fn forget(&mut self) {
        self.lba = None;
    }

    fn entry(&mut self, card: &mut SdCard<'_>, cluster: u32) -> Result<u32, sd::Error> {
        let width = if self.layout.fat32 { 4 } else { 2 };
        let offset = cluster * width;
        let lba = self.layout.fat_start + offset / SECTOR as u32;
        if self.lba != Some(lba) {
            card.read_block(lba, &mut self.sector)?;
            self.lba = Some(lba);
        }
        let at = (offset % SECTOR as u32) as usize;
        Ok(if self.layout.fat32 {
            u32::from_le_bytes(self.sector[at..at + 4].try_into().unwrap()) & 0x0FFF_FFFF
        } else {
            u32::from(u16::from_le_bytes([self.sector[at], self.sector[at + 1]]))
        })
    }
}

/// Count the root directory's entries, which reads the volume through the filesystem layer.
fn list_root(volume: &mut Volume<'_>) -> bool {
    let root = volume.root();
    let mut entries = volume.entries(root);
    let mut count = 0;
    loop {
        match entries.next(volume) {
            Ok(Some(_)) => count += 1,
            Ok(None) => break,
            Err(err) => {
                error!("SDW: root unreadable afterwards: {:?}", err);
                return false;
            }
        }
    }
    info!("SDW: root still lists {} entries", count);
    true
}
