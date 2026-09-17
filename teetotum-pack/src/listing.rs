//! One entry of the plugin list a sender reads from the knob over BLE.
//!
//! A sender writes an index, then reads the entry at that index:
//!
//! ```text
//! 0       index asked for
//! 1       plugins in the list
//! 2       flags: bit 0 bundled, bit 1 installed
//! 3       slot, NO_SLOT for a bundled plugin
//! 4..12   PluginId
//! 12..16  module length, little-endian
//! 16..22  version: major, minor, patch, u16 little-endian each
//! 22      name length, then NAME_MAX bytes
//! 43      summary length, then SUMMARY_MAX bytes
//! ```
//!
//! An index past the list carries only index and count; every other byte is zero.

use crate::PluginId;
use crate::manifest::{NAME_MAX, SUMMARY_MAX, Version};

/// Bytes of one entry.
pub const ENTRY: usize = 76;
/// Flag: the plugin comes with the firmware.
pub const BUNDLED: u8 = 0x01;
/// Flag: the plugin stands on home.
pub const INSTALLED: u8 = 0x02;
/// The slot byte of a bundled plugin.
pub const NO_SLOT: u8 = 0xff;

const NAME_AT: usize = 22;
const SUMMARY_AT: usize = NAME_AT + 1 + NAME_MAX;
const _: () = assert!(SUMMARY_AT + 1 + SUMMARY_MAX == ENTRY);

/// A plugin as the list describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The slot it was installed from; `None` for a bundled plugin.
    pub slot: Option<u8>,
    pub installed: bool,
    pub id: PluginId,
    /// Length of its module in bytes.
    pub len: u32,
    pub version: Version,
    pub name: &'a str,
    pub summary: &'a str,
}

/// An entry as read: what was asked for, how long the list is, and the plugin if there is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed<'a> {
    pub index: u8,
    pub count: u8,
    pub entry: Option<Entry<'a>>,
}

impl<'a> Entry<'a> {
    /// The bytes of this entry at `index` of a list of `count`. A name or summary longer than
    /// a manifest allows is cut at a character boundary.
    pub fn encode(&self, index: u8, count: u8) -> [u8; ENTRY] {
        let mut raw = past_end(index, count);
        if self.slot.is_none() {
            raw[2] |= BUNDLED;
        }
        if self.installed {
            raw[2] |= INSTALLED;
        }
        raw[3] = self.slot.unwrap_or(NO_SLOT);
        raw[4..12].copy_from_slice(&self.id.bytes());
        raw[12..16].copy_from_slice(&self.len.to_le_bytes());
        let Version {
            major,
            minor,
            patch,
        } = self.version;
        raw[16..18].copy_from_slice(&major.to_le_bytes());
        raw[18..20].copy_from_slice(&minor.to_le_bytes());
        raw[20..22].copy_from_slice(&patch.to_le_bytes());
        put(&mut raw[NAME_AT..SUMMARY_AT], self.name);
        put(&mut raw[SUMMARY_AT..], self.summary);
        raw
    }

    /// Reads an entry; `None` if its lengths or text are not what [`Self::encode`] writes.
    pub fn decode(raw: &'a [u8; ENTRY]) -> Option<Listed<'a>> {
        let (index, count) = (raw[0], raw[1]);
        if index >= count {
            return raw[2..].iter().all(|b| *b == 0).then_some(Listed {
                index,
                count,
                entry: None,
            });
        }
        let flags = raw[2];
        let bundled = flags & BUNDLED != 0;
        if flags & !(BUNDLED | INSTALLED) != 0 || bundled != (raw[3] == NO_SLOT) {
            return None;
        }
        let u16_at = |at: usize| u16::from_le_bytes([raw[at], raw[at + 1]]);
        let entry = Entry {
            slot: (!bundled).then_some(raw[3]),
            installed: flags & INSTALLED != 0,
            id: PluginId::from_bytes(raw[4..12].try_into().ok()?),
            len: u32::from_le_bytes(raw[12..16].try_into().ok()?),
            version: Version {
                major: u16_at(16),
                minor: u16_at(18),
                patch: u16_at(20),
            },
            name: take(&raw[NAME_AT..SUMMARY_AT])?,
            summary: take(&raw[SUMMARY_AT..])?,
        };
        Some(Listed {
            index,
            count,
            entry: Some(entry),
        })
    }
}

/// The bytes for an index past the end of a list of `count`.
pub fn past_end(index: u8, count: u8) -> [u8; ENTRY] {
    let mut raw = [0; ENTRY];
    raw[0] = index;
    raw[1] = count;
    raw
}

/// Writes a length byte and as much of `text` as the field holds.
fn put(field: &mut [u8], text: &str) {
    let mut len = text.len().min(field.len() - 1);
    while !text.is_char_boundary(len) {
        len -= 1;
    }
    field[0] = len as u8;
    field[1..=len].copy_from_slice(&text.as_bytes()[..len]);
}

/// Reads a length byte and the text after it; the rest of the field must be zero.
fn take(field: &[u8]) -> Option<&str> {
    let (len, body) = field.split_first()?;
    let len = usize::from(*len);
    if len > body.len() || body[len..].iter().any(|b| *b != 0) {
        return None;
    }
    core::str::from_utf8(&body[..len]).ok()
}
