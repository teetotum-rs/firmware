//! What the firmware remembers across a reboot.
//!
//! One record in [`teetotum::store`], laid out by hand rather than by a serialisation crate:
//! the whole thing is a handful of bytes, and a hand-written encoder is the only kind whose
//! wire format cannot change under a dependency bump.
//!
//! **A byte of version comes first**, and a payload whose version this build does not know is
//! not repaired -- it is ignored and the defaults stand. That is the cheap direction: settings
//! that fall back to their defaults are an annoyance, settings misread as something else are a
//! motor driven with the wrong calibration.
//!
//! ```text
//! 0   payload version
//! 1   1 if a haptic calibration follows, 0 if not
//! 2   calibration: feedback register
//! 3   calibration: compensation
//! 4   calibration: back-EMF
//! 5   the picture's quarter turns, 0 to 3              twelfths of a turn before version 12
//! 6   the colour theme                                  since version 3
//! 7   reserved, written 0 (was the face, see VERSION)   since version 4
//! 8   reserved, written 0 (was removed plugins, see VERSION) since version 4
//! 9   the screen's brightness, a step from 1 to 10       since version 5
//! 10  how hard the motor clicks, a step from 0 to 9     since version 6
//! 11  how big the cover stands, 0 sharp or 1 full       since version 7
//! 12  whether the cloud moves, 0 still or 1 moving      since version 8
//! 13  the cloud's points, in fifties                    since version 9
//! 14  the cloud's brightest, in percent                 since version 9
//! 15  the cloud's dark centre, in pixels                since version 9
//! 16  the cloud's points in the icon colour, percent    since version 9
//! 17  how many bundled plugins were removed, 0 to 16   since version 11
//! 18  their ids, eight bytes each                      since version 11
//! ```

use teetotum::haptic::Calibration;
use teetotum::menu::{
    PALETTE, PALETTE_BLUE, PALETTE_CYAN, PALETTE_GREEN, PALETTE_GREY, PALETTE_INDIGO,
    PALETTE_MAGENTA, PALETTE_ORANGE, PALETTE_PINK, PALETTE_RED, PALETTE_VIOLET, Palette,
};
use teetotum::screen::ORIENTATIONS;

use crate::plugin::PluginId;

/// The version this build writes.
///
/// **It went from 1 to 2 when the picture orientation arrived**, and a record left by an earlier
/// build was dropped rather than extended. The whole cost of that was one buzz: the first boot
/// found no calibration, ran one, stored it, and every boot after it was quiet again.
///
/// **From 2 to 3 the theme was appended**, and this time the older record is read: version 2 is
/// version 3 without its last byte, so keeping it costs one match arm, where dropping it would
/// cost that buzz again on every board.
///
/// **From 3 to 4 the face and the removed plugins were appended**, the same way. The face byte
/// went unused on the same day, when the home menu took over choosing the face after boot; it
/// is written as 0 and not read, so that the removed plugins stay where version 4 put them.
///
/// **From 4 to 5 the brightness was appended**, the same way again, **from 5 to 6 the
/// strength of the clicks**, **from 6 to 7 the size of the cover**, **from 7 to 8 whether
/// the cloud moves** and **from 8 to 9 the cloud's shape**.
///
/// **From 9 to 10 the removed plugins got a second byte.** Byte 8 holds eight of them, and the
/// rings hold more than eight since they page; a ninth plugin removed would have
/// set the bit of the first. The old byte keeps its meaning, so a version 9 record is read as
/// one whose upper eight plugins are all installed -- which, on a board that had at most eight,
/// is what it says.
///
/// **From 10 to 11 the removed plugins are named by id** ([`PluginId`]) instead of by their place
/// in the firmware's list, so a plugin stays removed wherever it stands, and a signed plugin is
/// told apart from another of the same name. Byte 8 is written 0, byte 17 counts the ids that
/// follow. A version 10 record is read by looking its bits up in the list this build bundles.
///
/// **From 11 to 12 the orientation counts quarter turns** instead of twelfths of a turn, the
/// only turns the panel controller makes. An older orientation is rounded to the nearest quarter.
const VERSION: u8 = 12;

/// How many bytes an encoded record takes.
pub const LEN: usize = LEN_10 + Settings::PLUGINS_MAX * PluginId::LEN;

const VERSION_11: u8 = 11;
const VERSION_10: u8 = 10;
const LEN_10: usize = 18;

/// The previous versions, still read: the same record without its last bytes.
const VERSION_9: u8 = 9;
const LEN_9: usize = 17;
const VERSION_8: u8 = 8;
const LEN_8: usize = 13;
const VERSION_7: u8 = 7;
const LEN_7: usize = 12;
const VERSION_6: u8 = 6;
const LEN_6: usize = 11;
const VERSION_5: u8 = 5;
const LEN_5: usize = 10;
const VERSION_4: u8 = 4;
const LEN_4: usize = 9;
const VERSION_3: u8 = 3;
const LEN_3: usize = 7;
const VERSION_2: u8 = 2;
const LEN_2: usize = 6;

/// Everything the firmware keeps between boots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// The haptic calibration, once one has been run on this board.
    ///
    /// **Its absence is what triggers a calibration**, and its presence is what keeps the boot
    /// quiet. Only the three registers that have to be written back are kept: the diagnostic
    /// status of the run that produced them says something about that run, not about the chip
    /// now, and storing it would invite reading it as if it did.
    pub haptic: Option<StoredCalibration>,
    /// How many quarter turns clockwise the picture stands at, 0 to 3.
    ///
    /// Zero is the default and means the picture stands the way the panel is mounted. There is
    /// no `Option` here because there is no difference worth keeping between "never chosen" and
    /// "chosen to be zero" -- unlike the calibration, nothing has to happen on the first boot.
    pub orientation: u8,
    /// Which colours the settings are drawn in.
    pub theme: Theme,
    /// The plugins that came with the firmware and were removed.
    ///
    /// **Removed rather than installed**, because a bundled plugin ships installed: the record a
    /// fresh board has -- none -- already says so. Removing one frees what a loaded face holds,
    /// its heap and its page; the module itself stays in the firmware image, which is also what
    /// lets it be installed again.
    removed: Removed,
    /// How bright the screen is.
    pub brightness: Brightness,
    /// How hard the motor clicks.
    pub haptics: Haptics,
    /// How big the cover stands behind the player.
    pub cover: CoverStyle,
    /// Whether the cloud behind the menus and the idle player moves.
    pub motion: Motion,
    /// How the cloud looks.
    pub shape: CloudShape,
}

/// Ids of removed plugins, sorted, so that two records removing the same plugins compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Removed {
    ids: [PluginId; Settings::PLUGINS_MAX],
    len: u8,
}

impl Removed {
    fn ids(&self) -> &[PluginId] {
        &self.ids[..usize::from(self.len)]
    }

    fn contains(&self, id: PluginId) -> bool {
        self.ids().binary_search(&id).is_ok()
    }

    fn insert(&mut self, id: PluginId) {
        let len = usize::from(self.len);
        if let Err(at) = self.ids().binary_search(&id)
            && len < self.ids.len()
        {
            self.ids.copy_within(at..len, at + 1);
            self.ids[at] = id;
            self.len += 1;
        }
    }

    fn remove(&mut self, id: PluginId) {
        let len = usize::from(self.len);
        if let Ok(at) = self.ids().binary_search(&id) {
            self.ids.copy_within(at + 1..len, at);
            self.ids[len - 1] = PluginId::default();
            self.len -= 1;
        }
    }

    /// Version 11: `count` ids, of which those this build bundles are kept.
    fn read(bytes: &[u8], count: u8, bundled: &[Option<PluginId>]) -> Self {
        let mut removed = Self::default();
        for chunk in bytes.chunks_exact(PluginId::LEN).take(usize::from(count)) {
            let mut id = [0; PluginId::LEN];
            id.copy_from_slice(chunk);
            let id = PluginId::from_bytes(id);
            if bundled.contains(&Some(id)) {
                removed.insert(id);
            }
        }
        removed
    }

    /// Before version 11: bit `n` for the `n`th bundled plugin.
    fn from_bits(bits: u16, bundled: &[Option<PluginId>]) -> Self {
        let mut removed = Self::default();
        for (n, id) in bundled.iter().enumerate().take(u16::BITS as usize) {
            if let Some(id) = id
                && bits & (1 << n) != 0
            {
                removed.insert(*id);
            }
        }
        removed
    }
}

/// The cloud's shape as the Background menu sets it: the four sliders of the mockup it was
/// chosen at, on the knob.
///
/// **The defaults are the values chosen at that mockup**: 2450 points, the brightest at 98 %, no
/// dark centre, 40 % in the icon colour. Two ranges go further than the mockup's, where its
/// sliders stood at their end: points to 5000 and the icon colour to 100 %. **The dark
/// centre stops at 130 px**: under a menu only the disc inside the ring shows the cloud, and past
/// its 135 px a menu would stand on black.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudShape {
    /// How many points, 100 to 5000 in steps of 50.
    pub points: u16,
    /// How bright a point at the rim is, in percent, 10 to 100 in steps of 2.
    pub brightest: u8,
    /// How far from the centre the darkening reaches zero, 0 to 130 px in steps of 5.
    pub centre: u8,
    /// How many points in a hundred are in the icon colour, 0 to 100 in steps of 5.
    pub accent: u8,
}

/// One of the cloud's four, for [`CloudShape::turned`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Points,
    Brightest,
    Centre,
    Accent,
}

impl Default for CloudShape {
    fn default() -> Self {
        Self {
            points: 2450,
            brightest: 98,
            centre: 0,
            accent: 40,
        }
    }
}

impl CloudShape {
    const PARTS: [Part; 4] = [Part::Points, Part::Brightest, Part::Centre, Part::Accent];

    /// A part's least value, its most and its step.
    const fn range(part: Part) -> (i32, i32, i32) {
        match part {
            Part::Points => (100, 5000, 50),
            Part::Brightest => (10, 100, 2),
            Part::Centre => (0, 130, 5),
            Part::Accent => (0, 100, 5),
        }
    }

    fn get(self, part: Part) -> i32 {
        match part {
            Part::Points => i32::from(self.points),
            Part::Brightest => i32::from(self.brightest),
            Part::Centre => i32::from(self.centre),
            Part::Accent => i32::from(self.accent),
        }
    }

    /// The shape with `part` `detents` steps further on, clockwise more. It stops at both ends,
    /// like the brightness, rather than going round from the most to the least.
    pub fn turned(self, part: Part, detents: i32) -> Self {
        let (least, most, step) = Self::range(part);
        let value = (self.get(part) + detents * step).clamp(least, most);
        let mut shape = self;
        match part {
            Part::Points => shape.points = value as u16,
            Part::Brightest => shape.brightest = value as u8,
            Part::Centre => shape.centre = value as u8,
            Part::Accent => shape.accent = value as u8,
        }
        shape
    }

    fn encode(self, buf: &mut [u8]) {
        buf[0] = (self.points / 50) as u8;
        buf[1] = self.brightest;
        buf[2] = self.centre;
        buf[3] = self.accent;
    }

    /// Four stored bytes back as a shape. If any of them is a value the knob cannot reach, the
    /// whole shape reads as the default, rather than a cloud put together from two records.
    fn from_bytes(bytes: &[u8]) -> Self {
        let shape = Self {
            points: u16::from(bytes[0]) * 50,
            brightest: bytes[1],
            centre: bytes[2],
            accent: bytes[3],
        };
        let reachable = Self::PARTS.iter().all(|&part| {
            let (least, most, step) = Self::range(part);
            let value = shape.get(part);
            (least..=most).contains(&value) && (value - least) % step == 0
        });
        if reachable { shape } else { Self::default() }
    }
}

/// Whether the cloud behind the menus and the idle player moves.
///
/// **Still by default**, with moving as the option: a
/// still ground is drawn when something changes, a moving one every frame, and every frame holds
/// the knob up for as long as the send takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Motion {
    /// Drawn when something else changes.
    #[default]
    Still = 0,
    /// Turning once in two minutes, every point breathing.
    Moving = 1,
}

impl Motion {
    /// Two states, so every odd number of detents flips it.
    pub fn turned(self, detents: i32) -> Self {
        match (self, detents % 2 != 0) {
            (motion, false) => motion,
            (Self::Still, true) => Self::Moving,
            (Self::Moving, true) => Self::Still,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Still => "Still",
            Self::Moving => "Moving",
        }
    }

    /// A stored byte back as a motion. One this build does not know reads as the default.
    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Moving,
            _ => Self::Still,
        }
    }
}

/// How wide a full cover stands: round, and as wide as the inside of the volume arc around the
/// player, whose 8 px stroke is centred on a circle of 352, so 4 px of it lie inside. A cover
/// filling the screen would show past the arc.
pub const COVER_DISC: usize = 344;

/// How big the cover stands behind the player.
///
/// **Sharp by default**: the other chip asks the phone for the 200x200
/// thumbnail, and brought up to the screen it has no more detail, only softer edges. Full screen
/// is there for whoever prefers size to sharpness, which is a matter of taste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoverStyle {
    /// At its own size in the middle.
    #[default]
    Sharp = 0,
    /// Brought up to fill the screen.
    Full = 1,
}

impl CoverStyle {
    /// Two states, so every odd number of detents flips it, like a plugin's Installed.
    pub fn turned(self, detents: i32) -> Self {
        match (self, detents % 2 != 0) {
            (style, false) => style,
            (Self::Sharp, true) => Self::Full,
            (Self::Full, true) => Self::Sharp,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sharp => "Sharp",
            Self::Full => "Full screen",
        }
    }

    /// What `teetotum::cover::show` is asked for.
    pub fn size(self) -> teetotum::cover::CoverSize {
        match self {
            Self::Sharp => teetotum::cover::CoverSize::Native,
            Self::Full => teetotum::cover::CoverSize::Disc(COVER_DISC),
        }
    }

    /// A stored byte back as a style. One this build does not know reads as the default.
    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Full,
            _ => Self::Sharp,
        }
    }
}

/// How hard the motor clicks, as one of ten steps: off, then nine strengths. What a step comes to
/// at the driver is `main`'s business, next to the clicks themselves.
///
/// **The default is the strongest**, because that is what every click was before there was a
/// choice: a record written before version 6 comes back feeling the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Haptics(u8);

impl Default for Haptics {
    fn default() -> Self {
        Self::MAX
    }
}

impl Haptics {
    /// No clicks at all.
    pub const OFF: Self = Self(0);
    /// The clicks as they were before this setting.
    pub const MAX: Self = Self(9);

    /// The step `detents` further on, clockwise stronger. It stops at both ends, like the
    /// brightness: one detent past the strongest would otherwise be off.
    pub fn turned(self, detents: i32) -> Self {
        Self((self.0 as i32 + detents).clamp(Self::OFF.0 as i32, Self::MAX.0 as i32) as u8)
    }

    /// The step, 0 for off to 9.
    pub fn step(self) -> u8 {
        self.0
    }

    /// A stored byte back as a step. One outside the range reads as the default.
    fn from_byte(byte: u8) -> Self {
        if byte <= Self::MAX.0 {
            Self(byte)
        } else {
            Self::default()
        }
    }
}

/// How bright the screen is, as one of ten steps; what a step comes to on the pin is
/// `backlight`'s business.
///
/// **The default is the brightest**, because that is what the screen was before there was a
/// choice: a record written before version 5 comes back looking the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brightness(u8);

impl Default for Brightness {
    fn default() -> Self {
        Self::MAX
    }
}

impl Brightness {
    /// The dimmest step. Not zero: a dark screen is one on which nobody can find the way back.
    pub const MIN: Self = Self(1);
    /// The backlight on all the time.
    pub const MAX: Self = Self(10);

    /// The step `detents` further on, clockwise brighter.
    ///
    /// **It stops at both ends instead of going round** like the theme does: one detent past the
    /// brightest would otherwise be the darkest, on the one setting where the darkest can mean
    /// not seeing the screen.
    pub fn turned(self, detents: i32) -> Self {
        Self((self.0 as i32 + detents).clamp(Self::MIN.0 as i32, Self::MAX.0 as i32) as u8)
    }

    /// The step, 1 to 10.
    pub fn step(self) -> u8 {
        self.0
    }

    /// What the screen calls it: the step in percent of the brightest.
    pub fn percent(self) -> u8 {
        self.0 * 10
    }

    /// A stored byte back as a step. One outside the range reads as the default.
    fn from_byte(byte: u8) -> Self {
        if (Self::MIN.0..=Self::MAX.0).contains(&byte) {
            Self(byte)
        } else {
            Self::default()
        }
    }
}

/// The colour themes the firmware offers: the menu's palette, chosen as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// The ring in teal, the way the menu was first drawn.
    Teal = 0,
    /// The same with the teal turned orange.
    Orange = 1,
    // The neon themes and the grey one. **Appended, never inserted**: the record
    // stores the number, so a theme keeps the number it was stored under.
    Magenta = 2,
    Violet = 3,
    Blue = 4,
    Pink = 5,
    /// The default: the case this firmware runs in is anodised red, and the ring that matches it
    /// is the one a new device should come up in.
    #[default]
    Red = 6,
    Green = 7,
    Cyan = 8,
    Indigo = 9,
    Grey = 10,
}

impl Theme {
    /// Every theme, in the order the knob walks them.
    const ALL: [Theme; 11] = [
        Theme::Teal,
        Theme::Orange,
        Theme::Magenta,
        Theme::Violet,
        Theme::Blue,
        Theme::Pink,
        Theme::Red,
        Theme::Green,
        Theme::Cyan,
        Theme::Indigo,
        Theme::Grey,
    ];

    /// The theme `detents` further on, clockwise positive and round and round.
    pub fn turned(self, detents: i32) -> Self {
        Self::ALL[(self as i32 + detents).rem_euclid(Self::ALL.len() as i32) as usize]
    }

    /// What the screen calls it.
    pub fn name(self) -> &'static str {
        match self {
            Theme::Teal => "Teal",
            Theme::Orange => "Orange",
            Theme::Magenta => "Magenta",
            Theme::Violet => "Violet",
            Theme::Blue => "Blue",
            Theme::Pink => "Pink",
            Theme::Red => "Red",
            Theme::Green => "Green",
            Theme::Cyan => "Cyan",
            Theme::Indigo => "Indigo",
            Theme::Grey => "Grey",
        }
    }

    /// The colours it stands for.
    pub fn palette(self) -> &'static Palette {
        match self {
            Theme::Teal => &PALETTE,
            Theme::Orange => &PALETTE_ORANGE,
            Theme::Magenta => &PALETTE_MAGENTA,
            Theme::Violet => &PALETTE_VIOLET,
            Theme::Blue => &PALETTE_BLUE,
            Theme::Pink => &PALETTE_PINK,
            Theme::Red => &PALETTE_RED,
            Theme::Green => &PALETTE_GREEN,
            Theme::Cyan => &PALETTE_CYAN,
            Theme::Indigo => &PALETTE_INDIGO,
            Theme::Grey => &PALETTE_GREY,
        }
    }

    /// A stored byte back as a theme. One this build does not know reads as the default, for the
    /// same reason as an orientation outside the dial.
    fn from_byte(byte: u8) -> Self {
        Self::ALL.get(byte as usize).copied().unwrap_or_default()
    }
}

/// The part of a [`Calibration`] that has to be written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredCalibration {
    pub feedback: u8,
    pub compensation: u8,
    pub back_emf: u8,
}

impl From<&Calibration> for StoredCalibration {
    fn from(cal: &Calibration) -> Self {
        Self {
            feedback: cal.feedback,
            compensation: cal.compensation,
            back_emf: cal.back_emf,
        }
    }
}

impl From<StoredCalibration> for Calibration {
    fn from(s: StoredCalibration) -> Self {
        Calibration {
            // A stored calibration is one that was kept, so it passed; the status byte is the
            // one thing here that is invented rather than remembered, and it is invented as
            // "nothing to report".
            passed: true,
            status: 0,
            compensation: s.compensation,
            back_emf: s.back_emf,
            feedback: s.feedback,
        }
    }
}

impl Settings {
    /// How many removed plugins a record keeps.
    pub const PLUGINS_MAX: usize = 16;

    /// Whether the bundled plugin `id` is installed.
    pub fn installed(&self, id: PluginId) -> bool {
        !self.removed.contains(id)
    }

    /// Past [`Self::PLUGINS_MAX`] removed plugins a removal is not kept. It does not come to that:
    /// the firmware bundles no more than that many, and a record keeps only their ids.
    pub fn set_installed(&mut self, id: PluginId, installed: bool) {
        if installed {
            self.removed.remove(id);
        } else {
            self.removed.insert(id);
        }
    }

    /// Write the record into `buf` and return how much of it was used.
    pub fn encode(&self, buf: &mut [u8; LEN]) -> usize {
        buf[0] = VERSION;
        match self.haptic {
            Some(cal) => {
                buf[1] = 1;
                buf[2] = cal.feedback;
                buf[3] = cal.compensation;
                buf[4] = cal.back_emf;
            }
            None => buf[1..5].fill(0),
        }
        buf[5] = self.orientation;
        buf[6] = self.theme as u8;
        // Was the face, then the removed plugins' low bits; see [`VERSION`].
        buf[7] = 0;
        buf[8] = 0;
        buf[9] = self.brightness.0;
        buf[10] = self.haptics.0;
        buf[11] = self.cover as u8;
        buf[12] = self.motion as u8;
        self.shape.encode(&mut buf[13..17]);
        let ids = self.removed.ids();
        buf[17] = self.removed.len;
        for (id, out) in ids
            .iter()
            .zip(buf[LEN_10..].chunks_exact_mut(PluginId::LEN))
        {
            out.copy_from_slice(&id.bytes());
        }
        LEN_10 + ids.len() * PluginId::LEN
    }

    /// Read a record back, or fall back to the defaults when it is not one this build knows.
    ///
    /// `bundled` are the ids of the plugins this build bundles, in their order: a record before
    /// version 11 names removed plugins by that order, and of a later one's ids only these are
    /// kept.
    pub fn decode(bytes: &[u8], bundled: &[Option<PluginId>]) -> Self {
        // Byte 7 of version 4 was the face, which the home menu chooses now.
        let (theme, bits, brightness, haptics) = match bytes.first() {
            Some(&VERSION) | Some(&VERSION_11) | Some(&VERSION_10) | Some(&VERSION_9)
            | Some(&VERSION_8) | Some(&VERSION_7) | Some(&VERSION_6)
                if bytes.len() >= LEN_6 =>
            {
                (
                    Theme::from_byte(bytes[6]),
                    u16::from(bytes[8]),
                    Brightness::from_byte(bytes[9]),
                    Haptics::from_byte(bytes[10]),
                )
            }
            // Written before the clicks could be made softer, so they were not.
            Some(&VERSION_5) if bytes.len() >= LEN_5 => (
                Theme::from_byte(bytes[6]),
                u16::from(bytes[8]),
                Brightness::from_byte(bytes[9]),
                Haptics::default(),
            ),
            // Written before the brightness could be turned down, so it was not.
            Some(&VERSION_4) if bytes.len() >= LEN_4 => (
                Theme::from_byte(bytes[6]),
                u16::from(bytes[8]),
                Brightness::default(),
                Haptics::default(),
            ),
            // Written before there were plugins to remove, so nothing was removed.
            Some(&VERSION_3) if bytes.len() >= LEN_3 => (
                Theme::from_byte(bytes[6]),
                0,
                Brightness::default(),
                Haptics::default(),
            ),
            // Written before there was a theme to choose, so none was chosen.
            Some(&VERSION_2) if bytes.len() >= LEN_2 => (
                Theme::default(),
                0,
                Brightness::default(),
                Haptics::default(),
            ),
            _ => return Self::default(),
        };
        // Written before the cover's size could be chosen, so it stood as it does by default.
        let cover = match bytes.first() {
            Some(&VERSION) | Some(&VERSION_11) | Some(&VERSION_10) | Some(&VERSION_9)
            | Some(&VERSION_8) | Some(&VERSION_7)
                if bytes.len() >= LEN_7 =>
            {
                CoverStyle::from_byte(bytes[11])
            }
            _ => CoverStyle::default(),
        };
        // Written before the cloud could move, so it stood still.
        let motion = match bytes.first() {
            Some(&VERSION) | Some(&VERSION_11) | Some(&VERSION_10) | Some(&VERSION_9)
            | Some(&VERSION_8)
                if bytes.len() >= LEN_8 =>
            {
                Motion::from_byte(bytes[12])
            }
            _ => Motion::default(),
        };
        // Written before the cloud could be shaped, so it had the shape it was chosen with.
        let shape = match bytes.first() {
            Some(&VERSION) | Some(&VERSION_11) | Some(&VERSION_10) | Some(&VERSION_9)
                if bytes.len() >= LEN_9 =>
            {
                CloudShape::from_bytes(&bytes[13..17])
            }
            _ => CloudShape::default(),
        };
        // Version 9 was written before a ring could page, and so before there could be more
        // than eight bundled plugins: the ones above the eighth were all installed.
        let removed = match bytes.first() {
            Some(&VERSION) | Some(&VERSION_11) if bytes.len() >= LEN_10 => {
                Removed::read(&bytes[LEN_10..], bytes[17], bundled)
            }
            Some(&VERSION_10) if bytes.len() >= LEN_10 => {
                Removed::from_bits(bits | u16::from(bytes[17]) << 8, bundled)
            }
            _ => Removed::from_bits(bits, bundled),
        };
        Self {
            haptic: (bytes[1] == 1).then_some(StoredCalibration {
                feedback: bytes[2],
                compensation: bytes[3],
                back_emf: bytes[4],
            }),
            // A stored value outside the dial is not an error worth a variant: the honest
            // reading of one is that the picture stands where it started.
            orientation: match (bytes[0], bytes[5]) {
                (VERSION, quarters) if usize::from(quarters) < ORIENTATIONS => quarters,
                // Twelfths of a turn, rounded to the nearest quarter; 330 degrees is upright.
                (version, step) if version != VERSION && step < 12 => (step + 1) / 3 % 4,
                _ => 0,
            },
            theme,
            removed,
            brightness,
            haptics,
            cover,
            motion,
            shape,
        }
    }
}
