//! Updating the firmware over BLE: a signed image into the application partition that is not
//! running.
//!
//! A sender writes [`crate::upload::UPDATE`] with the image's length and signature, the image in
//! pieces as for a plugin, and [`crate::upload::COMMIT`]. Each sector is erased when the first
//! piece reaches it, so no step blocks for the whole partition. The signature is checked over the
//! bytes as they come ([`teetotum_pack::firmware`]); only when it holds at the commit does
//! `otadata` point at the new partition. Until then the running firmware stays selected, and an
//! upload that breaks off leaves nothing to undo.

use embedded_storage::nor_flash::{NorFlash, NorFlashErrorKind};
use esp_bootloader_esp_idf::ota::{Ota, OtaImageState};
use esp_bootloader_esp_idf::partitions::AppPartitionSubType;
use esp_storage::FlashStorage;
use teetotum_pack::firmware::{self, Verifier};

use crate::flash::{self, TABLE_SCRATCH};

/// The public key of `~/.config/teetotum/firmware-key.pem`.
pub const KEY: [u8; firmware::KEY] = [
    0xa1, 0x30, 0x67, 0x62, 0x8f, 0x6e, 0x2d, 0x70, 0xff, 0x8d, 0xc9, 0x8c, 0x65, 0x53, 0x1c, 0x93,
    0xb0, 0x55, 0x08, 0xbd, 0xc4, 0x14, 0xa4, 0xd3, 0x64, 0x3b, 0x75, 0xdb, 0x06, 0x3e, 0x7a, 0x69,
];

const OTA_PARTITIONS: usize = 2;
const SECTOR: usize = FlashStorage::ERASE_SIZE;
const UNIT: usize = FlashStorage::WRITE_SIZE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The address the code runs from lies in no application partition, so the partition to
    /// write cannot be told apart from the running one.
    NotRunning,
    NoPartition,
    /// Longer than the partition, or than announced, or shorter at the commit.
    Length,
    Image(firmware::Error),
    Flash(NorFlashErrorKind),
    Ota,
}

/// An update under way.
pub struct Update {
    target: u8,
    len: usize,
    received: usize,
    erased: usize,
    verifier: Verifier,
    carry: [u8; UNIT],
    carried: usize,
}

unsafe extern "C" {
    /// The ROM's lookup from a mapped address to the flash address behind it; `u32::MAX` if the
    /// address is not mapped from flash.
    fn spi_flash_cache2phys(cached: *const core::ffi::c_void) -> u32;
}

/// The flash address this code runs from.
pub fn running_address() -> Option<u32> {
    // SAFETY: the ROM function only reads the MMU table for the address it is given.
    let address = unsafe { spi_flash_cache2phys(running_address as *const core::ffi::c_void) };
    (address != u32::MAX).then_some(address)
}

/// The OTA partition number the firmware runs from.
pub fn running(flash: &mut FlashStorage<'_>, table: &mut [u8; TABLE_SCRATCH]) -> Option<u8> {
    let address = running_address()?;
    (0..OTA_PARTITIONS as u8)
        .find(|&n| flash::app(flash, table, n).is_ok_and(|region| region.contains(address)))
}

impl Update {
    /// Starts an update of `len` bytes into the partition that is not running.
    pub fn begin(
        flash: &mut FlashStorage<'_>,
        table: &mut [u8; TABLE_SCRATCH],
        len: usize,
        signature: &[u8; firmware::SIGNATURE],
    ) -> Result<Self, Error> {
        let running = running(flash, table).ok_or(Error::NotRunning)?;
        let target = (running + 1) % OTA_PARTITIONS as u8;
        let region = flash::app(flash, table, target).map_err(|_| Error::NoPartition)?;
        if len == 0 || len > region.partition_size() {
            return Err(Error::Length);
        }
        Ok(Self {
            target,
            len,
            received: 0,
            erased: 0,
            verifier: Verifier::new(&KEY, signature).map_err(Error::Image)?,
            carry: [0xff; UNIT],
            carried: 0,
        })
    }

    pub fn target(&self) -> u8 {
        self.target
    }

    pub fn received(&self) -> usize {
        self.received
    }

    pub fn total(&self) -> usize {
        self.len
    }

    /// Writes the next bytes of the image. Whole write units go to flash straight away; what is
    /// left of one waits for the next piece.
    pub fn feed(
        &mut self,
        flash: &mut FlashStorage<'_>,
        table: &mut [u8; TABLE_SCRATCH],
        bytes: &[u8],
    ) -> Result<(), Error> {
        let end = self.received + bytes.len();
        if end > self.len {
            return Err(Error::Length);
        }
        self.verifier.absorb(bytes).map_err(Error::Image)?;
        let mut region = flash::app(flash, table, self.target).map_err(|_| Error::NoPartition)?;
        let needed = end.div_ceil(SECTOR) * SECTOR;
        if needed > self.erased {
            region
                .erase(self.erased as u32, needed as u32)
                .map_err(Error::Flash)?;
            self.erased = needed;
        }
        let mut rest = bytes;
        let mut at = self.received - self.carried;
        if self.carried > 0 {
            let take = (UNIT - self.carried).min(rest.len());
            self.carry[self.carried..self.carried + take].copy_from_slice(&rest[..take]);
            self.carried += take;
            rest = &rest[take..];
            if self.carried == UNIT {
                region.write(at as u32, &self.carry).map_err(Error::Flash)?;
                at += UNIT;
                self.carry = [0xff; UNIT];
                self.carried = 0;
            }
        }
        let whole = rest.len() / UNIT * UNIT;
        if whole > 0 {
            region
                .write(at as u32, &rest[..whole])
                .map_err(Error::Flash)?;
        }
        let left = rest.len() - whole;
        if left > 0 {
            self.carry[..left].copy_from_slice(&rest[whole..]);
            self.carried = left;
        }
        self.received = end;
        Ok(())
    }

    /// Writes the last bytes and, if all came and the signature holds, selects the new partition
    /// for the next boot.
    pub fn finish(
        self,
        flash: &mut FlashStorage<'_>,
        table: &mut [u8; TABLE_SCRATCH],
    ) -> Result<u8, Error> {
        if self.received != self.len {
            return Err(Error::Length);
        }
        self.verifier.verify().map_err(Error::Image)?;
        if self.carried > 0 {
            let mut region =
                flash::app(flash, table, self.target).map_err(|_| Error::NoPartition)?;
            region
                .write((self.received - self.carried) as u32, &self.carry)
                .map_err(Error::Flash)?;
        }
        let target = match self.target {
            0 => AppPartitionSubType::Ota0,
            _ => AppPartitionSubType::Ota1,
        };
        let region = flash::ota_data(flash, table).map_err(|_| Error::NoPartition)?;
        let mut ota = Ota::new(region, OTA_PARTITIONS).map_err(|_| Error::Ota)?;
        // From blank otadata the crate's step to ota_1 lands on sequence 0; through ota_0 it
        // does not.
        if ota.current_app_partition().map_err(|_| Error::Ota)? == AppPartitionSubType::Factory {
            ota.set_current_app_partition(AppPartitionSubType::Ota0)
                .map_err(|_| Error::Ota)?;
        }
        ota.set_current_app_partition(target)
            .map_err(|_| Error::Ota)?;
        ota.set_current_ota_state(OtaImageState::New)
            .map_err(|_| Error::Ota)?;
        Ok(self.target)
    }
}

/// How the running image stands in `otadata`, and whether it was marked valid just now: an image
/// that got this far started, so a bootloader with rollback keeps it.
pub fn confirm(
    flash: &mut FlashStorage<'_>,
    table: &mut [u8; TABLE_SCRATCH],
) -> Result<(OtaImageState, bool), Error> {
    let region = flash::ota_data(flash, table).map_err(|_| Error::NoPartition)?;
    let mut ota = Ota::new(region, OTA_PARTITIONS).map_err(|_| Error::Ota)?;
    let state = ota.current_ota_state().map_err(|_| Error::Ota)?;
    if matches!(state, OtaImageState::New | OtaImageState::PendingVerify) {
        ota.set_current_ota_state(OtaImageState::Valid)
            .map_err(|_| Error::Ota)?;
        return Ok((state, true));
    }
    Ok((state, false))
}
