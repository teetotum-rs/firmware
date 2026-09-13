//! A small settings store on the `nvs` partition.
//!
//! The firmware has settings that must survive a reboot -- the orientation of the picture, which
//! chip owns the knob, which backdrop was on screen -- and until now every one of them was a
//! constant in `main.rs`. A constant is honest while nobody can change it and a lie the moment a
//! settings page exists, so the page needs somewhere to write.
//!
//! **The partition is found, not assumed** (read from the device). `espflash` writes
//! its own table when it flashes, and on this board that table is `nvs` at `0x9000` for 24 KiB,
//! `phy_init` at `0xf000`, `factory` at `0x10000`. Those numbers are measured and they are still
//! not hard-coded here: [`esp_bootloader_esp_idf::partitions`] reads the table at run time and
//! hands out a region, and this module takes whatever [`embedded_storage::nor_flash::NorFlash`]
//! it is handed. **Finding the partition is the firmware's job, not the store's** -- and not a
//! plugin's either. A plugin that could search the partition table for itself could also find
//! the application image; it gets a region from the firmware instead, and this type is what it
//! puts in it.
//!
//! **This is not the ESP-IDF NVS format.** The partition carries the name because that is what
//! the table calls it, but nothing in this firmware speaks IDF's key-value log, and an IDF tool
//! pointed at this partition will find garbage. Writing a compatible log would cost pages,
//! entry types and two CRC schemes to gain interoperability with a stack we do not run.
//!
//! **Two sectors, alternating** -- because a flash sector must be erased before it is written,
//! and between the erase and the write the old value is already gone. A power cut in that window
//! costs every setting. So a save never touches the sector it just read from: it writes the new
//! record into the *other* one with the sequence number raised by one, and a load takes whichever
//! of the two carries the higher sequence and a matching checksum. The worst a power cut can do
//! is lose the save that was in flight.
//!
//! The record is 16 bytes of header and then the payload:
//!
//! ```text
//! 0..4    magic, `TTSt`
//! 4       format version
//! 5       reserved, zero
//! 6..8    payload length, little endian
//! 8..12   sequence number, little endian
//! 12..16  CRC-32 over bytes 0..12 and the payload
//! 16..    payload
//! ```
//!
//! Both halves are handled in whole words. The header is sixteen bytes, the payload is read
//! rounded up and written padded with zeroes past its recorded length, and [`MAX_PAYLOAD`] is a
//! multiple of four -- because a NOR flash does not read or write a stray byte, it refuses the
//! whole call.
//!
//! A record whose magic, version, length or checksum does not fit is not a record. Erased flash
//! reads as `0xff` everywhere, which fails the magic on the first byte -- so a fresh partition
//! needs no initialisation and no separate "empty" marker.

use embedded_storage::nor_flash::{NorFlash, NorFlashError, NorFlashErrorKind};

/// Header bytes in front of every payload.
pub const HEADER: usize = 16;

const MAGIC: [u8; 4] = *b"TTSt";
const VERSION: u8 = 1;

/// The largest payload a record can carry.
///
/// Nothing but the stack buffer in [`Store::save`] argues for a limit; settings are tens of
/// bytes, not hundreds. Raise it when something needs the room.
pub const MAX_PAYLOAD: usize = 256;

/// What can go wrong on the way to the flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The region is smaller than the two sectors this store needs.
    TooSmall,
    /// The payload is longer than [`MAX_PAYLOAD`].
    TooLong,
    /// The flash refused a read, an erase or a write.
    ///
    /// The kind comes from the driver underneath. It is carried rather than swallowed because
    /// the three that can happen here mean very different things: `NotAligned` and `OutOfBounds`
    /// are this module's own arithmetic being wrong, `Other` is the flash itself saying no.
    Flash(NorFlashErrorKind),
}

/// A pair of sectors holding the newest settings record.
pub struct Store<F> {
    flash: F,
    sector: usize,
    /// Which of the two sectors holds the record that was loaded last, if either does.
    newest: Option<usize>,
    seq: u32,
}

impl<F: NorFlash> Store<F> {
    /// Take a flash region and use its first two sectors.
    ///
    /// The region is expected to be the `nvs` partition, or some other region of at least two
    /// sectors that nothing else writes to.
    pub fn new(flash: F) -> Result<Self, Error> {
        let sector = F::ERASE_SIZE;
        if sector < HEADER + MAX_PAYLOAD || flash.capacity() < 2 * sector {
            return Err(Error::TooSmall);
        }
        Ok(Self {
            flash,
            sector,
            newest: None,
            seq: 0,
        })
    }

    /// Read the newest valid record into `buf` and return its length.
    ///
    /// `Ok(None)` means both sectors are empty or damaged -- a board that has never been written
    /// to looks exactly like one whose only record was corrupted, and the caller wants its
    /// defaults either way.
    pub fn load(&mut self, buf: &mut [u8]) -> Result<Option<usize>, Error> {
        let mut best: Option<(usize, u32, usize)> = None;
        let mut scratch = Scratch([0u8; HEADER + MAX_PAYLOAD]);
        let scratch = &mut scratch.0;

        for slot in 0..2 {
            let Some((seq, len)) = self.read_slot(slot, scratch)? else {
                continue;
            };
            // Sequence numbers only ever go up, so the newer record is the one whose distance
            // from the other is positive when read as a signed difference. That keeps working
            // across the wrap at 2^32, which no real board will reach.
            let newer = match best {
                None => true,
                Some((_, other, _)) => (seq.wrapping_sub(other) as i32) > 0,
            };
            if newer {
                best = Some((slot, seq, len));
            }
        }

        let Some((slot, seq, len)) = best else {
            self.newest = None;
            self.seq = 0;
            return Ok(None);
        };

        // Read it again rather than keep a second buffer alive across the loop.
        self.read_slot(slot, scratch)?;
        let taken = len.min(buf.len());
        buf[..taken].copy_from_slice(&scratch[HEADER..HEADER + taken]);
        self.newest = Some(slot);
        self.seq = seq;
        Ok(Some(len))
    }

    /// Write a new record into the sector the last load did not come from.
    pub fn save(&mut self, payload: &[u8]) -> Result<(), Error> {
        if payload.len() > MAX_PAYLOAD {
            return Err(Error::TooLong);
        }

        let slot = match self.newest {
            Some(0) => 1,
            _ => 0,
        };
        let seq = self.seq.wrapping_add(1);

        let mut record = Scratch([0u8; HEADER + MAX_PAYLOAD]);
        let record = &mut record.0;
        record[0..4].copy_from_slice(&MAGIC);
        record[4] = VERSION;
        record[6..8].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        record[8..12].copy_from_slice(&seq.to_le_bytes());
        let crc = crc32(crc32(!0, &record[0..12]), payload);
        record[12..16].copy_from_slice(&crc.to_le_bytes());
        record[HEADER..HEADER + payload.len()].copy_from_slice(payload);

        // A NOR flash writes in words, so round the record up to the write granularity. The
        // padding is zero and lies past the recorded length, where nothing reads it.
        let unit = F::WRITE_SIZE;
        let written = (HEADER + payload.len()).div_ceil(unit) * unit;

        let start = (slot * self.sector) as u32;
        self.flash
            .erase(start, start + self.sector as u32)
            .map_err(|e| Error::Flash(e.kind()))?;
        self.flash
            .write(start, &record[..written])
            .map_err(|e| Error::Flash(e.kind()))?;

        self.newest = Some(slot);
        self.seq = seq;
        Ok(())
    }

    /// The sequence number of the record last loaded or saved; zero when there is none.
    pub fn sequence(&self) -> u32 {
        self.seq
    }

    /// Which of the two sectors the last load or save used, if either.
    ///
    /// Nothing in normal operation needs this -- the point of the pair is that the caller does
    /// not have to care. It is here because a run that wants to prove the alternation, or damage
    /// one sector on purpose, has to know which one it is looking at.
    pub fn slot(&self) -> Option<usize> {
        self.newest
    }

    /// The size of one of the two sectors, in bytes.
    pub fn sector_size(&self) -> usize {
        self.sector
    }

    /// The region the store writes to, for a caller that needs the flash under it while the
    /// store holds it. A write into the store's two sectors through it breaks the store.
    pub fn flash_mut(&mut self) -> &mut F {
        &mut self.flash
    }

    /// Read one sector's header and payload, returning its sequence and length when it is valid.
    fn read_slot(&mut self, slot: usize, scratch: &mut [u8]) -> Result<Option<(u32, usize)>, Error> {
        let start = (slot * self.sector) as u32;
        self.flash
            .read(start, &mut scratch[..HEADER])
            .map_err(|e| Error::Flash(e.kind()))?;

        if scratch[0..4] != MAGIC || scratch[4] != VERSION {
            return Ok(None);
        }
        let len = u16::from_le_bytes([scratch[6], scratch[7]]) as usize;
        if len > MAX_PAYLOAD {
            return Ok(None);
        }
        let seq = u32::from_le_bytes([scratch[8], scratch[9], scratch[10], scratch[11]]);
        let want = u32::from_le_bytes([scratch[12], scratch[13], scratch[14], scratch[15]]);

        // A NOR flash reads in words, so the payload is fetched rounded up to the read
        // granularity even though only `len` of it counts. Asking for 65 bytes is not a small
        // inefficiency, it is refused outright -- and the refusal arrives as a plain storage
        // error, which reads exactly like a broken chip.
        let unit = F::READ_SIZE;
        let rounded = len.div_ceil(unit) * unit;
        self.flash
            .read(start + HEADER as u32, &mut scratch[HEADER..HEADER + rounded])
            .map_err(|e| Error::Flash(e.kind()))?;

        let crc = crc32(crc32(!0, &scratch[0..12]), &scratch[HEADER..HEADER + len]);
        if crc != want {
            return Ok(None);
        }
        Ok(Some((seq, len)))
    }
}

/// A record-sized buffer on a word boundary.
///
/// The flash driver reads straight into a word-aligned destination and copies through a 4 KiB
/// stack buffer when the destination is not aligned. A `[u8; N]` has an alignment of one, so
/// without this wrapper every read takes the expensive path -- on a task stack, the dangerous
/// one.
#[repr(align(4))]
struct Scratch([u8; HEADER + MAX_PAYLOAD]);

/// CRC-32, the reflected IEEE one, computed a bit at a time.
///
/// A record is tens of bytes and gets checked twice per boot; a 1 KiB table would cost more flash
/// than the loop costs time. Seed it with `!0` and it needs no final inversion as long as both
/// sides agree, which they do because both sides are here.
fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    crc
}
