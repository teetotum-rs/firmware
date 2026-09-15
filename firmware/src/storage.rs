//! The card as `fatfs` wants it: a seekable byte stream over the block driver.
//!
//! [`teetotum::fat`] only reads; writing goes through `fatfs`, which creates long names, extends
//! cluster chains and keeps both FAT copies. The two keep separate caches, so a writer takes the
//! card out of the reader's [`Volume`](teetotum::fat::Volume) and the reader mounts again after.
//!
//! `fatfs` never flushes its storage on unmount. [`Storage`] therefore writes its cached sector
//! back when it is dropped, and leaves a failure there in the cell it was given.

use core::cell::Cell;

use fatfs::{
    Date, DateTime, FileSystem, FsOptions, IoBase, IoError, LossyOemCpConverter, Read, Seek,
    SeekFrom, Time, TimeProvider, Write,
};
use log::error;
use teetotum::fat::{SECTOR, Stamp};
use teetotum::sd::{self, SdCard};

/// A writing mount of the card.
pub type Fs<'c, 'd> = FileSystem<Storage<'c, 'd>, Clock, LossyOemCpConverter>;
/// A directory on a writing mount.
pub type FsDir<'a, 'c, 'd> = fatfs::Dir<'a, Storage<'c, 'd>, Clock, LossyOemCpConverter>;
/// A file on a writing mount.
pub type FsFile<'a, 'c, 'd> = fatfs::File<'a, Storage<'c, 'd>, Clock, LossyOemCpConverter>;
/// Why a writing operation did not finish.
pub type FsError = fatfs::Error<StorageError>;

/// Mounts the filesystem whose first sector is `start` for writing. New entries get `clock`'s
/// time, since the Knob has no clock of its own.
pub fn mount<'c, 'd>(
    card: &'c mut SdCard<'d>,
    start: u32,
    clock: DateTime,
    failure: &'c Cell<Option<sd::Error>>,
) -> Result<Fs<'c, 'd>, FsError> {
    let options = FsOptions::new().time_provider(Clock(clock));
    FileSystem::new(Storage::new(card, start, failure), options)
}

/// A reader's timestamp as `fatfs` spells it.
pub fn date_time(stamp: Stamp) -> DateTime {
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

/// A clock that stands still at one moment.
#[derive(Clone, Copy, Debug)]
pub struct Clock(pub DateTime);

impl TimeProvider for Clock {
    fn get_current_date(&self) -> Date {
        self.0.date
    }

    fn get_current_date_time(&self) -> DateTime {
        self.0
    }
}

/// The card as a byte stream starting at the partition's first sector.
///
/// Whole sectors from a sector boundary go to the card as one run; anything else passes through
/// one cached sector, written back when a different sector is needed, on flush, or on drop.
pub struct Storage<'c, 'd> {
    card: &'c mut SdCard<'d>,
    start: u32,
    position: u64,
    sector: [u8; SECTOR],
    /// The cached sector, counted from `start`.
    cached: Option<u32>,
    dirty: bool,
    /// Where a write-back that failed on drop is reported.
    failure: &'c Cell<Option<sd::Error>>,
}

#[derive(Debug)]
pub enum StorageError {
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
    pub fn new(card: &'c mut SdCard<'d>, start: u32, failure: &'c Cell<Option<sd::Error>>) -> Self {
        Storage {
            card,
            start,
            position: 0,
            sector: [0u8; SECTOR],
            cached: None,
            dirty: false,
            failure,
        }
    }

    /// The sector the position is in, and the offset into it.
    fn here(&self) -> (u32, usize) {
        (
            (self.position / SECTOR as u64) as u32,
            (self.position % SECTOR as u64) as usize,
        )
    }

    fn load(&mut self, index: u32) -> Result<(), sd::Error> {
        if self.cached == Some(index) {
            return Ok(());
        }
        self.write_back()?;
        self.card.read_block(self.start + index, &mut self.sector)?;
        self.cached = Some(index);
        Ok(())
    }

    fn write_back(&mut self) -> Result<(), sd::Error> {
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
            error!("Storage: writing the cached sector back failed: {err:?}");
            self.failure.set(Some(err));
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
        Ok(self.write_back()?)
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
