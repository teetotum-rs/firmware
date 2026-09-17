//! The knob's settings a sender reads and writes over BLE.
//!
//! ```text
//! 0   FORMAT
//! 1   theme, 0 to THEMES - 1
//! 2   brightness, BRIGHTNESS_MIN to BRIGHTNESS_MAX
//! 3   click strength, 0 (off) to HAPTICS_MAX
//! 4   quarter turns of the picture, 0 to ORIENTATIONS - 1
//! ```
//!
//! The format has its own version, apart from the record the firmware stores, so a new field in
//! the record does not change what a sender reads.

/// Bytes of the settings.
pub const LEN: usize = 5;
/// The format this crate reads and writes.
pub const FORMAT: u8 = 1;
/// How many colour themes there are.
pub const THEMES: u8 = 11;
pub const BRIGHTNESS_MIN: u8 = 1;
pub const BRIGHTNESS_MAX: u8 = 10;
pub const HAPTICS_MAX: u8 = 9;
pub const ORIENTATIONS: u8 = 4;

/// The settings, as steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub theme: u8,
    pub brightness: u8,
    pub haptics: u8,
    pub orientation: u8,
}

impl Settings {
    pub fn encode(&self) -> [u8; LEN] {
        [
            FORMAT,
            self.theme,
            self.brightness,
            self.haptics,
            self.orientation,
        ]
    }

    /// Reads settings; `None` for another length or format, or a step out of range.
    pub fn decode(raw: &[u8]) -> Option<Self> {
        let [format, theme, brightness, haptics, orientation] =
            *<&[u8; LEN]>::try_from(raw).ok()?;
        let settings = Self {
            theme,
            brightness,
            haptics,
            orientation,
        };
        (format == FORMAT && settings.in_range()).then_some(settings)
    }

    fn in_range(&self) -> bool {
        self.theme < THEMES
            && (BRIGHTNESS_MIN..=BRIGHTNESS_MAX).contains(&self.brightness)
            && self.haptics <= HAPTICS_MAX
            && self.orientation < ORIENTATIONS
    }
}
