//! The header in front of a face in a slot of the `plugins` partition.
//!
//! The partition is cut into slots of [`SLOT`] bytes, one face each: a [`HEADER`]-byte header,
//! then the module.
//!
//! ```text
//! 0..4    magic "TTPS"
//! 4       format
//! 5       state: PENDING as written, anything else once accepted
//! 8..16   PluginId
//! 16..20  module length, little-endian
//! 20..52  first HASH bytes of the module's SHA-512
//! ```
//!
//! Only the bytes are here; reading and writing flash is the firmware's.

use core::fmt;

use ed25519_compact::sha512;

use crate::PluginId;

/// Bytes per slot.
pub const SLOT: usize = 64 * 1024;
/// Bytes of header in front of each module.
pub const HEADER: usize = 64;
/// The largest module a slot holds.
pub const MODULE_MAX: usize = SLOT - HEADER;
/// Bytes of the module's SHA-512 the header keeps.
pub const HASH: usize = 32;
/// Where in the header the slot says whether its face was accepted.
pub const STATE: usize = 5;
/// [`STATE`] as written: the face waits for the user.
pub const PENDING: u8 = 0xff;
/// [`STATE`] once accepted. Accepting only clears bits, so it needs no erase.
pub const ACCEPTED: u8 = 0x00;

const MAGIC: [u8; 4] = *b"TTPS";
const FORMAT: u8 = 1;

/// What a slot's header says about the module behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub id: PluginId,
    pub len: usize,
    /// Whether the user accepted the face; one that waits is not loaded.
    pub accepted: bool,
    hash: [u8; HASH],
}

/// Why a module does not go with a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The module is longer than [`MODULE_MAX`].
    TooLong,
    /// The module names no id, or not the one in its header.
    Id,
    /// The module's bytes do not match the hash in its header.
    Hash,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => write!(f, "module longer than {MODULE_MAX} bytes"),
            Self::Id => f.write_str("module does not name the id in its header"),
            Self::Hash => f.write_str("module does not match the hash in its header"),
        }
    }
}

impl Header {
    /// The header for `wasm`, waiting to be accepted. The signature is not checked here.
    pub fn of(wasm: &[u8]) -> Result<Self, Error> {
        if wasm.len() > MODULE_MAX {
            return Err(Error::TooLong);
        }
        Ok(Self {
            id: PluginId::of(wasm).map_err(|_| Error::Id)?,
            len: wasm.len(),
            accepted: false,
            hash: digest(wasm),
        })
    }

    pub fn encode(&self) -> [u8; HEADER] {
        let mut raw = [0; HEADER];
        raw[0..4].copy_from_slice(&MAGIC);
        raw[4] = FORMAT;
        raw[STATE] = if self.accepted { ACCEPTED } else { PENDING };
        raw[8..16].copy_from_slice(&self.id.bytes());
        raw[16..20].copy_from_slice(&(self.len as u32).to_le_bytes());
        raw[20..20 + HASH].copy_from_slice(&self.hash);
        raw
    }

    /// `None` when `raw` holds no header of this format, as an erased slot does.
    pub fn decode(raw: &[u8; HEADER]) -> Option<Self> {
        if raw[0..4] != MAGIC || raw[4] != FORMAT {
            return None;
        }
        let len = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]) as usize;
        if len > MODULE_MAX {
            return None;
        }
        let mut id = [0; PluginId::LEN];
        id.copy_from_slice(&raw[8..16]);
        let mut hash = [0; HASH];
        hash.copy_from_slice(&raw[20..20 + HASH]);
        Some(Self {
            id: PluginId::from_bytes(id),
            len,
            accepted: raw[STATE] != PENDING,
            hash,
        })
    }

    /// Whether `module` is the one this header describes: its hash first, then the id it names.
    pub fn matches(&self, module: &[u8]) -> Result<(), Error> {
        if module.len() != self.len || digest(module) != self.hash {
            return Err(Error::Hash);
        }
        if PluginId::of(module).ok() != Some(self.id) {
            return Err(Error::Id);
        }
        Ok(())
    }
}

/// The first [`HASH`] bytes of the module's SHA-512.
pub fn digest(module: &[u8]) -> [u8; HASH] {
    let mut hash = sha512::Hash::new();
    hash.update(module);
    let full = hash.finalize();
    let mut out = [0; HASH];
    out.copy_from_slice(&full[..HASH]);
    out
}
