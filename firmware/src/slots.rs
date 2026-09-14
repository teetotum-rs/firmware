//! Plugin slots in the `plugins` partition.
//!
//! What a slot holds -- a header, then the module -- is [`teetotum_pack::slot`]'s. This reads
//! and writes it on flash.
//!
//! **The header is written last**, so a write cut short leaves a slot that reads as empty or
//! fails its hash -- never one that passes as a plugin with half its bytes.
//!
//! **A written slot waits to be accepted** until the user accepts the plugin on the glass. Only
//! bits are cleared then, so it needs no erase, and a slot written again waits again.
//!
//! Reading copies the module into the caller's buffer and checks hash and id. The signature is
//! [`crate::plugin`]'s to check, before anything runs.

use embedded_storage::nor_flash::{NorFlash, NorFlashError, NorFlashErrorKind};
pub use teetotum_pack::slot::{self, HEADER, Header, MODULE_MAX, SLOT};

/// Bytes read and written to change [`slot::STATE`].
const WORD: usize = 4;
/// The largest write size a module's last bytes can be padded to.
const TAIL: usize = 16;
/// Bytes per read of a module.
const BOUNCE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The partition has no slot with that number.
    NoSlot,
    /// The slot holds no plugin.
    Empty,
    /// The buffer is shorter than the module.
    Buffer {
        need: usize,
    },
    /// The flash reads in units that do not divide [`BOUNCE`].
    ReadSize,
    /// The flash writes in units larger than [`TAIL`].
    WriteSize,
    /// The module does not go with its header, or would not fit a slot.
    Module(slot::Error),
    Flash(NorFlashErrorKind),
}

/// A module on its way into a slot, from [`Slots::begin`] to [`Slots::finish`]. Dropping it
/// leaves the slot without a header, which reads as empty.
pub struct Upload {
    slot: usize,
    header: Header,
    received: usize,
    digest: slot::Digest,
    carry: [u8; TAIL],
    carried: usize,
}

impl Upload {
    pub fn slot(&self) -> usize {
        self.slot
    }

    /// Bytes of the module written so far.
    pub fn received(&self) -> usize {
        self.received
    }

    /// Bytes of the module in all.
    pub fn total(&self) -> usize {
        self.header.len
    }
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
        Ok(Header::decode(&raw.0))
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
        let at = self.start(n)? + (slot::STATE / WORD * WORD) as u32;
        let mut word = Aligned([0u8; WORD]);
        self.flash.read(at, &mut word.0).map_err(flash_error)?;
        word.0[slot::STATE % WORD] = slot::ACCEPTED;
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
        if !BOUNCE.is_multiple_of(unit) {
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

        header.matches(&buf[..header.len]).map_err(Error::Module)?;
        Ok(Some(header))
    }

    /// Erases slot `n` and writes `wasm` into it, the header last.
    pub fn write(&mut self, n: usize, wasm: &[u8]) -> Result<Header, Error> {
        let header = Header::of(wasm).map_err(Error::Module)?;
        let unit = F::WRITE_SIZE;
        if unit > TAIL || !HEADER.is_multiple_of(unit) {
            return Err(Error::WriteSize);
        }

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

        let raw = Aligned(header.encode());
        self.flash.write(start, &raw.0).map_err(flash_error)?;
        Ok(header)
    }

    /// The first slot that holds no plugin.
    pub fn free(&mut self) -> Result<Option<usize>, Error> {
        for n in 0..self.count() {
            if self.header(n)?.is_none() {
                return Ok(Some(n));
            }
        }
        Ok(None)
    }

    /// Erases slot `n` for the module `header` describes, which [`Slots::feed`] then writes as
    /// it arrives and [`Slots::finish`] completes.
    pub fn begin(&mut self, n: usize, header: Header) -> Result<Upload, Error> {
        let unit = F::WRITE_SIZE;
        if unit > TAIL || !HEADER.is_multiple_of(unit) {
            return Err(Error::WriteSize);
        }
        if header.len > MODULE_MAX {
            return Err(Error::Module(slot::Error::TooLong));
        }
        self.erase(n)?;
        let mut header = header;
        header.accepted = false;
        Ok(Upload {
            slot: n,
            header,
            received: 0,
            digest: slot::Digest::new(),
            carry: [0xff; TAIL],
            carried: 0,
        })
    }

    /// Writes the next bytes of `upload`'s module. Whole write units go to flash straight away;
    /// what is left of one waits for the next piece.
    pub fn feed(&mut self, upload: &mut Upload, bytes: &[u8]) -> Result<(), Error> {
        if upload.received + bytes.len() > upload.header.len {
            return Err(Error::Module(slot::Error::TooLong));
        }
        let unit = F::WRITE_SIZE;
        let body = self.start(upload.slot)? + HEADER as u32;
        upload.digest.update(bytes);
        let mut rest = bytes;
        if upload.carried > 0 {
            let take = (unit - upload.carried).min(rest.len());
            upload.carry[upload.carried..upload.carried + take].copy_from_slice(&rest[..take]);
            upload.carried += take;
            rest = &rest[take..];
            if upload.carried == unit {
                let at = body + (upload.received + take - unit) as u32;
                self.flash
                    .write(at, &upload.carry[..unit])
                    .map_err(flash_error)?;
                upload.carry = [0xff; TAIL];
                upload.carried = 0;
            }
        }
        let at = body + (upload.received + bytes.len() - rest.len()) as u32;
        let whole = rest.len() / unit * unit;
        if whole > 0 {
            self.flash.write(at, &rest[..whole]).map_err(flash_error)?;
        }
        // Bytes left over here mean the carry was flushed above; none left may mean it still
        // fills, and then it stays as it is.
        let left = rest.len() - whole;
        if left > 0 {
            upload.carry[..left].copy_from_slice(&rest[whole..]);
            upload.carried = left;
        }
        upload.received += bytes.len();
        Ok(())
    }

    /// Writes the last bytes of `upload`'s module and, if all of them came and match the hash,
    /// the header. The id is checked when the slot is read.
    pub fn finish(&mut self, upload: Upload) -> Result<Header, Error> {
        let Upload {
            slot: n,
            header,
            received,
            digest,
            carry,
            carried,
        } = upload;
        header
            .matches_digest(received, digest)
            .map_err(Error::Module)?;
        let start = self.start(n)?;
        if carried > 0 {
            let at = start + (HEADER + received - carried) as u32;
            self.flash
                .write(at, &carry[..F::WRITE_SIZE])
                .map_err(flash_error)?;
        }
        let raw = Aligned(header.encode());
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

fn flash_error<E: NorFlashError>(e: E) -> Error {
    Error::Flash(e.kind())
}
