//! What a face is, readable without running any of it.
//!
//! The manifest travels inside the module, as a custom section named [`SECTION`]: one file, so
//! a manifest can neither lose its module nor be paired with another. The
//! [`face!`](crate::face) macro writes it, and the firmware reads it before the module is even
//! validated -- which is what lets the settings show a face, and remove it, without a line of
//! its code running.
//!
//! Laid out by hand, like the firmware's settings record, because a format written by hand is
//! the only kind a dependency bump cannot change:
//!
//! ```text
//! 0          format version, 3
//! 1..5       rights, u32 little-endian
//! 5          length of the name in bytes, 1 to NAME_MAX
//! 6..26      the name, UTF-8, padded with zeros
//! 26..122    the icon: 24 rows, u32 little-endian, bit 23 leftmost
//! 122        length of the summary in bytes, 0 to SUMMARY_MAX
//! 123..155   the summary, UTF-8, padded with zeros
//! 155..157   the host ABI the face was built against, u16 little-endian
//! 157..163   the face's own version: major, minor, patch, u16 little-endian each
//! ```
//!
//! **A face is signed by its author**, in a second custom section, [`SIGNATURE`]: the author's
//! Ed25519 public key, then the signature over every byte of the module before that section.
//! It has to be the module's last section, so nothing can be added to a signed module unsigned.
//! The key is not in the manifest because it is not the face's to choose: the same source
//! signed by someone else is someone else's face. **Key and name together are the face's
//! identity** -- an update is the same face only if both match.
//!
//! Formats 1 and 2 are not read any more. They had no version and no signature, and they went
//! before the first release of this crate, when no face written against them could exist
//! outside the firmware's own tree.

use core::fmt;

use crate::{Icon, abi};

/// The custom section a manifest is in.
pub const SECTION: &str = "teetotum.manifest";
/// The format this version writes and reads.
pub const VERSION: u8 = 3;
/// The custom section the author's key and signature are in; see the module notes.
pub const SIGNATURE: &str = "teetotum.signature";
/// How long an Ed25519 public key is.
pub const KEY_LEN: usize = 32;
/// How long an Ed25519 signature is.
pub const SIGNATURE_LEN: usize = 64;
/// The longest name, in bytes. It stands large in the middle of the settings ring, and twenty
/// is about what the middle of the ring holds.
pub const NAME_MAX: usize = 20;
/// The longest summary, in bytes. It stands under the name at home, in a smaller font, and the
/// firmware cuts it with "..." where the ring runs out -- so this is a bound, not a promise.
pub const SUMMARY_MAX: usize = 32;
const ICON_AT: usize = 6 + NAME_MAX;
const SUMMARY_AT: usize = ICON_AT + 4 * Icon::SIZE;
const ABI_AT: usize = SUMMARY_AT + 1 + SUMMARY_MAX;
const VERSION_AT: usize = ABI_AT + 2;
/// How long a manifest is.
pub const LEN: usize = VERSION_AT + 6;

/// A face's own version, as its crate gives it: `major.minor.patch`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl Version {
    /// A version from `CARGO_PKG_VERSION`. A pre-release or build suffix (`-beta`, `+local`)
    /// is dropped: the manifest has no room for it, and the firmware compares numbers only.
    ///
    /// # Panics
    ///
    /// If it is not three numbers of at most 65535 -- at compile time, where the macro calls it.
    pub const fn parse(text: &str) -> Self {
        let bytes = text.as_bytes();
        let mut parts = [0u32; 3];
        let mut part = 0;
        let mut digits = 0;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'-' || b == b'+' {
                break;
            }
            if b == b'.' {
                assert!(digits > 0 && part < 2, "a face's version is major.minor.patch");
                part += 1;
                digits = 0;
            } else {
                assert!(b.is_ascii_digit(), "a face's version is major.minor.patch");
                parts[part] = parts[part] * 10 + (b - b'0') as u32;
                assert!(parts[part] <= u16::MAX as u32, "a version number is at most 65535");
                digits += 1;
            }
            i += 1;
        }
        assert!(part == 2 && digits > 0, "a face's version is major.minor.patch");
        Self {
            major: parts[0] as u16,
            minor: parts[1] as u16,
            patch: parts[2] as u16,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// What a face may do beyond drawing and hearing the glass.
///
/// The loader holds a face to it: a module that imports [`send`](crate::send) without
/// [`Rights::HID`] is refused, so what the settings show is all the face can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Self = Self(0);
    /// Send consumer-control usages to the phone, through the other chip's BLE HID link.
    pub const HID: Self = Self(1 << 0);
    /// The knob. While the face is shown, a detent comes to it as an event instead of turning
    /// the volume of whatever plays through the knob.
    pub const KNOB: Self = Self(1 << 1);
    /// Random bytes from the chip's generator, which says whether they came from physical noise.
    pub const RANDOM: Self = Self(1 << 2);
    /// What the radio hears nearby: the names, strengths and channels of Wi-Fi networks and
    /// Bluetooth devices, through [`nearby`](crate::nearby). **Never their addresses**, which no
    /// right gives; see [`Signal`](crate::Signal).
    pub const RADIO: Self = Self(1 << 3);
    /// A steady pulse of the motor, through [`pulse`](crate::pulse).
    pub const HAPTIC: Self = Self(1 << 4);

    const KNOWN: u32 =
        Self::HID.0 | Self::KNOB.0 | Self::RANDOM.0 | Self::RADIO.0 | Self::HAPTIC.0;

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// `None` for a right this version does not know. **Such a face is refused, not trimmed**:
    /// it was written for a newer firmware, and installing it with less than it asked for would
    /// make a face that half works without saying why.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::KNOWN == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
}

impl core::ops::BitOr for Rights {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// As the settings show it: `hid knob`, or `none`.
impl fmt::Display for Rights {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (right, name) in [
            (Self::HID, "hid"),
            (Self::KNOB, "knob"),
            (Self::RANDOM, "random"),
            (Self::RADIO, "radio"),
            (Self::HAPTIC, "haptic"),
        ] {
            if self.contains(right) {
                f.write_str(if first { "" } else { " " })?;
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("none")?;
        }
        Ok(())
    }
}

/// The manifest as [`face!`](crate::face) writes it.
///
/// # Panics
///
/// If the name is empty or longer than [`NAME_MAX`], or the summary longer than [`SUMMARY_MAX`]
/// -- at compile time, where the macro calls it.
pub const fn encode(
    name: &str,
    summary: &str,
    icon: &Icon,
    rights: Rights,
    version: Version,
) -> [u8; LEN] {
    let name = name.as_bytes();
    let summary = summary.as_bytes();
    assert!(
        !name.is_empty() && name.len() <= NAME_MAX,
        "a face's name is 1 to 20 bytes"
    );
    assert!(summary.len() <= SUMMARY_MAX, "a face's summary is at most 32 bytes");
    let mut out = [0u8; LEN];
    out[0] = VERSION;
    let bits = rights.bits().to_le_bytes();
    let mut i = 0;
    while i < 4 {
        out[1 + i] = bits[i];
        i += 1;
    }
    out[5] = name.len() as u8;
    let mut i = 0;
    while i < name.len() {
        out[6 + i] = name[i];
        i += 1;
    }
    let mut y = 0;
    while y < Icon::SIZE {
        let row = icon.rows()[y].to_le_bytes();
        let mut k = 0;
        while k < 4 {
            out[ICON_AT + 4 * y + k] = row[k];
            k += 1;
        }
        y += 1;
    }
    out[SUMMARY_AT] = summary.len() as u8;
    let mut i = 0;
    while i < summary.len() {
        out[SUMMARY_AT + 1 + i] = summary[i];
        i += 1;
    }
    let numbers = [abi::VERSION, version.major, version.minor, version.patch];
    let mut i = 0;
    while i < numbers.len() {
        let bytes = numbers[i].to_le_bytes();
        out[ABI_AT + 2 * i] = bytes[0];
        out[ABI_AT + 2 * i + 1] = bytes[1];
        i += 1;
    }
    out
}

/// A manifest as read back, borrowing from the module it came out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Manifest<'a> {
    name: &'a str,
    summary: &'a str,
    rights: Rights,
    icon: &'a [u8],
    abi: u16,
    version: Version,
}

impl<'a> Manifest<'a> {
    /// The manifest of a module, found without validating, compiling or running any of it.
    pub fn read(wasm: &'a [u8]) -> Result<Self, Error> {
        Self::decode(section(wasm, SECTION)?.ok_or(Error::Missing)?)
    }

    /// A manifest from the bytes of its section. Bytes after [`LEN`] are not looked at.
    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error> {
        match bytes.first() {
            None => return Err(Error::Truncated),
            Some(&VERSION) => {}
            Some(&other) => return Err(Error::Version(other)),
        }
        if bytes.len() < LEN {
            return Err(Error::Truncated);
        }
        let bits = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        let rights = Rights::from_bits(bits).ok_or(Error::Rights(bits))?;
        let len = bytes[5] as usize;
        if len == 0 || len > NAME_MAX {
            return Err(Error::Name);
        }
        let name = core::str::from_utf8(&bytes[6..6 + len]).map_err(|_| Error::Name)?;
        let len = bytes[SUMMARY_AT] as usize;
        if len > SUMMARY_MAX {
            return Err(Error::Summary);
        }
        let at = SUMMARY_AT + 1;
        let summary = core::str::from_utf8(&bytes[at..at + len]).map_err(|_| Error::Summary)?;
        let number = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let abi = number(ABI_AT);
        if abi > abi::VERSION {
            return Err(Error::Abi(abi));
        }
        Ok(Self {
            name,
            summary,
            rights,
            icon: &bytes[ICON_AT..SUMMARY_AT],
            abi,
            version: Version {
                major: number(VERSION_AT),
                minor: number(VERSION_AT + 2),
                patch: number(VERSION_AT + 4),
            },
        })
    }

    pub fn name(&self) -> &'a str {
        self.name
    }

    /// One line on what the face is, which home shows under its name. May be empty.
    pub fn summary(&self) -> &'a str {
        self.summary
    }

    pub fn rights(&self) -> Rights {
        self.rights
    }

    /// The host ABI the face was built against; never newer than this crate's [`abi::VERSION`].
    pub fn abi(&self) -> u16 {
        self.abi
    }

    /// The face's own version.
    pub fn version(&self) -> Version {
        self.version
    }

    /// The icon as it is stored: 24 rows of four bytes, little-endian, bit 23 leftmost. Bytes
    /// rather than an [`Icon`] so that it can be drawn straight out of the module.
    pub fn icon_bytes(&self) -> &'a [u8] {
        self.icon
    }

    pub fn icon(&self) -> Icon {
        let mut rows = [0u32; Icon::SIZE];
        for (row, bytes) in rows.iter_mut().zip(self.icon.chunks_exact(4)) {
            *row = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        Icon::from_rows(rows)
    }
}

/// Why a module has no manifest this version can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Not a WebAssembly module, or one whose sections do not add up.
    NotWasm,
    /// A module without a manifest.
    Missing,
    /// Two manifests. Which one counts would be a guess, and a guess is how a module shows the
    /// settings one set of rights and the loader another.
    Twice,
    /// Shorter than a manifest.
    Truncated,
    /// A format this version does not read.
    Version(u8),
    /// A name that is empty, too long or not UTF-8.
    Name,
    /// A summary that is too long or not UTF-8.
    Summary,
    /// Rights this version does not know.
    Rights(u32),
    /// Built against a host ABI newer than this version's.
    Abi(u16),
    /// No signature section.
    Unsigned,
    /// A signature section that is repeated, not the module's last, or not a key and a
    /// signature long.
    Signature,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotWasm => f.write_str("not a WebAssembly module"),
            Self::Missing => f.write_str("no manifest"),
            Self::Twice => f.write_str("two manifests"),
            Self::Truncated => f.write_str("manifest too short"),
            Self::Version(v) => write!(f, "manifest format {v}, this firmware reads {VERSION}"),
            Self::Name => f.write_str("manifest name empty, too long or not UTF-8"),
            Self::Summary => f.write_str("manifest summary too long or not UTF-8"),
            Self::Rights(bits) => write!(f, "rights {bits:#x} include some this firmware lacks"),
            Self::Abi(v) => write!(f, "built for host ABI {v}, this firmware offers {}", abi::VERSION),
            Self::Unsigned => f.write_str("not signed"),
            Self::Signature => f.write_str("signature section malformed or not last"),
        }
    }
}

/// A module split at its signature: what was signed, and by which key.
///
/// Reading it checks the section's place and length, **not the signature** -- that takes the
/// Ed25519 code, which is the firmware's to carry and not every face's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signed<'a> {
    /// Every byte of the module before the signature section.
    pub message: &'a [u8],
    pub key: &'a [u8; KEY_LEN],
    pub signature: &'a [u8; SIGNATURE_LEN],
}

impl<'a> Signed<'a> {
    pub fn read(wasm: &'a [u8]) -> Result<Self, Error> {
        let mut found = None;
        walk(wasm, |start, end, name, contents| {
            if name == SIGNATURE.as_bytes() && found.replace((start, end, contents)).is_some() {
                return Err(Error::Signature);
            }
            Ok(())
        })?;
        let (start, end, contents) = found.ok_or(Error::Unsigned)?;
        if end != wasm.len() || contents.len() != KEY_LEN + SIGNATURE_LEN {
            return Err(Error::Signature);
        }
        let (key, signature) = contents.split_at(KEY_LEN);
        Ok(Self {
            message: &wasm[..start],
            key: key.try_into().map_err(|_| Error::Signature)?,
            signature: signature.try_into().map_err(|_| Error::Signature)?,
        })
    }
}

/// The contents of the custom section called `name`, if the module has one.
fn section<'a>(wasm: &'a [u8], name: &str) -> Result<Option<&'a [u8]>, Error> {
    let mut found = None;
    walk(wasm, |_, _, own, contents| {
        if own == name.as_bytes() && found.replace(contents).is_some() {
            return Err(Error::Twice);
        }
        Ok(())
    })?;
    Ok(found)
}

/// Calls `each` with the start, end, name and contents of every custom section.
///
/// A module is a header and then sections, each an id byte and a length; a custom section has
/// id 0 and starts with its name. That is all this reads -- everything else is skipped by its
/// length, unvalidated, because validating is the loader's job and costs 9.5 ms.
fn walk<'a>(
    wasm: &'a [u8],
    mut each: impl FnMut(usize, usize, &'a [u8], &'a [u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut rest = wasm
        .strip_prefix(b"\0asm\x01\0\0\0")
        .ok_or(Error::NotWasm)?;
    while let Some((&id, tail)) = rest.split_first() {
        let start = wasm.len() - rest.len();
        let (size, tail) = leb128(tail).ok_or(Error::NotWasm)?;
        let body = tail.get(..size).ok_or(Error::NotWasm)?;
        rest = &tail[size..];
        if id != 0 {
            continue;
        }
        let (len, body) = leb128(body).ok_or(Error::NotWasm)?;
        let name = body.get(..len).ok_or(Error::NotWasm)?;
        each(start, wasm.len() - rest.len(), name, &body[len..])?;
    }
    Ok(())
}

/// An unsigned LEB128 of at most 32 bits, and what follows it.
fn leb128(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let mut value: u32 = 0;
    for (i, &byte) in bytes.iter().enumerate().take(5) {
        value |= u32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value as usize, &bytes[i + 1..]));
        }
    }
    None
}
