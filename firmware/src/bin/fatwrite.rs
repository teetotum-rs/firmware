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
use core::sync::atomic::{AtomicBool, Ordering};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use fatfs::{
    Date, DateTime, FileSystem, FsOptions, IoBase, IoError, Read, Seek, SeekFrom, Time,
    TimeProvider, Write,
};
use log::{error, info};
use teetotum::fat::{self, SECTOR, Stamp, Volume};
use teetotum::sd::{self, SdCard};

esp_bootloader_esp_idf::esp_app_desc!();

const DIR: &str = "Teetotum write test";
const FILE: &str = "Long name with \u{fc}mlaut, checked.txt";
const PATH: &str = "Teetotum write test/Long name with \u{fc}mlaut, checked.txt";
/// Three clusters and a bit on this card, and not a multiple of a sector.
const FILE_BYTES: usize = 10_000;

/// Set when [`Storage`] could not write its last sector back on drop, which `fatfs` relies on:
/// its unmount writes the clean flag and never flushes.
static LOST_WRITE: AtomicBool = AtomicBool::new(false);

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

    let contents: Vec<u8> = (0..FILE_BYTES).map(|i| (i * 7 % 251) as u8).collect();

    let mut card = volume.into_card();
    {
        let options = FsOptions::new().time_provider(Clock);
        let fs = check(
            "fatfs mount",
            FileSystem::new(Storage::new(&mut card, partition), options),
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
    written_back()?;
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
        let options = FsOptions::new().time_provider(Clock);
        let fs = check(
            "fatfs mount",
            FileSystem::new(Storage::new(&mut card, partition), options),
        )?;
        check("remove file", fs.root_dir().remove(PATH))?;
        check("remove directory", fs.root_dir().remove(DIR))?;
        check("fatfs unmount", fs.unmount())?;
    }
    written_back()?;

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

fn written_back() -> Result<(), ()> {
    if LOST_WRITE.load(Ordering::Relaxed) {
        error!("FW: the last cached sector never reached the card");
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

fn date_time(stamp: Stamp) -> DateTime {
    DateTime::new(
        Date::new(stamp.year, stamp.month.into(), stamp.day.into()),
        Time::new(
            stamp.hour.into(),
            stamp.minute.into(),
            stamp.second.into(),
            0,
        ),
    )
}

#[derive(Debug)]
struct Clock;

impl TimeProvider for Clock {
    fn get_current_date(&self) -> Date {
        date_time(CLOCK).date
    }

    fn get_current_date_time(&self) -> DateTime {
        date_time(CLOCK)
    }
}

/// The card as the seekable byte stream `fatfs` expects, starting at the partition's first
/// sector.
///
/// Whole sectors from a sector boundary go to the card as one run; anything else passes through
/// one cached sector, written back when a different sector is needed or on flush.
struct Storage<'c, 'd> {
    card: &'c mut SdCard<'d>,
    start: u32,
    position: u64,
    sector: [u8; SECTOR],
    /// The cached sector, counted from `start`.
    cached: Option<u32>,
    dirty: bool,
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "the fields are read through Debug when a failure is logged"
)]
enum StorageError {
    Card(sd::Error),
    UnexpectedEof,
    WriteZero,
    Seek,
}

impl From<sd::Error> for StorageError {
    fn from(error: sd::Error) -> Self {
        StorageError::Card(error)
    }
}

impl IoError for StorageError {
    fn is_interrupted(&self) -> bool {
        false
    }

    fn new_unexpected_eof_error() -> Self {
        StorageError::UnexpectedEof
    }

    fn new_write_zero_error() -> Self {
        StorageError::WriteZero
    }
}

impl<'c, 'd> Storage<'c, 'd> {
    fn new(card: &'c mut SdCard<'d>, start: u32) -> Self {
        Storage {
            card,
            start,
            position: 0,
            sector: [0u8; SECTOR],
            cached: None,
            dirty: false,
        }
    }

    /// The sector the position is in, and the offset into it.
    fn here(&self) -> (u32, usize) {
        (
            (self.position / SECTOR as u64) as u32,
            (self.position % SECTOR as u64) as usize,
        )
    }

    fn load(&mut self, index: u32) -> Result<(), StorageError> {
        if self.cached == Some(index) {
            return Ok(());
        }
        self.write_back()?;
        self.card.read_block(self.start + index, &mut self.sector)?;
        self.cached = Some(index);
        Ok(())
    }

    fn write_back(&mut self) -> Result<(), StorageError> {
        if let (true, Some(index)) = (self.dirty, self.cached) {
            self.card.write_block(self.start + index, &self.sector)?;
            self.dirty = false;
        }
        Ok(())
    }
}

impl Drop for Storage<'_, '_> {
    fn drop(&mut self) {
        if let Err(err) = self.write_back() {
            error!("FW: writing the cached sector back failed: {:?}", err);
            LOST_WRITE.store(true, Ordering::Relaxed);
        }
    }
}

impl IoBase for Storage<'_, '_> {
    type Error = StorageError;
}

impl Read for Storage<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, StorageError> {
        let (index, offset) = self.here();
        let whole = buf.len() / SECTOR * SECTOR;
        let count = if offset == 0 && whole > 0 {
            self.write_back()?;
            self.card
                .read_blocks(self.start + index, &mut buf[..whole])?;
            whole
        } else {
            self.load(index)?;
            let count = buf.len().min(SECTOR - offset);
            buf[..count].copy_from_slice(&self.sector[offset..offset + count]);
            count
        };
        self.position += count as u64;
        Ok(count)
    }
}

impl Write for Storage<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, StorageError> {
        let (index, offset) = self.here();
        let whole = buf.len() / SECTOR * SECTOR;
        let count = if offset == 0 && whole > 0 {
            let sectors = (whole / SECTOR) as u32;
            if let Some(cached) = self.cached
                && (index..index + sectors).contains(&cached)
            {
                // The run replaces the cached sector, pending changes included.
                self.cached = None;
                self.dirty = false;
            }
            self.card.write_blocks(self.start + index, &buf[..whole])?;
            whole
        } else {
            self.load(index)?;
            let count = buf.len().min(SECTOR - offset);
            self.sector[offset..offset + count].copy_from_slice(&buf[..count]);
            self.dirty = true;
            count
        };
        self.position += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        self.write_back()
    }
}

impl Seek for Storage<'_, '_> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, StorageError> {
        self.position = match pos {
            SeekFrom::Start(to) => to,
            SeekFrom::Current(by) => self
                .position
                .checked_add_signed(by)
                .ok_or(StorageError::Seek)?,
            SeekFrom::End(_) => return Err(StorageError::Seek),
        };
        Ok(self.position)
    }
}
