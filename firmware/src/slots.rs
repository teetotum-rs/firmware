//! Plugin slots in the `plugins` partition.
//!
//! The partition is cut into slots of [`SLOT`] bytes, one plugin each: a [`HEADER`]-byte header,
//! then the module. The header holds a magic, the format, the [`PluginId`], the module's length
//! and the first 32 bytes of its SHA-512.
//!
//! **The header is written last**, so a write cut short leaves a slot that reads as empty or
//! fails its hash -- never one that passes as a plugin with half its bytes.
//!
//! **A written slot waits to be accepted.** Header byte 5 is `0xff` as written and `0x00` once
//! the user has accepted the plugin on the glass; any other value reads as accepted. Accepting
//! only clears bits, so it needs no erase, and a slot written again waits again.
//!
//! Reading copies the module into the caller's buffer and checks hash and id. The signature is
//! [`crate::plugin`]'s to check, before anything runs.

use ed25519_compact::sha512;
use embedded_storage::nor_flash::{NorFlash, NorFlashError, NorFlashErrorKind};

use crate::plugin::PluginId;

/// Bytes per slot.
pub const SLOT: usize = 64 * 1024;
/// Bytes of header in front of each module.
pub const HEADER: usize = 64;
/// The largest module a slot holds.
pub const MODULE_MAX: usize = SLOT - HEADER;

const MAGIC: [u8; 4] = *b"TTPS";
const FORMAT: u8 = 1;
const HASH: usize = 32;
/// Where in the header the slot says whether its plugin was accepted.
const STATE: usize = 5;
const PENDING: u8 = 0xff;
const ACCEPTED: u8 = 0x00;
/// Bytes read and written to change [`STATE`].
const WORD: usize = 4;
/// The largest write size a module's last bytes can be padded to.
const TAIL: usize = 16;
/// Bytes per read of a module.
const BOUNCE: usize = 1024;

/// What a slot's header says about the module behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub id: PluginId,
    pub len: usize,
    /// Whether the user accepted the plugin; one that waits is not loaded.
    pub accepted: bool,
    hash: [u8; HASH],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The partition has no slot with that number.
    NoSlot,
    /// The slot holds no plugin.
    Empty,
    /// The module is longer than [`MODULE_MAX`].
    TooLong,
    /// The buffer is shorter than the module.
    Buffer { need: usize },
    /// The flash reads in units that do not divide [`BOUNCE`].
    ReadSize,
    /// The module names no id, or not the one in its header.
    Id,
    /// The module's bytes do not match the hash in its header.
    Hash,
    /// The flash writes in units larger than [`TAIL`].
    WriteSize,
    Flash(NorFlashErrorKind),
}

/// The slots of one partition.
pub struct Slots<F> {
    flash: F,
}

impl<F: NorFlash> Slots<F> {
    pub fn new(flash: F) -> Self {
        Self { flash }
    }

    pub fn count(&self) -> usize {
        self.flash.capacity() / SLOT
    }

    pub fn capacity(&self) -> usize {
        self.flash.capacity()
    }

    pub fn into_inner(self) -> F {
        self.flash
    }

    /// Slot `n`'s header; `None` when the slot is empty or holds no header of this format.
    pub fn header(&mut self, n: usize) -> Result<Option<Header>, Error> {
        let start = self.start(n)?;
        let mut raw = Aligned([0u8; HEADER]);
        self.flash.read(start, &mut raw.0).map_err(flash_error)?;
        let raw = &raw.0;

        if raw[0..4] != MAGIC || raw[4] != FORMAT {
            return Ok(None);
        }
        let len = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]) as usize;
        if len > MODULE_MAX {
            return Ok(None);
        }
        let mut id = [0; PluginId::LEN];
        id.copy_from_slice(&raw[8..16]);
        let mut hash = [0; HASH];
        hash.copy_from_slice(&raw[20..20 + HASH]);
        Ok(Some(Header {
            id: PluginId::from_bytes(id),
            len,
            accepted: raw[STATE] != PENDING,
            hash,
        }))
    }

    /// Marks slot `n`'s plugin accepted. Only bits are cleared: nothing is erased, and module
    /// and hash stay as they are.
    pub fn accept(&mut self, n: usize) -> Result<(), Error> {
        if self.header(n)?.is_none() {
            return Err(Error::Empty);
        }
        if !WORD.is_multiple_of(F::WRITE_SIZE) {
            return Err(Error::WriteSize);
        }
        if !WORD.is_multiple_of(F::READ_SIZE) {
            return Err(Error::ReadSize);
        }
        let at = self.start(n)? + (STATE / WORD * WORD) as u32;
        let mut word = Aligned([0u8; WORD]);
        self.flash.read(at, &mut word.0).map_err(flash_error)?;
        word.0[STATE % WORD] = ACCEPTED;
        self.flash.write(at, &word.0).map_err(flash_error)
    }

    /// Copies slot `n`'s module into `buf` and checks it against the header.
    ///
    /// The bytes come through a word-aligned buffer on the stack, [`BOUNCE`] at a time, so `buf`
    /// may lie in external RAM: the flash shares its bus, and a read straight into it is untested.
    pub fn read(&mut self, n: usize, buf: &mut [u8]) -> Result<Option<Header>, Error> {
        let Some(header) = self.header(n)? else {
            return Ok(None);
        };
        if buf.len() < header.len {
            return Err(Error::Buffer { need: header.len });
        }
        let unit = F::READ_SIZE;
        if BOUNCE % unit != 0 {
            return Err(Error::ReadSize);
        }
        let mut bounce = Aligned([0u8; BOUNCE]);
        let mut at = self.start(n)? + HEADER as u32;
        for chunk in buf[..header.len].chunks_mut(BOUNCE) {
            let rounded = chunk.len().div_ceil(unit) * unit;
            self.flash
                .read(at, &mut bounce.0[..rounded])
                .map_err(flash_error)?;
            chunk.copy_from_slice(&bounce.0[..chunk.len()]);
            at += chunk.len() as u32;
        }

        let module = &buf[..header.len];
        if digest(module) != header.hash {
            return Err(Error::Hash);
        }
        if PluginId::of(module).ok() != Some(header.id) {
            return Err(Error::Id);
        }
        Ok(Some(header))
    }

    /// Erases slot `n` and writes `wasm` into it, the header last.
    pub fn write(&mut self, n: usize, wasm: &[u8]) -> Result<Header, Error> {
        if wasm.len() > MODULE_MAX {
            return Err(Error::TooLong);
        }
        let unit = F::WRITE_SIZE;
        if unit > TAIL || HEADER % unit != 0 {
            return Err(Error::WriteSize);
        }
        let id = PluginId::of(wasm).map_err(|_| Error::Id)?;
        let header = Header {
            id,
            len: wasm.len(),
            accepted: false,
            hash: digest(wasm),
        };

        let start = self.start(n)?;
        self.erase(n)?;

        let body = start + HEADER as u32;
        let whole = wasm.len() / unit * unit;
        if whole > 0 {
            self.flash
                .write(body, &wasm[..whole])
                .map_err(flash_error)?;
        }
        if whole < wasm.len() {
            let mut tail = [0xff; TAIL];
            tail[..wasm.len() - whole].copy_from_slice(&wasm[whole..]);
            self.flash
                .write(body + whole as u32, &tail[..unit])
                .map_err(flash_error)?;
        }

        let mut raw = Aligned([0u8; HEADER]);
        raw.0[0..4].copy_from_slice(&MAGIC);
        raw.0[4] = FORMAT;
        raw.0[STATE] = PENDING;
        raw.0[8..16].copy_from_slice(&id.bytes());
        raw.0[16..20].copy_from_slice(&(wasm.len() as u32).to_le_bytes());
        raw.0[20..20 + HASH].copy_from_slice(&header.hash);
        self.flash.write(start, &raw.0).map_err(flash_error)?;
        Ok(header)
    }

    /// Erases slot `n`; it reads as empty afterwards.
    pub fn erase(&mut self, n: usize) -> Result<(), Error> {
        let start = self.start(n)?;
        self.flash
            .erase(start, start + SLOT as u32)
            .map_err(flash_error)
    }

    fn start(&self, n: usize) -> Result<u32, Error> {
        if n < self.count() {
            Ok((n * SLOT) as u32)
        } else {
            Err(Error::NoSlot)
        }
    }
}

#[repr(align(4))]
struct Aligned<const N: usize>([u8; N]);

fn digest(module: &[u8]) -> [u8; HASH] {
    let mut hash = sha512::Hash::new();
    hash.update(module);
    let full = hash.finalize();
    let mut out = [0; HASH];
    out.copy_from_slice(&full[..HASH]);
    out
}

fn flash_error<E: NorFlashError>(e: E) -> Error {
    Error::Flash(e.kind())
}
