//! The card's filesystem, read-only: a path in, bytes out.
//!
//! `src/bin/sdcard.rs` walked the FAT once to answer a question nobody could
//! take on trust -- what is actually on the card inside the closed case. That walk was a
//! measurement and it printed; this is the same knowledge turned into something the firmware
//! can call. A plugin that comes off the card, a font, a background: all of them are a path and
//! a stream of bytes, and neither of those existed until here.
//!
//! Three things are worth knowing before reading the code.
//!
//! - **The volume owns the card.** [`Volume::mount`] takes an [`SdCard`] and keeps it, because a
//!   cached FAT sector and a card that someone else is also seeking are not compatible.
//!   [`Volume::card`] hands out the block device for the runs that want it raw, and
//!   [`Volume::into_card`] ends the mount to give it back.
//! - **Reading is done in runs, not in blocks.** A read that starts on a sector boundary and
//!   asks for whole sectors goes out as one CMD18 for everything the current cluster still
//!   holds ([`SdCard::read_blocks`]); only the ragged head and tail of a request pass through
//!   the one-sector scratch buffer. On this card's 4 KiB clusters that is eight blocks per
//!   command instead of eight commands.
//! - **The chain is followed once per cluster, and the FAT sector is cached.** One 512-byte FAT
//!   sector holds 128 FAT32 links, which on 4 KiB clusters is half a megabyte of file: reading
//!   a 253 KiB picture consults the table exactly once.
//!
//! Not here, deliberately: writing, creating, deleting, and FAT12. Writing is what turns a
//! filesystem into something that can corrupt a card, and nothing in this firmware needs it yet.
//!
//! # Names
//!
//! Long names are UTF-16 on disk and come back as UTF-8, so the non-ASCII file names on this
//! card -- which an early listing could only print as `?` -- are names like any other.
//! A name longer than [`NAME_MAX`] bytes is truncated at a character boundary rather than
//! rejected: this reader is here to find files, and a truncated name simply will not match.
//! Comparison is case-insensitive over ASCII and exact everywhere else, which is what FAT means
//! by case-insensitive anyway.

use crate::sd::{self, SdCard};

/// Every SD card block, and every FAT sector on this card, is 512 bytes.
pub const SECTOR: usize = 512;
/// How much of a name is kept, in bytes of UTF-8.
pub const NAME_MAX: usize = 128;
/// How many UTF-16 units a long name is assembled from before it is truncated.
const LFN_UNITS: usize = 128;
/// A cached sector number that means "nothing cached": no FAT sector and no file sector is ever
/// block 0, which belongs to the partition table.
const NO_SECTOR: u32 = 0;

/// Why a filesystem operation did not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The card itself failed.
    Card(sd::Error),
    /// Block 0 carries neither a partition table nor a boot sector.
    NoSignature,
    /// The boot sector does not describe a FAT volume.
    NotFat,
    /// The volume is FAT12, which this reader does not follow.
    Fat12,
    /// The volume uses a sector size other than 512.
    SectorSize(u32),
    /// No entry of that name in that directory.
    NotFound,
    /// A component of the path is a file, so it cannot be descended into.
    NotADirectory,
    /// The path names a directory where a file was wanted, or the other way round.
    WrongKind,
    /// The cluster chain ends before the file's own size says it should.
    ShortChain,
    /// A read asked for something outside the file.
    OutOfRange,
}

impl From<sd::Error> for Error {
    fn from(error: sd::Error) -> Self {
        Error::Card(error)
    }
}

/// Where a directory's contents begin.
///
/// A FAT16 root directory is a fixed run of sectors outside the data area and has no cluster
/// chain at all; everything else, and every directory on FAT32, is an ordinary chain.
#[derive(Clone, Copy, Debug)]
enum Start {
    Fixed { sector: u32, sectors: u32 },
    Chain { cluster: u32 },
}

/// A directory, as something to list or to look in.
#[derive(Clone, Copy, Debug)]
pub struct Dir {
    start: Start,
}

/// What the boot sector says, in the numbers a reader needs.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    /// First sector of the first file allocation table.
    pub fat_start: u32,
    /// First sector of the data area, from which cluster numbers are counted.
    pub data_start: u32,
    /// Sectors per cluster, always a power of two.
    pub sectors_per_cluster: u32,
    /// How many data clusters the volume has, which is what decides FAT16 against FAT32.
    pub clusters: u32,
    /// FAT32 if true, FAT16 if false.
    pub fat32: bool,
    /// The partition's first sector, or zero on a card formatted without a partition table.
    pub partition_start: u32,
}

impl Layout {
    /// The bytes one cluster holds.
    pub fn cluster_bytes(&self) -> u32 {
        self.sectors_per_cluster * SECTOR as u32
    }

    /// Where a cluster's first sector is.
    fn cluster_lba(&self, cluster: u32) -> u32 {
        self.data_start + (cluster - 2) * self.sectors_per_cluster
    }
}

/// A mounted volume and the card it is on.
pub struct Volume<'d> {
    card: SdCard<'d>,
    layout: Layout,
    root: Start,
    /// One sector of the allocation table, kept because a chain is followed link by link and
    /// consecutive links are almost always in the same sector.
    fat: [u8; SECTOR],
    fat_lba: u32,
}

impl<'d> Volume<'d> {
    /// Find the filesystem on a card and read its boot sector.
    ///
    /// A card formatted by a camera or a phone carries a partition table; one formatted as a
    /// "superfloppy" starts with the boot sector itself. Both are read.
    pub fn mount(mut card: SdCard<'d>) -> Result<Volume<'d>, Error> {
        let mut sector = [0u8; SECTOR];
        card.read_block(0, &mut sector)?;
        if u16::from_le_bytes([sector[510], sector[511]]) != 0xAA55 {
            return Err(Error::NoSignature);
        }

        let mut start = 0u32;
        for slot in 0..4 {
            let entry = &sector[446 + slot * 16..462 + slot * 16];
            let kind = entry[4];
            let first = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
            let count = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]);
            if kind == 0 || count == 0 {
                continue;
            }
            if start == 0 {
                start = first;
            }
        }
        if start != 0 {
            card.read_block(start, &mut sector)?;
        }

        let (layout, root) = parse_boot_sector(&sector, start)?;
        Ok(Volume {
            card,
            layout,
            root,
            fat: [0u8; SECTOR],
            fat_lba: NO_SECTOR,
        })
    }

    /// What the boot sector said.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The block device underneath, for whatever wants the card and not the filesystem.
    pub fn card(&mut self) -> &mut SdCard<'d> {
        &mut self.card
    }

    /// End the mount and return the card, for a writer that would leave this volume's cache stale.
    pub fn into_card(self) -> SdCard<'d> {
        self.card
    }

    /// The root directory.
    pub fn root(&self) -> Dir {
        Dir { start: self.root }
    }

    /// A reader over one directory's entries.
    ///
    /// The `.` and `..` links every subdirectory carries are skipped, as are the volume label
    /// and deleted entries -- what comes back is what a listing would call the contents.
    pub fn entries(&self, dir: Dir) -> Entries {
        Entries::new(dir.start)
    }

    /// Look one name up in one directory.
    pub fn lookup(&mut self, dir: Dir, name: &str) -> Result<Entry, Error> {
        let mut entries = self.entries(dir);
        while let Some(entry) = entries.next(self)? {
            if equal_ignoring_case(entry.name(), name) {
                return Ok(entry);
            }
        }
        Err(Error::NotFound)
    }

    /// Resolve a path, `/` separated, to the entry it names.
    ///
    /// Leading, trailing and doubled separators are ignored, so `/PIC/1.JPG` and `PIC//1.JPG`
    /// are the same path. The root itself is not an entry and comes back as [`Error::NotFound`].
    pub fn entry(&mut self, path: &str) -> Result<Entry, Error> {
        let mut here = self.root();
        let mut found: Option<Entry> = None;
        for part in path.split('/').filter(|part| !part.is_empty()) {
            if let Some(entry) = &found {
                match entry.as_dir() {
                    Some(dir) => here = dir,
                    None => return Err(Error::NotADirectory),
                }
            }
            found = Some(self.lookup(here, part)?);
        }
        found.ok_or(Error::NotFound)
    }

    /// Open a file by path, positioned at its first byte.
    pub fn open(&mut self, path: &str) -> Result<File, Error> {
        let entry = self.entry(path)?;
        entry.open().ok_or(Error::WrongKind)
    }

    /// Resolve a path to a directory.
    pub fn dir(&mut self, path: &str) -> Result<Dir, Error> {
        if path.split('/').all(|part| part.is_empty()) {
            return Ok(self.root());
        }
        let entry = self.entry(path)?;
        entry.as_dir().ok_or(Error::WrongKind)
    }

    /// Follow the allocation table one link.
    ///
    /// `None` is the end of the chain: an end-of-chain marker, a free entry, or a link that
    /// points outside the volume -- all three mean there is no next cluster, and telling them
    /// apart would only matter to a repair tool.
    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>, Error> {
        let width = if self.layout.fat32 { 4 } else { 2 };
        let offset = cluster * width;
        let lba = self.layout.fat_start + offset / SECTOR as u32;
        if self.fat_lba != lba {
            self.card.read_block(lba, &mut self.fat)?;
            self.fat_lba = lba;
        }
        let at = (offset % SECTOR as u32) as usize;
        let entry = if self.layout.fat32 {
            u32::from_le_bytes([
                self.fat[at],
                self.fat[at + 1],
                self.fat[at + 2],
                self.fat[at + 3],
            ]) & 0x0FFF_FFFF
        } else {
            u32::from(u16::from_le_bytes([self.fat[at], self.fat[at + 1]]))
        };
        let end = if self.layout.fat32 {
            0x0FFF_FFF8
        } else {
            0xFFF8
        };
        if entry < 2 || entry >= end || entry >= self.layout.clusters + 2 {
            Ok(None)
        } else {
            Ok(Some(entry))
        }
    }
}

/// Turn a boot sector into the numbers a reader needs.
fn parse_boot_sector(sector: &[u8; SECTOR], start: u32) -> Result<(Layout, Start), Error> {
    let word = |at: usize| u32::from(u16::from_le_bytes([sector[at], sector[at + 1]]));
    let long = |at: usize| {
        u32::from_le_bytes([sector[at], sector[at + 1], sector[at + 2], sector[at + 3]])
    };

    let bytes_per_sector = word(11);
    if bytes_per_sector != SECTOR as u32 {
        return Err(Error::SectorSize(bytes_per_sector));
    }
    let sectors_per_cluster = u32::from(sector[13]);
    let reserved = word(14);
    let fats = u32::from(sector[16]);
    let root_entries = word(17);
    let total = if word(19) != 0 { word(19) } else { long(32) };
    let fat_sectors = if word(22) != 0 { word(22) } else { long(36) };
    if sectors_per_cluster == 0 || fats == 0 || fat_sectors == 0 || total == 0 {
        return Err(Error::NotFat);
    }

    // The root directory of a FAT16 volume is a fixed run of sectors between the tables and the
    // data; on FAT32 it is an ordinary chain and this count is zero.
    let root_sectors = (root_entries * 32).div_ceil(bytes_per_sector);
    let data_start = start + reserved + fats * fat_sectors + root_sectors;
    let clusters = (total - (data_start - start)) / sectors_per_cluster;
    if clusters < 4085 {
        return Err(Error::Fat12);
    }
    let fat32 = clusters >= 65525;

    let layout = Layout {
        fat_start: start + reserved,
        data_start,
        sectors_per_cluster,
        clusters,
        fat32,
        partition_start: start,
    };
    let root = if fat32 {
        Start::Chain { cluster: long(44) }
    } else {
        Start::Fixed {
            sector: start + reserved + fats * fat_sectors,
            sectors: root_sectors,
        }
    };
    Ok((layout, root))
}

/// One entry of a directory.
#[derive(Clone)]
pub struct Entry {
    name: [u8; NAME_MAX],
    length: usize,
    /// The file's length in bytes; zero for a directory, which is what FAT stores.
    pub size: u32,
    /// The first cluster of its contents, or zero for an empty file.
    pub cluster: u32,
    /// Whether it is a directory.
    pub directory: bool,
    /// The attribute byte.
    pub attributes: Attributes,
    /// When it was made, if whoever made it said.
    pub created: Option<Stamp>,
    /// When its contents last changed, if set.
    pub modified: Option<Stamp>,
    /// The day it was last read, if set; FAT keeps no time for it, so the time reads midnight.
    pub accessed: Option<Stamp>,
}

/// The attribute byte of a directory entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attributes(pub u8);

impl Attributes {
    pub fn read_only(self) -> bool {
        self.0 & 0x01 != 0
    }

    pub fn hidden(self) -> bool {
        self.0 & 0x02 != 0
    }

    pub fn system(self) -> bool {
        self.0 & 0x04 != 0
    }

    /// Set by writers when the file changed since the last backup.
    pub fn archive(self) -> bool {
        self.0 & 0x20 != 0
    }
}

/// A date and time as FAT stores it: local time, no zone, from 1980 on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl Stamp {
    /// A packed date and time, or `None` for the zero date writers leave when they have no clock.
    fn decode(date: u16, time: u16) -> Option<Stamp> {
        let (month, day) = (((date >> 5) & 0x0F) as u8, (date & 0x1F) as u8);
        if !(1..=12).contains(&month) || day == 0 {
            return None;
        }
        Some(Stamp {
            year: 1980 + (date >> 9),
            month,
            day,
            hour: (time >> 11) as u8,
            minute: ((time >> 5) & 0x3F) as u8,
            // Two-second steps.
            second: ((time & 0x1F) * 2) as u8,
        })
    }
}

impl Entry {
    /// The name, long if the entry had one and 8.3 otherwise.
    pub fn name(&self) -> &str {
        // The buffer is only ever filled by this module, and only with whole characters.
        core::str::from_utf8(&self.name[..self.length]).unwrap_or("?")
    }

    /// The entry as a directory to descend into, if that is what it is.
    pub fn as_dir(&self) -> Option<Dir> {
        if self.directory && self.cluster >= 2 {
            Some(Dir {
                start: Start::Chain {
                    cluster: self.cluster,
                },
            })
        } else {
            None
        }
    }

    /// The entry as a file to read, if that is what it is.
    pub fn open(&self) -> Option<File> {
        if self.directory {
            return None;
        }
        Some(File {
            first: self.cluster,
            size: self.size,
            position: 0,
            cluster: self.cluster,
            sector: [0u8; SECTOR],
            sector_lba: NO_SECTOR,
        })
    }
}

/// A reader over the entries of one directory.
///
/// It is not an [`Iterator`]: every step needs the card, and the card is the volume's. So the
/// volume is handed in at each call instead of being borrowed for the life of the reader, which
/// is what lets a caller look something up while a listing is still running.
pub struct Entries {
    start: Start,
    /// The cluster being read, for a chained directory.
    cluster: u32,
    /// Which sector within the cluster, or within the fixed root.
    index: u32,
    /// How far into the loaded sector the next entry begins.
    offset: usize,
    sector: [u8; SECTOR],
    loaded: bool,
    done: bool,
    /// The long name being assembled for the entry that is still to come.
    long: [u16; LFN_UNITS],
    long_len: usize,
}

impl Entries {
    fn new(start: Start) -> Self {
        Entries {
            start,
            cluster: match start {
                Start::Chain { cluster } => cluster,
                Start::Fixed { .. } => 0,
            },
            index: 0,
            offset: 0,
            sector: [0u8; SECTOR],
            loaded: false,
            done: false,
            long: [0u16; LFN_UNITS],
            long_len: 0,
        }
    }

    /// The next entry, or `None` at the end of the directory.
    pub fn next(&mut self, volume: &mut Volume<'_>) -> Result<Option<Entry>, Error> {
        loop {
            if self.done {
                return Ok(None);
            }
            if !self.loaded {
                let lba = match self.start {
                    Start::Fixed { sector, sectors } => {
                        if self.index >= sectors {
                            self.done = true;
                            return Ok(None);
                        }
                        sector + self.index
                    }
                    Start::Chain { .. } => {
                        if self.cluster < 2 {
                            self.done = true;
                            return Ok(None);
                        }
                        volume.layout.cluster_lba(self.cluster) + self.index
                    }
                };
                volume.card.read_block(lba, &mut self.sector)?;
                self.loaded = true;
                self.offset = 0;
            }

            while self.offset + 32 <= SECTOR {
                let entry = &self.sector[self.offset..self.offset + 32];
                self.offset += 32;
                match entry[0] {
                    // No entry here and none after it.
                    0x00 => {
                        self.done = true;
                        return Ok(None);
                    }
                    // A deleted entry, and with it the long name being assembled for it.
                    0xE5 => {
                        self.long_len = 0;
                        continue;
                    }
                    _ => {}
                }
                let attributes = entry[11];
                if attributes & 0x0F == 0x0F {
                    collect_long_name(entry, &mut self.long, &mut self.long_len);
                    continue;
                }
                // The volume label is a directory entry like any other, and is not contents.
                if attributes & 0x08 != 0 {
                    self.long_len = 0;
                    continue;
                }

                let mut name = [0u8; NAME_MAX];
                let length = if self.long_len > 0 {
                    encode_utf8(&self.long[..self.long_len], &mut name)
                } else {
                    short_name(entry, &mut name)
                };
                self.long_len = 0;

                // Every subdirectory holds a link to itself and one to its parent.
                let text = core::str::from_utf8(&name[..length]).unwrap_or("");
                if text == "." || text == ".." {
                    continue;
                }

                let word = |at: usize| u16::from_le_bytes([entry[at], entry[at + 1]]);
                // Byte 13 holds hundredths on top of the two-second step; only its whole second counts.
                let created = Stamp::decode(word(16), word(14)).map(|mut stamp| {
                    stamp.second += entry[13].min(199) / 100;
                    stamp
                });
                return Ok(Some(Entry {
                    name,
                    length,
                    attributes: Attributes(attributes),
                    created,
                    modified: Stamp::decode(word(24), word(22)),
                    accessed: Stamp::decode(word(18), 0),
                    size: u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]),
                    cluster: (u32::from(u16::from_le_bytes([entry[20], entry[21]])) << 16)
                        | u32::from(u16::from_le_bytes([entry[26], entry[27]])),
                    directory: attributes & 0x10 != 0,
                }));
            }

            // That sector is spent; move to the next one, following the chain if there is one.
            self.loaded = false;
            self.index += 1;
            if let Start::Chain { .. } = self.start
                && self.index >= volume.layout.sectors_per_cluster
            {
                self.index = 0;
                match volume.next_cluster(self.cluster)? {
                    Some(next) => self.cluster = next,
                    None => {
                        self.done = true;
                        return Ok(None);
                    }
                }
            }
        }
    }
}

/// An open file, positioned somewhere in itself.
pub struct File {
    /// The first cluster of the chain, so a seek backwards can start over.
    first: u32,
    size: u32,
    position: u32,
    /// The cluster the position is in. Meaningful while `position < size`.
    cluster: u32,
    /// One sector, for the ragged ends of a request that does not sit on block boundaries.
    sector: [u8; SECTOR],
    sector_lba: u32,
}

impl File {
    /// The file's length in bytes.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// How far in the next read will start.
    pub fn position(&self) -> u32 {
        self.position
    }

    /// Whether everything has been read.
    pub fn at_end(&self) -> bool {
        self.position >= self.size
    }

    /// Fill as much of `buffer` as the file still holds, and say how much that was.
    ///
    /// Short reads are ordinary: the end of the file ends one, and so does the end of a cluster,
    /// because a run that crosses into the next cluster is not one run on the card. A caller
    /// that wants a buffer filled calls this until it returns zero.
    pub fn read(&mut self, volume: &mut Volume<'_>, buffer: &mut [u8]) -> Result<usize, Error> {
        if self.at_end() || buffer.is_empty() {
            return Ok(0);
        }
        if self.cluster < 2 {
            return Err(Error::ShortChain);
        }
        let layout = volume.layout;
        let cluster_bytes = layout.cluster_bytes();
        let into_cluster = self.position % cluster_bytes;
        let lba = layout.cluster_lba(self.cluster) + into_cluster / SECTOR as u32;
        let into_sector = (into_cluster % SECTOR as u32) as usize;
        let left_in_file = (self.size - self.position) as usize;
        let left_in_cluster = (cluster_bytes - into_cluster) as usize;
        let want = buffer.len().min(left_in_file).min(left_in_cluster);

        let taken = if into_sector != 0 || want < SECTOR {
            // A ragged end: one sector through the scratch buffer, of which part is wanted.
            if self.sector_lba != lba {
                volume.card.read_block(lba, &mut self.sector)?;
                self.sector_lba = lba;
            }
            let take = want.min(SECTOR - into_sector);
            buffer[..take].copy_from_slice(&self.sector[into_sector..into_sector + take]);
            take
        } else {
            // Whole sectors, straight into the caller's buffer and in one command.
            let sectors = want / SECTOR;
            let take = sectors * SECTOR;
            volume.card.read_blocks(lba, &mut buffer[..take])?;
            take
        };

        self.advance(volume, taken as u32)?;
        Ok(taken)
    }

    /// Read exactly `buffer.len()` bytes, or fail.
    ///
    /// [`File::read`] stops at every cluster boundary; a caller that wants a whole structure --
    /// a header, a row of pixels -- wants it filled.
    pub fn read_exact(&mut self, volume: &mut Volume<'_>, buffer: &mut [u8]) -> Result<(), Error> {
        let mut done = 0;
        while done < buffer.len() {
            let taken = self.read(volume, &mut buffer[done..])?;
            if taken == 0 {
                return Err(Error::OutOfRange);
            }
            done += taken;
        }
        Ok(())
    }

    /// Move the position, following the chain from wherever is nearer.
    pub fn seek(&mut self, volume: &mut Volume<'_>, to: u32) -> Result<(), Error> {
        if to > self.size {
            return Err(Error::OutOfRange);
        }
        let cluster_bytes = volume.layout.cluster_bytes();
        let target = to / cluster_bytes;
        let current = self.position / cluster_bytes;
        let (mut cluster, mut index) = if target >= current && self.position < self.size {
            (self.cluster, current)
        } else {
            (self.first, 0)
        };
        while index < target {
            match volume.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Err(Error::ShortChain),
            }
            index += 1;
        }
        self.cluster = cluster;
        self.position = to;
        Ok(())
    }

    /// Move forward by what was just read, following the chain at a cluster boundary.
    fn advance(&mut self, volume: &mut Volume<'_>, by: u32) -> Result<(), Error> {
        let cluster_bytes = volume.layout.cluster_bytes();
        let before = self.position / cluster_bytes;
        self.position += by;
        if self.position >= self.size {
            return Ok(());
        }
        let after = self.position / cluster_bytes;
        for _ in before..after {
            match volume.next_cluster(self.cluster)? {
                Some(next) => self.cluster = next,
                None => return Err(Error::ShortChain),
            }
        }
        Ok(())
    }
}

/// Take the thirteen UTF-16 units one long-name entry carries and put them where they belong.
///
/// The sequence number in the first byte says which thirteen, counting from one; bit `0x40`
/// marks the entry that comes first on disk and last in the name.
fn collect_long_name(entry: &[u8], name: &mut [u16; LFN_UNITS], length: &mut usize) {
    const POSITIONS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let sequence = usize::from(entry[0] & 0x1F);
    if sequence == 0 || sequence > 20 {
        return;
    }
    let base = (sequence - 1) * 13;
    for (slot, at) in POSITIONS.iter().enumerate() {
        let unit = u16::from_le_bytes([entry[*at], entry[*at + 1]]);
        // A name shorter than its entries is padded with a terminator and then with 0xFFFF.
        if unit == 0x0000 || unit == 0xFFFF {
            continue;
        }
        let index = base + slot;
        if index >= name.len() {
            continue;
        }
        name[index] = unit;
        if index + 1 > *length {
            *length = index + 1;
        }
    }
}

/// UTF-16 as the disk stores it into UTF-8 as the rest of the world wants it.
///
/// Surrogate pairs are put back together; an unpaired surrogate becomes `?`, which is what it
/// is. The result stops on a character boundary if the buffer runs out.
fn encode_utf8(units: &[u16], out: &mut [u8; NAME_MAX]) -> usize {
    let mut length = 0;
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        index += 1;
        let code = if (0xD800..0xDC00).contains(&unit) {
            let low = units.get(index).copied().unwrap_or(0);
            if (0xDC00..0xE000).contains(&low) {
                index += 1;
                0x1_0000 + ((u32::from(unit) - 0xD800) << 10) + (u32::from(low) - 0xDC00)
            } else {
                u32::from(b'?')
            }
        } else if (0xDC00..0xE000).contains(&unit) {
            u32::from(b'?')
        } else {
            u32::from(unit)
        };
        let Some(character) = char::from_u32(code) else {
            continue;
        };
        let width = character.len_utf8();
        if length + width > out.len() {
            break;
        }
        character.encode_utf8(&mut out[length..]);
        length += width;
    }
    length
}

/// The 8.3 name, with the padding taken out and the dot put back in.
///
/// Bytes above ASCII are the disk's own code page, which nothing here knows, so they become `?`
/// -- unlike a long name, which says its encoding.
fn short_name(entry: &[u8], out: &mut [u8; NAME_MAX]) -> usize {
    let mut length = 0;
    let put = |byte: u8, out: &mut [u8; NAME_MAX], length: &mut usize| {
        out[*length] = if byte.is_ascii() { byte } else { b'?' };
        *length += 1;
    };
    for &byte in &entry[..8] {
        if byte == b' ' {
            break;
        }
        put(byte, out, &mut length);
    }
    if entry[8] != b' ' {
        out[length] = b'.';
        length += 1;
        for &byte in &entry[8..11] {
            if byte == b' ' {
                break;
            }
            put(byte, out, &mut length);
        }
    }
    length
}

/// FAT's idea of the same name: ASCII case ignored, everything else exact.
fn equal_ignoring_case(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(&b))
}
