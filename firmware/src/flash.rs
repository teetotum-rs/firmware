//! Finding partitions at run time.
//!
//! **The table is read, not assumed.** `partitions.csv` puts `nvs` at `0x9000` and `plugins` at
//! `0x810000`, but a board flashed with another table keeps working, and a wrong guess cannot
//! land in somebody's application image.
//!
//! Entries are matched on raw type, subtype and label: the bootloader crate's typed lookup panics
//! on a subtype it does not know, so one foreign entry would take the settings with it. Its
//! `FlashRegion` refuses an erase that ends at the end of the partition, so the lookups hand back
//! a [`Region`] of their own.

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use esp_bootloader_esp_idf::partitions::{self, PartitionEntry};
use esp_storage::FlashStorage;

/// How much scratch space the lookups need to read the partition table into.
pub const TABLE_SCRATCH: usize = partitions::PARTITION_TABLE_MAX_LEN;

/// The label of the partition that holds installed plugins.
pub const PLUGINS: &str = "plugins";

const APP: u8 = 0x00;
const OTA_0: u8 = 0x10;
const DATA: u8 = 0x01;
const OTA_DATA: u8 = 0x00;
const NVS: u8 = 0x02;
const UNDEFINED: u8 = 0x06;

/// The partition table held no such entry, or could not be read at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPartition;

/// Find the `nvs` partition and hand back a region that reads and writes inside it.
///
/// The scratch buffer holds the raw partition table. It is 3 KiB, which is why it is the
/// caller's to place -- a task stack is the wrong home for it.
pub fn nvs<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    scratch: &mut [u8; TABLE_SCRATCH],
) -> Result<Region<'a, 'd>, NoPartition> {
    find(flash, scratch, |entry| {
        entry.raw_type() == DATA && entry.raw_subtype() == NVS
    })
}

/// Find the `plugins` partition, a data partition of subtype `undefined`; see [`nvs`].
pub fn plugins<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    scratch: &mut [u8; TABLE_SCRATCH],
) -> Result<Region<'a, 'd>, NoPartition> {
    find(flash, scratch, |entry| {
        entry.raw_type() == DATA
            && entry.raw_subtype() == UNDEFINED
            && entry.label_as_str() == PLUGINS
    })
}

/// Find the application partition `ota_<n>`; see [`nvs`].
pub fn app<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    scratch: &mut [u8; TABLE_SCRATCH],
    n: u8,
) -> Result<Region<'a, 'd>, NoPartition> {
    find(flash, scratch, |entry| {
        entry.raw_type() == APP && entry.raw_subtype() == OTA_0 + n
    })
}

/// The `otadata` partition, which says the bootloader what to start, as the bootloader crate's
/// [`Ota`](esp_bootloader_esp_idf::ota::Ota) takes it. Writes go through `Storage`, which never
/// meets the refused erase.
pub fn ota_data<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    scratch: &'a mut [u8; TABLE_SCRATCH],
) -> Result<partitions::FlashRegion<'a, FlashStorage<'d>>, NoPartition> {
    let table = partitions::read_partition_table(flash, scratch).map_err(|_| NoPartition)?;
    let entry = table
        .iter()
        .find(|entry| entry.raw_type() == DATA && entry.raw_subtype() == OTA_DATA)
        .ok_or(NoPartition)?;
    Ok(entry.as_embedded_storage(flash))
}

fn find<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    scratch: &mut [u8; TABLE_SCRATCH],
    want: impl Fn(&PartitionEntry<'_>) -> bool,
) -> Result<Region<'a, 'd>, NoPartition> {
    let table = partitions::read_partition_table(flash, scratch).map_err(|_| NoPartition)?;
    let entry = table.iter().find(|entry| want(entry)).ok_or(NoPartition)?;
    Ok(Region {
        offset: entry.offset(),
        size: entry.len(),
        flash,
    })
}

/// One partition of the flash, addressed from its start.
pub struct Region<'a, 'd> {
    flash: &'a mut FlashStorage<'d>,
    offset: u32,
    size: u32,
}

impl<'d> Region<'_, 'd> {
    pub fn partition_size(&self) -> usize {
        self.size as usize
    }

    /// Whether the absolute flash address `address` lies inside the partition.
    pub fn contains(&self, address: u32) -> bool {
        address
            .checked_sub(self.offset)
            .is_some_and(|at| at < self.size)
    }

    /// The flash under this region, to find another partition while this one is held.
    pub fn storage(&mut self) -> &mut FlashStorage<'d> {
        self.flash
    }

    /// The absolute address of `len` bytes at `from`, if they lie inside the partition.
    fn address(&self, from: u32, len: usize) -> Result<u32, NorFlashErrorKind> {
        let end = u32::try_from(len)
            .ok()
            .and_then(|len| from.checked_add(len))
            .ok_or(NorFlashErrorKind::OutOfBounds)?;
        if end > self.size {
            return Err(NorFlashErrorKind::OutOfBounds);
        }
        Ok(self.offset + from)
    }
}

impl ErrorType for Region<'_, '_> {
    type Error = NorFlashErrorKind;
}

impl ReadNorFlash for Region<'_, '_> {
    const READ_SIZE: usize = FlashStorage::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let at = self.address(offset, bytes.len())?;
        self.flash.read(at, bytes).map_err(|e| e.kind())
    }

    fn capacity(&self) -> usize {
        self.partition_size()
    }
}

impl NorFlash for Region<'_, '_> {
    const WRITE_SIZE: usize = FlashStorage::WRITE_SIZE;
    const ERASE_SIZE: usize = FlashStorage::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let len = to.checked_sub(from).ok_or(NorFlashErrorKind::OutOfBounds)?;
        let at = self.address(from, len as usize)?;
        self.flash.erase(at, at + len).map_err(|e| e.kind())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let at = self.address(offset, bytes.len())?;
        self.flash.write(at, bytes).map_err(|e| e.kind())
    }
}
