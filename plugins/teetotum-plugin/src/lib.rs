//! The teetotum: the spinning top the project is named after, with the knob as its stem.
//!
//! **Every detent is a throw.** Turn the knob and the side changes with each detent; where the
//! hand stops, the side stands; a tap on the screen throws too. Each one is drawn from the chip's random number generator, and
//! while the radio runs -- in the firmware, always -- that generator mixes in physical noise
//! (ESP-IDF, "Random Number Generation": the RF subsystem enables a high-speed ADC as its
//! entropy source). The face says which it got, because a face that promises chance should not
//! quietly hand out a pseudo-random sequence instead.
//!
//! A wipe picks how many sides the top has, from a coin to 256. The ring shows them as
//! segments as long as they fit into the drawing calls a face gets; past twenty, it shows the
//! side as a mark on a plain ring.
//!
//! It asks for two rights: [`Rights::KNOB`], because the knob is what it is about, and
//! [`Rights::RANDOM`] for the bytes.

#![no_std]

use teetotum_face::{Colour, Event, Face, Icon, Rights, Size, Source, face};

/// A teetotum seen from the side: stem, body, point.
const ICON: Icon = Icon::new(&[
    "........................",
    "........................",
    "...........##...........",
    "...........##...........",
    "...........##...........",
    "...........##...........",
    "...........##...........",
    "....################....",
    "...##################...",
    "..####################..",
    "..####################..",
    "...##################...",
    "....################....",
    ".....##############.....",
    "......############......",
    ".......##########.......",
    "........########........",
    ".........######.........",
    "..........####..........",
    "...........##...........",
    "...........##...........",
    "........................",
    "........................",
    "........................",
]);

/// The tops a wipe walks through: a coin, the five Platonic dice, the d10, and then 16, 32, 42,
/// 64, 100, 128 and 256.
const SIDES: [u32; 14] = [2, 4, 6, 8, 10, 12, 16, 20, 32, 42, 64, 100, 128, 256];
/// Where it starts: the common die.
const SIDES_AT_LOAD: usize = 2;
/// Up to this many sides the ring is drawn as segments, one drawing call each.
const SEGMENTS_MAX: u32 = 20;

struct Teetotum {
    /// Which entry of [`SIDES`] the top has.
    sides: usize,
    /// The side it came to rest on, 1-based, and where the randomness came from. `None` until
    /// the first detent, and again after the number of sides changes.
    thrown: Option<(u32, Source)>,
}

impl Teetotum {
    /// One side out of `n`, all equally likely.
    ///
    /// **Not `random % n`**: 2^32 is not a multiple of six, so the low sides would come up a
    /// little more often. A draw from the incomplete last stretch is thrown away and drawn
    /// again; for a hundred sides that happens about once in forty million throws. Each try costs
    /// four of the event's [`RANDOM_MAX`](teetotum_face::abi::RANDOM_MAX) bytes, and a
    /// seventeenth try would trap -- which, at these odds, it never gets to.
    fn throw(n: u32) -> (u32, Source) {
        let span = 1u64 << 32;
        let accept = span - span % u64::from(n);
        loop {
            let mut bytes = [0; 4];
            let source = teetotum_face::random(&mut bytes);
            let draw = u64::from(u32::from_le_bytes(bytes));
            if draw < accept {
                return ((draw % u64::from(n)) as u32 + 1, source);
            }
        }
    }
}

impl Face for Teetotum {
    fn event(&mut self, event: Event) -> bool {
        match event {
            // A tap throws as well as a detent does: the knob is the stem to spin, the screen a
            // table to knock on.
            Event::Clockwise | Event::Anticlockwise | Event::Tap => {
                self.thrown = Some(Self::throw(SIDES[self.sides]));
            }
            // Right is more, the way a number line reads.
            Event::WipeRight => {
                self.sides = (self.sides + 1) % SIDES.len();
                self.thrown = None;
            }
            Event::WipeLeft => {
                self.sides = (self.sides + SIDES.len() - 1) % SIDES.len();
                self.thrown = None;
            }
            _ => return false,
        }
        true
    }

    fn draw(&self) {
        let n = SIDES[self.sides];
        let side = self.thrown.map(|(side, _)| side);
        // Twelve o'clock is -90 degrees; the first side starts there and they run clockwise.
        if n <= SEGMENTS_MAX {
            // A gap of two degrees between segments, and none for a coin, whose two halves
            // would otherwise look like one broken ring.
            let gap = if n > 2 { 2 } else { 0 };
            for k in 0..n {
                let colour = if side == Some(k + 1) {
                    Colour::SELECTED
                } else {
                    Colour::RING
                };
                // Each segment ends where the next begins, both taken from the whole circle:
                // a fixed step of `360 / n` degrees left sixteen sides eight degrees short of
                // closing, as a gap at twelve o'clock.
                let from = (k * 360 / n) as i32;
                let to = ((k + 1) * 360 / n) as i32;
                teetotum_face::arc(
                    180,
                    180,
                    160,
                    -90 + from + gap / 2,
                    to - from - gap,
                    16,
                    colour,
                );
            }
        } else {
            teetotum_face::arc(180, 180, 160, 0, 360, 16, Colour::EMPTY);
            if let Some(side) = side {
                let start = -90 + ((side - 1) * 360 / n) as i32;
                teetotum_face::arc(180, 180, 160, start, 4, 16, Colour::SELECTED);
            }
        }

        let mut label = [0; 12];
        teetotum_face::text(
            sides_label(n, &mut label),
            180,
            100,
            Size::Body,
            Colour::QUIET,
        );
        match self.thrown {
            Some((side, source)) => {
                let mut number = [0; 3];
                teetotum_face::text(
                    decimal(side, &mut number),
                    180,
                    170,
                    Size::Large,
                    Colour::NAME,
                );
                let (says, colour) = match source {
                    Source::Physical => ("true random", Colour::VALUE),
                    Source::Pseudo => ("pseudo-random only", Colour::SELECTED),
                };
                teetotum_face::text(says, 180, 266, Size::Small, colour);
            }
            None => teetotum_face::icon(&ICON, 180, 162, Colour::ICON),
        }
        teetotum_face::text("turn the knob or tap", 180, 212, Size::Small, Colour::QUIET);
        teetotum_face::text("swipe for sides", 180, 230, Size::Small, Colour::QUIET);
    }
}

/// `n` in decimal, without `core::fmt`: formatting machinery would be most of the module.
fn decimal(mut n: u32, buf: &mut [u8; 3]) -> &str {
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 || at == 0 {
            break;
        }
    }
    // SAFETY: only ASCII digits were written from `at` on.
    unsafe { core::str::from_utf8_unchecked(&buf[at..]) }
}

/// `"6 sides"`, into `buf`.
fn sides_label(n: u32, buf: &mut [u8; 12]) -> &str {
    let mut digits = [0; 3];
    let number = decimal(n, &mut digits).as_bytes();
    let tail = b" sides";
    buf[..number.len()].copy_from_slice(number);
    buf[number.len()..number.len() + tail.len()].copy_from_slice(tail);
    // SAFETY: ASCII digits and ASCII letters.
    unsafe { core::str::from_utf8_unchecked(&buf[..number.len() + tail.len()]) }
}

face! {
    name: "Teetotum",
    summary: "a die of 2 to 256 sides",
    icon: ICON,
    rights: Rights::KNOB.union(Rights::RANDOM),
    face: Teetotum = Teetotum { sides: SIDES_AT_LOAD, thrown: None },
}
