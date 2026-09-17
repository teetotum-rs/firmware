//! Uploading a plugin over BLE: what a sender writes, and what the knob answers.
//!
//! A sender writes [`BEGIN`] and the slot header of its module to the control characteristic,
//! waits for [`Status::Ready`], writes the module in pieces to the data characteristic, each
//! after its offset, and writes [`COMMIT`]. The knob writes each piece into a free slot as it
//! comes ([`crate::slots`]), checks length and hash at the commit, writes the header and
//! restarts; the install dialog at boot checks the signature and asks the user.
//!
//! A sender lists the plugins by writing an index to the select characteristic and reading the
//! entry characteristic ([`listing`]), and deletes one by writing [`DELETE`] and its slot; the
//! knob answers [`Status::Deleted`] and restarts.
//!
//! Writes to control and data are taken only while the receive dialog is open on the screen;
//! the list can be read at any time.

use crate::slots::{HEADER, Header};
pub use teetotum_pack::listing::{self, ENTRY};

/// Control: a header follows; a free slot is erased for its module.
pub const BEGIN: u8 = 1;
/// Control: the module is complete.
pub const COMMIT: u8 = 2;
/// Control: the upload is dropped, and the slot left empty.
pub const ABORT: u8 = 3;
/// Control: the slot in the next byte is erased, with the plugin it holds.
pub const DELETE: u8 = 4;

/// The longest control write: a command and a header.
pub const CONTROL_MAX: usize = 1 + HEADER;
/// Bytes in front of a piece: where it goes in the module, little-endian.
pub const OFFSET: usize = 4;
/// The longest data write. The packet pool's 251 bytes leave an ATT MTU of 247, and a write
/// carries three bytes less.
pub const DATA_MAX: usize = 244;
/// The most module bytes in one piece.
pub const PIECE_MAX: usize = DATA_MAX - OFFSET;
/// Bytes of the status characteristic: code, slot, bytes received.
pub const STATUS_LEN: usize = 6;

/// One write of a sender, read.
pub enum Command {
    Begin(Header),
    Piece {
        offset: usize,
        len: usize,
        bytes: [u8; PIECE_MAX],
    },
    Commit,
    Abort,
    Delete(u8),
}

impl Command {
    /// A write to the control characteristic; `None` if it is none of the commands.
    pub fn control(raw: &[u8]) -> Option<Self> {
        match raw {
            [BEGIN, rest @ ..] => {
                let raw: &[u8; HEADER] = rest.try_into().ok()?;
                Header::decode(raw).map(Self::Begin)
            }
            [COMMIT] => Some(Self::Commit),
            [ABORT] => Some(Self::Abort),
            [DELETE, slot] => Some(Self::Delete(*slot)),
            _ => None,
        }
    }

    /// A write to the data characteristic; `None` if it holds no bytes after the offset.
    pub fn data(raw: &[u8]) -> Option<Self> {
        if raw.len() <= OFFSET || raw.len() > DATA_MAX {
            return None;
        }
        let (offset, piece) = raw.split_at(OFFSET);
        let mut bytes = [0; PIECE_MAX];
        bytes[..piece.len()].copy_from_slice(piece);
        Some(Self::Piece {
            offset: u32::from_le_bytes([offset[0], offset[1], offset[2], offset[3]]) as usize,
            len: piece.len(),
            bytes,
        })
    }
}

/// How an upload stands, as the status characteristic says it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// Nothing is being uploaded.
    #[default]
    Idle,
    /// A slot is erased and takes pieces.
    Ready,
    /// The module and its header are written; the knob restarts.
    Written,
    /// A plugin's slot is erased; the knob restarts.
    Deleted,
    /// Every slot holds a plugin.
    NoSlot,
    /// A write that is no command, or a piece or commit with no upload begun.
    Refused,
    /// A piece that does not continue where the last one ended.
    OutOfOrder,
    /// The module is longer or shorter than its header says, or does not match its hash.
    Mismatch,
    /// The flash failed.
    Flash,
    /// A delete names a slot that holds no plugin in the list.
    NoPlugin,
}

impl Status {
    pub fn code(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Ready => 1,
            Self::Written => 2,
            Self::Deleted => 3,
            Self::NoSlot => 0x81,
            Self::Refused => 0x82,
            Self::OutOfOrder => 0x83,
            Self::Mismatch => 0x84,
            Self::Flash => 0x85,
            Self::NoPlugin => 0x86,
        }
    }

    /// What went wrong, short enough for the screen; empty for a status that is no failure.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Idle | Self::Ready | Self::Written | Self::Deleted => "",
            Self::NoSlot => "every slot is taken",
            Self::Refused => "not an upload",
            Self::OutOfOrder => "a piece went missing",
            Self::Mismatch => "not what was announced",
            Self::Flash => "flash failed",
            Self::NoPlugin => "no plugin in that slot",
        }
    }

    /// The status characteristic's bytes: the code, the slot, the module bytes received so far.
    pub fn encode(self, slot: usize, received: usize) -> [u8; STATUS_LEN] {
        let received = (received as u32).to_le_bytes();
        [
            self.code(),
            slot as u8,
            received[0],
            received[1],
            received[2],
            received[3],
        ]
    }
}
