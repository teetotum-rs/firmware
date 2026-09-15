//! Write the card through a filesystem: make a directory and a file with long names, check them
//! with the read-only reader, then remove both.
//!
//! `sdwrite` showed the block layer writes where it is asked. This puts `fatfs` on top of it and
//! checks the result with [`teetotum::fat`], which shares no code with `fatfs`: the names (one
//! with a non-ASCII character), a file spanning several clusters, and three timestamps from
//! three sources -- the directory's from the clock handed to `fatfs`, the file's created and
//! modified stamps set per file. Afterwards the root lists what it listed before.
//!
//! The last line is `fatwrite: PASS` or `fatwrite: FAIL`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use fatfs::Write;
use log::{error, info};
use teetotum::fat::{self, SECTOR, Stamp, Volume};
use teetotum::sd::{self, SdCard};
use teetotum_firmware::storage::{self, date_time};

esp_bootloader_esp_idf::esp_app_desc!();

const DIR: &str = "Teetotum write test";
const FILE: &str = "Long name with \u{fc}mlaut, checked.txt";
const PATH: &str = "Teetotum write test/Long name with \u{fc}mlaut, checked.txt";
/// Three clusters and a bit on this card, and not a multiple of a sector.
const FILE_BYTES: usize = 10_000;

/// What the clock handed to `fatfs` says, so the directory's stamp can be told from the file's.
const CLOCK: Stamp = Stamp {
    year: 2026,
    month: 9,
    day: 15,
    hour: 12,
    minute: 0,
    second: 0,
};
const CREATED: Stamp = Stamp {
    year: 2026,
    month: 9,
    day: 15,
    hour: 16,
    minute: 10,
    second: 24,
};
const MODIFIED: Stamp = Stamp {
    year: 2024,
    month: 3,
    day: 1,
    hour: 9,
    minute: 30,
    second: 0,
};

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    esp_alloc::heap_allocator!(size: 64 * 1024);
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

    let passed = match check("card", SdCard::new(spi, cs, delay)) {
        Ok(card) => run(card).is_ok(),
        Err(()) => false,
    };
    info!("fatwrite: {}", if passed { "PASS" } else { "FAIL" });

    loop {
        delay.delay_millis(1000);
    }
}

fn run(card: SdCard<'_>) -> Result<(), ()> {
    let mut volume = check("mount", Volume::mount(card))?;
    let partition = volume.layout().partition_start;
    let before = check("root listing", count_root(&mut volume))?;
    let flags = volume_flags(&mut volume)?;
    if volume.entry(DIR).is_ok() {
        error!(
            "FW: {:?} already exists, left over from an earlier run",
            DIR
        );
        return Err(());
    }
    info!(
        "FW: root lists {} entries, {:?} is absent, volume flags {:#04x}",
        before, DIR, flags
    );

    // Where the storage reports a sector it could not write back when dropped.
    let failure = Cell::new(None);
    let contents: Vec<u8> = (0..FILE_BYTES).map(|i| (i * 7 % 251) as u8).collect();

    let mut card = volume.into_card();
    {
        let fs = check(
            "fatfs mount",
            storage::mount(&mut card, partition, date_time(CLOCK), &failure),
        )?;
        {
            let dir = check("create_dir", fs.root_dir().create_dir(DIR))?;
            let mut file = check("create_file", dir.create_file(FILE))?;
            for chunk in contents.chunks(1000) {
                check("write", file.write_all(chunk))?;
            }
            file.set_created(date_time(CREATED));
            file.set_modified(date_time(MODIFIED));
            check("flush", file.flush())?;
        }
        check("fatfs unmount", fs.unmount())?;
    }
    written_back(&failure)?;
    info!("FW: fatfs made the directory and wrote the file");

    let mut volume = check("remount", Volume::mount(card))?;
    same_flags(&mut volume, flags)?;
    let dir = check("lookup of the directory", volume.entry(DIR))?;
    info!(
        "FW: found {:?}, directory {}, created {:?}",
        dir.name(),
        dir.directory,
        dir.created
    );
    let entry = check("lookup of the file", volume.entry(PATH))?;
    info!(
        "FW: found {:?}, {} bytes, created {:?}, modified {:?}",
        entry.name(),
        entry.size,
        entry.created,
        entry.modified
    );
    if dir.name() != DIR || !dir.directory || dir.created != Some(CLOCK) {
        error!("FW: the directory is not what was made");
        return Err(());
    }
    if entry.name() != FILE
        || entry.size as usize != FILE_BYTES
        || entry.created != Some(CREATED)
        || entry.modified != Some(MODIFIED)
    {
        error!("FW: the file entry is not what was written");
        return Err(());
    }
    let mut file = entry.open().ok_or(())?;
    let mut back = vec![0u8; FILE_BYTES];
    check("read back", file.read_exact(&mut volume, &mut back))?;
    if back != contents {
        error!("FW: the file reads back different bytes");
        return Err(());
    }
    info!("FW: names, stamps and contents check out through the read-only reader");

    let mut card = volume.into_card();
    {
        let fs = check(
            "fatfs mount",
            storage::mount(&mut card, partition, date_time(CLOCK), &failure),
        )?;
        check("remove file", fs.root_dir().remove(PATH))?;
        check("remove directory", fs.root_dir().remove(DIR))?;
        check("fatfs unmount", fs.unmount())?;
    }
    written_back(&failure)?;

    let mut volume = check("remount", Volume::mount(card))?;
    same_flags(&mut volume, flags)?;
    match volume.entry(DIR) {
        Err(fat::Error::NotFound) => {}
        other => {
            error!("FW: {:?} is still there: {:?}", DIR, other.is_ok());
            return Err(());
        }
    }
    let after = check("root listing", count_root(&mut volume))?;
    if after != before {
        error!("FW: root lists {} entries, {} before", after, before);
        return Err(());
    }
    info!("FW: removed again, root lists {} entries as before", after);
    Ok(())
}

/// Log a failure where it happens and hand on only that it happened.
fn check<T, E: core::fmt::Debug>(what: &str, result: Result<T, E>) -> Result<T, ()> {
    result.map_err(|err| error!("FW: {} failed: {:?}", what, err))
}

/// The flags byte of the boot sector, whose bit 0 marks a volume that was not cleanly unmounted.
fn volume_flags(volume: &mut Volume<'_>) -> Result<u8, ()> {
    let layout = *volume.layout();
    let mut boot = [0u8; SECTOR];
    check(
        "boot sector",
        volume.card().read_block(layout.partition_start, &mut boot),
    )?;
    Ok(boot[if layout.fat32 { 0x41 } else { 0x25 }])
}

fn same_flags(volume: &mut Volume<'_>, before: u8) -> Result<(), ()> {
    let after = volume_flags(volume)?;
    if after != before {
        error!("FW: volume flags {:#04x}, {:#04x} before", after, before);
        return Err(());
    }
    Ok(())
}

fn written_back(failure: &Cell<Option<sd::Error>>) -> Result<(), ()> {
    if let Some(err) = failure.take() {
        error!(
            "FW: the last cached sector never reached the card: {:?}",
            err
        );
        return Err(());
    }
    Ok(())
}

fn count_root(volume: &mut Volume<'_>) -> Result<usize, fat::Error> {
    let root = volume.root();
    let mut entries = volume.entries(root);
    let mut count = 0;
    while entries.next(volume)?.is_some() {
        count += 1;
    }
    Ok(count)
}
