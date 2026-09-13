//! A placeholder face: it says its name and nothing else happens.
//!
//! **It exists so that the rings overflow.** A menu holds twelve segments, of which the home
//! ring keeps three for the firmware and the settings ring seven; three bundled plugins fit
//! either with room to spare, so paging through a menu needs more faces to test. Building this
//! crate once per name gives as many faces as a test needs, each with its own name in the
//! manifest and its own entry in both rings.
//!
//! The name comes from `TEETOTUM_DUMMY_NAME` at build time; see `build.sh`. It asks for no
//! rights at all, which also makes it the smallest face in the tree -- the one to read first
//! when writing one.

#![no_std]

use teetotum_face::{Colour, Event, Face, Icon, Rights, Size, face};

/// A ring with a dot in it: something that stands in for an icon without pretending to be one.
const ICON: Icon = Icon::new(&[
    "........................",
    "........................",
    ".......##########.......",
    ".....##############.....",
    "....####........####....",
    "...###............###...",
    "..###..............###..",
    "..##................##..",
    ".###................###.",
    ".##......######......##.",
    ".##.....########.....##.",
    ".##.....########.....##.",
    ".##.....########.....##.",
    ".##.....########.....##.",
    ".###................###.",
    "..##................##..",
    "..###..............###..",
    "...###............###...",
    "....####........####....",
    ".....##############.....",
    ".......##########.......",
    "........................",
    "........................",
    "........................",
]);

/// What this one is called, and so what stands in its manifest, in both rings and on the glass.
const NAME: &str = env!("TEETOTUM_DUMMY_NAME");

/// The middle of the glass.
const CENTRE: i32 = 180;

struct Dummy {
    /// Whether a finger has been on the glass since the face came up. The only thing it has to
    /// show, and it is there so that a tap proves the face is running and not a picture.
    touched: bool,
}

impl Face for Dummy {
    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Tap if !self.touched => {
                self.touched = true;
                true
            }
            _ => false,
        }
    }

    fn draw(&self) {
        teetotum_face::icon(&ICON, CENTRE, CENTRE - 60, Colour::ICON);
        teetotum_face::text(NAME, CENTRE, CENTRE, Size::Large, Colour::NAME);
        let line = match self.touched {
            false => "tap the glass",
            true => "tapped",
        };
        teetotum_face::text(line, CENTRE, CENTRE + 34, Size::Body, Colour::VALUE);
        teetotum_face::text(
            "a placeholder face",
            CENTRE,
            CENTRE + 62,
            Size::Small,
            Colour::QUIET,
        );
    }
}

face!(
    name: NAME,
    summary: "placeholder, does nothing",
    icon: ICON,
    rights: Rights::NONE,
    face: Dummy = Dummy { touched: false },
);
