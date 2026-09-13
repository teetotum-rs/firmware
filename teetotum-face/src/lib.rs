//! Write a face for teetotum: a plugin that the firmware loads at run time and shows on the
//! glass.
//!
//! A face is a WebAssembly module. While it is shown it gets the taps and wipes on the glass,
//! the knob if it asks for it, and a way to say what should be on the glass -- never the glass
//! itself. **The firmware draws, the face only names what to draw**: a pixel loop in the
//! interpreter runs about thirty times slower than the same loop in the firmware, while a whole
//! face of three calls is drawn in 19 µs, measured on the board.
//!
//! ```ignore
//! #![no_std]
//!
//! use teetotum_face::{Colour, Event, Face, Rights, Size, face};
//!
//! struct Hello {
//!     tapped: bool,
//! }
//!
//! impl Face for Hello {
//!     fn event(&mut self, event: Event) -> bool {
//!         if event != Event::Tap {
//!             return false;
//!         }
//!         self.tapped = !self.tapped;
//!         true
//!     }
//!
//!     fn draw(&self) {
//!         let line = if self.tapped { "tapped" } else { "hello" };
//!         teetotum_face::text(line, 180, 180, Size::Large, Colour::NAME);
//!     }
//! }
//!
//! face! {
//!     name: "Hello",
//!     summary: "says hello",
//!     icon: HELLO, // an `Icon`, drawn as ASCII art
//!     rights: Rights::NONE,
//!     face: Hello = Hello { tapped: false },
//! }
//! ```
//!
//! The picture is 360 by 360 pixels, (0, 0) at the top left, and a face draws it upright: the
//! firmware turns it with the rest of the picture to wherever the user has the knob standing.
//! Every [`draw`](Face::draw) starts from black.
//!
//! **One box of the picture is not the face's: [`HINT`].** Holding a finger on the glass leads
//! home from every face, and the firmware says so there, after the face has drawn -- a face does
//! not have to, and cannot cover it. Whatever a face draws into that box ends up under the hint.
//!
//! # Building
//!
//! For `wasm32v1-none`, with four linker flags. `plugins/hid-remote/.cargo/config.toml` in the
//! firmware repository is the template:
//!
//! ```toml
//! [build]
//! target = "wasm32v1-none"
//!
//! [target.wasm32v1-none]
//! rustflags = [
//!   "-C", "link-arg=-zstack-size=4096",
//!   "-C", "link-arg=--initial-memory=65536",
//!   "-C", "link-arg=--max-memory=65536",
//!   "-C", "link-arg=--import-memory",
//! ]
//! ```
//!
//! Without the first three, rustc asks for a 1 MiB stack and the module for 1088 KiB of memory,
//! and the firmware refuses it: a face gets one page of 64 KiB. Without the fourth, the module
//! brings its own page, which would then have to come out of the chip's internal RAM -- 76.6 KB
//! of it, where an imported page in external RAM costs 11.3 KB (measured) -- so the firmware
//! refuses that too.
//!
//! # What the firmware refuses
//!
//! At load, before any code of the face has run:
//!
//! - a module without a manifest, or with one this version cannot read;
//! - memory that is not imported, or more than one page of it;
//! - an import that is not one of this crate's functions;
//! - [`send`] without [`Rights::HID`] in the manifest, [`random`] without [`Rights::RANDOM`],
//!   [`nearby`] without [`Rights::RADIO`] or [`pulse`] without [`Rights::HAPTIC`] -- a face can
//!   do no more than it says.
//!
//! While it runs, a call traps if it takes more than [`abi::FUEL`], draws outside
//! [`Face::draw`], passes something the firmware cannot draw, asks for random bytes past
//! [`abi::RANDOM_MAX`], or calls [`random`], [`nearby`] or [`pulse`] from [`Face::draw`]. After
//! a trap the face is stopped, the firmware says so on the glass, and the rest of the device
//! carries on.

#![no_std]

pub mod abi;
mod icon;
pub mod manifest;

#[cfg(target_arch = "wasm32")]
mod guest;

#[cfg(target_arch = "wasm32")]
pub use guest::{arc, icon, nearby, pulse, random, send, text};
pub use icon::Icon;
pub use manifest::Rights;

/// A face: what it does when something happens, and what it looks like.
///
/// One value of it lives for as long as the face is loaded, and the firmware calls it from one
/// thread, one call at a time. [`face!`] makes it the module's.
pub trait Face {
    /// Something happened on the glass or, with [`Rights::KNOB`], at the knob. Answers whether
    /// the face has to be drawn again.
    fn event(&mut self, event: Event) -> bool;

    /// Draws the whole face, on black, through [`text`], [`arc`] and [`icon`]. Called after an
    /// event that answered `true`, and whenever the firmware needs the picture again.
    fn draw(&self);
}

/// A box in the picture, in pixels, `right` and `bottom` not included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Where the firmware writes "hold for home" on every face, below the middle. A face keeps it
/// free.
///
/// It sits in the gap at the foot of a 270-degree arc of radius 176, the shape of the player's
/// volume, and inside a full ring of radius 160 -- the two shapes round glass invites.
pub const HINT: Area = Area {
    left: 120,
    top: 288,
    right: 240,
    bottom: 312,
};

/// Makes a type the module's face, and writes its manifest.
///
/// ```ignore
/// face! {
///     name: "HID remote",       // 1 to 20 bytes; it stands in the middle of the settings ring
///     summary: "remote for the phone's player", // 0 to 32 bytes; under the name at home
///     icon: ICON,               // an `Icon`
///     rights: Rights::HID,      // what the face may do beyond drawing
///     face: Remote = Remote { last: None },  // the type, and its value at load
/// }
/// ```
///
/// The initial value is a constant: the module starts with it in its memory, and no code of the
/// face runs until the first event. The macro also brings the panic handler -- a panic is a trap,
/// and a trap stops the face.
#[macro_export]
macro_rules! face {
    (
        name: $name:expr,
        summary: $summary:expr,
        icon: $icon:expr,
        rights: $rights:expr,
        face: $face:ty = $init:expr $(,)?
    ) => {
        // `manifest::SECTION`, spelt out: an attribute takes only a literal.
        #[unsafe(link_section = "teetotum.manifest")]
        #[used]
        static __TEETOTUM_MANIFEST: [u8; $crate::manifest::LEN] =
            $crate::manifest::encode($name, $summary, &$icon, $rights);

        static mut __TEETOTUM_FACE: $face = $init;

        #[unsafe(no_mangle)]
        pub extern "C" fn on_event(event: u32) -> u32 {
            let Some(event) = $crate::Event::from_u32(event) else {
                return 0;
            };
            // SAFETY: the firmware calls a face from one thread and one call at a time, so this
            // is the only reference to it while it lives.
            let face = unsafe { &mut *&raw mut __TEETOTUM_FACE };
            u32::from($crate::Face::event(face, event))
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn draw() {
            // SAFETY: as in `on_event`.
            let face = unsafe { &*&raw const __TEETOTUM_FACE };
            $crate::Face::draw(face);
        }

        #[panic_handler]
        fn panic(_: &core::panic::PanicInfo) -> ! {
            core::arch::wasm32::unreachable()
        }
    };
}

/// What reaches a face. Directions are the picture's: left is where the finger went, in the
/// picture as the user sees it, whichever way the knob is standing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Event {
    /// A finger put down and lifted again without wiping. A long press is not an event: it
    /// leads home, whatever face is up, and no face can take that away.
    Tap = 0,
    WipeLeft = 1,
    WipeRight = 2,
    /// One detent clockwise. Only with [`Rights::KNOB`].
    Clockwise = 3,
    /// One detent anticlockwise. Only with [`Rights::KNOB`].
    Anticlockwise = 4,
    WipeUp = 5,
    WipeDown = 6,
    /// A new round of what the radio hears nearby is in; [`nearby`] reads it. Only with
    /// [`Rights::RADIO`], and only while the face is on the glass: once when it comes up, so that
    /// it starts from what is known, and again after every round.
    Nearby = 7,
    /// A phone is connected to the other chip over BLE HID, so what [`send`] sends reaches one.
    /// Only with [`Rights::HID`], and only while the face is on the glass: once when it comes up,
    /// and again whenever the other chip reports a change.
    Linked = 8,
    /// No phone is connected over BLE HID, and what [`send`] sends is lost without a word. As
    /// [`Event::Linked`].
    Unlinked = 9,
}

impl Event {
    pub const fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Tap,
            1 => Self::WipeLeft,
            2 => Self::WipeRight,
            3 => Self::Clockwise,
            4 => Self::Anticlockwise,
            5 => Self::WipeUp,
            6 => Self::WipeDown,
            7 => Self::Nearby,
            8 => Self::Linked,
            9 => Self::Unlinked,
            _ => return None,
        })
    }
}

/// Which radio [`nearby`] tells about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Radio {
    /// Wi-Fi networks, one entry per access point.
    Wifi = 0,
    /// Bluetooth LE devices that advertise.
    Bluetooth = 1,
}

impl Radio {
    pub const fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Wifi,
            1 => Self::Bluetooth,
            _ => return None,
        })
    }
}

/// One thing the radio heard, a Wi-Fi network or a Bluetooth device, as [`nearby`] hands it
/// over.
///
/// **There is no address in it.** In its place stands [`Signal::key`]: the same network or
/// device keeps the same key while the firmware runs, which is all a face needs to follow one.
/// The firmware makes it from the address and a number it draws at boot and keeps to itself,
/// so a key cannot be looked up in a list of addresses, and a key from before a restart means
/// nothing after it. A Bluetooth device that changes its own address -- phones do, every few
/// minutes -- comes back under a new key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Signal {
    key: u32,
    strength: i8,
    channel: u8,
    name_len: u8,
    reserved: u8,
    name: [u8; abi::NAME_BYTES],
}

const _: () = assert!(core::mem::size_of::<Signal>() == abi::SIGNAL_BYTES);

impl Signal {
    /// Nothing heard: what an array of them starts as.
    pub const EMPTY: Self = Self {
        key: 0,
        strength: 0,
        channel: 0,
        name_len: 0,
        reserved: 0,
        name: [0; abi::NAME_BYTES],
    };

    /// Stands for the address; see the notes on the type.
    pub const fn key(&self) -> u32 {
        self.key
    }

    /// How strongly it was heard, in dBm. Next to the knob that is about -30, at the edge of
    /// hearing about -95.
    pub const fn strength(&self) -> i8 {
        self.strength
    }

    /// The Wi-Fi channel, or `None` for Bluetooth, which moves between three.
    pub const fn channel(&self) -> Option<u8> {
        if self.channel == 0 {
            None
        } else {
            Some(self.channel)
        }
    }

    /// Whether it gave a name. A hidden network does not, and nor do most Bluetooth devices: a
    /// name costs bytes in a 31-byte packet.
    pub const fn has_name(&self) -> bool {
        self.name_len != 0
    }

    /// The network's name or the device's, or `""` if it gave none.
    pub fn name(&self) -> &str {
        let len = (self.name_len as usize).min(abi::NAME_BYTES);
        // SAFETY: a `Signal` only ever holds `EMPTY` or a record the firmware wrote with
        // `Signal::record`, which cuts at a character boundary; its fields are private, so safe
        // code cannot put anything else into it. Checking again would put the UTF-8 validation
        // into every face that shows a name.
        unsafe { core::str::from_utf8_unchecked(&self.name[..len]) }
    }

    /// A record as the firmware writes it for [`nearby`]. The name is cut to
    /// [`abi::NAME_BYTES`] at a character boundary, so what a face reads back is always whole
    /// UTF-8 -- which [`text`] insists on.
    pub fn record(key: u32, strength: i8, channel: u8, name: &str) -> [u8; abi::SIGNAL_BYTES] {
        let mut end = name.len().min(abi::NAME_BYTES);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = [0u8; abi::SIGNAL_BYTES];
        out[..4].copy_from_slice(&key.to_le_bytes());
        out[4] = strength as u8;
        out[5] = channel;
        out[6] = end as u8;
        out[8..8 + end].copy_from_slice(&name.as_bytes()[..end]);
        out
    }
}

/// Where the bytes from [`random`] came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Physical noise went into them: the radio was running, and with it the entropy source
    /// the chip's generator mixes in.
    Physical,
    /// No entropy source was running, so they are only as good as a pseudo-random generator.
    Pseudo,
}

/// How large [`text`] is set: the firmware's Helvetica in its three sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Size {
    /// 14 pixels, with Latin-1: anything that explains.
    Small = 0,
    /// 18 pixels, with Latin-1: running text, a title.
    Body = 1,
    /// Bold, 24 pixels, ASCII only: a name, a value to be read off the glass.
    Large = 2,
}

impl Size {
    pub const fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Small,
            1 => Self::Body,
            2 => Self::Large,
            _ => return None,
        })
    }
}

/// A colour: one of the theme's, which follows the user's choice of theme, or a fixed RGB565.
///
/// **Prefer the theme's.** A face drawn in them looks like part of the device in every theme
/// the firmware has, and in every one it will get.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colour(u32);

/// Where the theme's roles start. Everything below is RGB565.
const THEME: u32 = 0x0001_0000;

impl Colour {
    pub const RING: Self = Self::theme(Role::Ring);
    pub const SELECTED: Self = Self::theme(Role::Selected);
    pub const EMPTY: Self = Self::theme(Role::Empty);
    pub const ICON: Self = Self::theme(Role::Icon);
    pub const NAME: Self = Self::theme(Role::Name);
    pub const VALUE: Self = Self::theme(Role::Value);
    pub const QUIET: Self = Self::theme(Role::Quiet);

    /// A fixed colour, written the way every colour picker shows it: `Colour::rgb(0xFFB547)`.
    pub const fn rgb(hex: u32) -> Self {
        let (r, g, b) = ((hex >> 16) & 0xFF, (hex >> 8) & 0xFF, hex & 0xFF);
        Self(((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3))
    }

    const fn theme(role: Role) -> Self {
        Self(THEME | role as u32)
    }

    /// The number that crosses the boundary.
    pub const fn to_raw(self) -> u32 {
        self.0
    }

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// What the firmware paints with, or `None` for a number that is no colour.
    pub const fn paint(self) -> Option<Paint> {
        if self.0 <= 0xFFFF {
            return Some(Paint::Rgb565(self.0 as u16));
        }
        if self.0 & !0xFF != THEME {
            return None;
        }
        match Role::from_u32(self.0 & 0xFF) {
            Some(role) => Some(Paint::Theme(role)),
            None => None,
        }
    }
}

/// A [`Colour`] as the firmware reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    Rgb565(u16),
    Theme(Role),
}

/// The roles of the firmware's palette, in its own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Role {
    /// The ring's segments: the theme's colour, dark.
    Ring = 0,
    /// The segment the knob is on and the OK button: the theme's colour, bright.
    Selected = 1,
    /// An empty segment: the theme's colour, darker than [`Role::Ring`].
    Empty = 2,
    /// Every icon in the ring.
    Icon = 3,
    /// A name, a label: white.
    Name = 4,
    /// A value to be read off the glass: green.
    Value = 5,
    /// Anything that explains rather than informs: grey.
    Quiet = 6,
}

impl Role {
    pub const fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Ring,
            1 => Self::Selected,
            2 => Self::Empty,
            3 => Self::Icon,
            4 => Self::Name,
            5 => Self::Value,
            6 => Self::Quiet,
            _ => return None,
        })
    }
}

/// What [`send`] can put on the other chip's BLE HID link: usage ids of the consumer page.
///
/// **Exactly the ones the other chip was read to map.** Its report descriptor (at `0x3f41ef38`
/// in the classic chip's factory image) declares thirteen usages as a four-bit array, and the
/// function that fills the report (`0x400da420`) was read to take these ten. Mute, Recall Last
/// and Assign Selection stand in the descriptor, but their mapping was not read, so they are
/// left out rather than guessed. **There is no volume among them**: the descriptor has none.
///
/// Measured at the glass: [`Usage::Next`] skipped the track on a phone paired with
/// `TAIJI_KNOB_HID`, and [`Usage::PlayPause`] paused and resumed. The rest were tried as well
/// and worked, except [`Usage::Pause`], which never took effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Usage {
    Power = 0x30,
    Play = 0xB0,
    Pause = 0xB1,
    Record = 0xB2,
    FastForward = 0xB3,
    Rewind = 0xB4,
    Next = 0xB5,
    Previous = 0xB6,
    Stop = 0xB7,
    PlayPause = 0xCD,
}

impl Usage {
    pub const fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            0x30 => Self::Power,
            0xB0 => Self::Play,
            0xB1 => Self::Pause,
            0xB2 => Self::Record,
            0xB3 => Self::FastForward,
            0xB4 => Self::Rewind,
            0xB5 => Self::Next,
            0xB6 => Self::Previous,
            0xB7 => Self::Stop,
            0xCD => Self::PlayPause,
            _ => return None,
        })
    }
}
