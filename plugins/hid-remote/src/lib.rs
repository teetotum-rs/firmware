//! The phone remote: a tap or a wipe becomes one consumer-control usage, which the other chip
//! sends to the phone over BLE HID.
//!
//! It ships with the firmware and can be removed like any other face. **It needs no audio**: a
//! phone paired with `TAIJI_KNOB_HID` takes the usages while it plays through its own speaker.
//! The price is that nothing comes back -- no title, no volume -- so the face shows what it
//! sent. **Whether a phone is listening it does learn**: the firmware tells it with
//! `Event::Linked` and `Event::Unlinked`, from bit 6 of the other chip's state. Until a phone is
//! connected it says how to pair.
//!
//! **A detent seeks**, with [`Rights::KNOB`]: clockwise fast forward, anticlockwise rewind. How
//! far one of them jumps is the player's business and not ours. The other chip's HID descriptor
//! carries thirteen usages and no volume among them, so seeking is what a turn can become here
//! -- volume it never could. **The price is that turn:** while this face is up the firmware
//! hands the detents to it and tells the other chip to keep its own volume out of them, so a
//! turn no longer works the volume of whatever plays through the knob. Leave the face for that.

#![no_std]

use teetotum_face::{Colour, Event, Face, Icon, Rights, Size, Usage, face};

/// Play and pause side by side: what the face does, and its icon in the settings ring.
const ICON: Icon = Icon::new(&[
    "........................",
    "........................",
    "........................",
    "........................",
    "........................",
    "...#..........###..###..",
    "...##.........###..###..",
    "...####.......###..###..",
    "...#####......###..###..",
    "...#######....###..###..",
    "...########...###..###..",
    "...#########..###..###..",
    "...#########..###..###..",
    "...########...###..###..",
    "...#######....###..###..",
    "...#####......###..###..",
    "...####.......###..###..",
    "...##.........###..###..",
    "...#..........###..###..",
    "........................",
    "........................",
    "........................",
    "........................",
    "........................",
]);

struct Remote {
    last: Option<Usage>,
    /// Whether a phone is connected over BLE HID, as the firmware last said.
    linked: bool,
}

impl Face for Remote {
    fn event(&mut self, event: Event) -> bool {
        let usage = match event {
            Event::Tap => Usage::PlayPause,
            // Left is back in time: the previous title, as on a timeline and not a carousel.
            Event::WipeLeft => Usage::Previous,
            Event::WipeRight => Usage::Next,
            // Measured to reach the phone with `firmware/src/bin/hidkeys.rs`; only the player
            // decides how far one press of them goes.
            Event::Clockwise => Usage::FastForward,
            Event::Anticlockwise => Usage::Rewind,
            Event::Linked | Event::Unlinked => {
                self.linked = event == Event::Linked;
                return true;
            }
            _ => return false,
        };
        teetotum_face::send(usage);
        self.last = Some(usage);
        true
    }

    fn draw(&self) {
        let label = match self.last {
            Some(Usage::PlayPause) => "Play/Pause",
            Some(Usage::Next) => "Next",
            Some(Usage::Previous) => "Previous",
            Some(Usage::FastForward) => "Fast forward",
            Some(Usage::Rewind) => "Rewind",
            _ => "HID remote",
        };
        // The rim the player has, without a volume on it: this face cannot know the volume.
        teetotum_face::arc(180, 180, 170, 135, 270, 8, Colour::RING);
        teetotum_face::icon(&ICON, 180, 118, Colour::ICON);
        teetotum_face::text(label, 180, 170, Size::Large, Colour::NAME);
        teetotum_face::text("tap to play or pause", 180, 212, Size::Small, Colour::QUIET);
        teetotum_face::text("swipe to skip", 180, 230, Size::Small, Colour::QUIET);
        teetotum_face::text("turn to seek", 180, 248, Size::Small, Colour::QUIET);
        // Green like every other value to be read off the screen; the way to pair stands where it
        // stood, in the colour it had, until there is a phone to send to.
        match self.linked {
            true => teetotum_face::text("phone connected", 180, 266, Size::Small, Colour::VALUE),
            false => teetotum_face::text(
                "pair TAIJI_KNOB_HID",
                180,
                266,
                Size::Small,
                Colour::SELECTED,
            ),
        }
    }
}

face! {
    name: "HID remote",
    summary: "remote for the phone's player",
    icon: ICON,
    rights: Rights::HID.union(Rights::KNOB),
    face: Remote = Remote { last: None, linked: false },
}
