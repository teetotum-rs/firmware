//! Finding the `nvs` partition at run time.
//!
//! **The table is read, not assumed.** `espflash` writes its own table when it flashes this
//! board, and today that table says `nvs` at `0x9000` for 24 KiB,
//! `phy_init` at `0xf000`, `factory` at `0x10000`. Those offsets are measured and they are still
//! not written down in the code: a board flashed with a different table keeps working, and a
//! wrong guess cannot land in somebody's application image.

use esp_bootloader_esp_idf::partitions::{
    self, DataPartitionSubType, FlashRegion, PartitionType,
};
use esp_storage::FlashStorage;

/// How much scratch space [`nvs`] needs to read the partition table into.
pub const TABLE_SCRATCH: usize = partitions::PARTITION_TABLE_MAX_LEN;

/// The partition table held no `nvs` entry, or could not be read at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPartition;

/// Find the `nvs` partition and hand back a region that reads and writes inside it.
///
/// The scratch buffer holds the raw partition table and has to outlive the region, because the
/// entry the region is built from points into it. It is 3 KiB, which is why it is the caller's
/// to place -- a task stack is the wrong home for it.
pub fn nvs<'a, 'd>(
    flash: &'a mut FlashStorage<'d>,
    scratch: &'a mut [u8; TABLE_SCRATCH],
) -> Result<FlashRegion<'a, FlashStorage<'d>>, NoPartition> {
    let table = partitions::read_partition_table(flash, scratch).map_err(|_| NoPartition)?;
    table
        .find_partition(PartitionType::Data(DataPartitionSubType::Nvs))
        .map_err(|_| NoPartition)?
        .ok_or(NoPartition)
        .map(|entry| entry.as_embedded_storage(flash))
}
