//! Nearby: what the radio hears around the knob, and a way to find one thing among it.
//!
//! **Wi-Fi networks or Bluetooth devices, one dot each, and the stronger the signal the nearer
//! the middle.** Where a dot stands around the middle means nothing -- one antenna cannot tell
//! where a signal comes from -- it only keeps each dot in its place from one round to the next.
//! A wipe changes between Wi-Fi and Bluetooth; the knob walks through what was heard, strongest
//! first, and the middle names the one it is on.
//!
//! **A tap finds that one**, the way a game of hot and cold goes: the motor pulses, the faster
//! the stronger its signal, and falls silent in a round that did not hear it. Carry the knob
//! about and follow the pulse to the headphones that were put down somewhere. A tap stops it.
//!
//! Three rights: [`Rights::KNOB`] to choose, [`Rights::RADIO`] for what is heard, and
//! [`Rights::HAPTIC`] for the pulse. **No address ever reaches it**: the firmware hands a face
//! names, strengths, channels and a key that stands for the address without giving it away.

#![no_std]

use teetotum_face::{Colour, Event, Face, Icon, Radio, Rights, Signal, Size, face};

/// Waves going out from a point.
const ICON: Icon = Icon::new(&[
    "........................",
    ".........######.........",
    "......############......",
    "....################....",
    "..######........######..",
    ".####..............####.",
    "####....########....####",
    ".#....############....#.",
    "....######....######....",
    "...####..........####...",
    "....##....####....##....",
    "........########........",
    "......############......",
    ".......##......##.......",
    "........................",
    "...........##...........",
    "..........####..........",
    ".........######.........",
    ".........######.........",
    "..........####..........",
    "...........##...........",
    "........................",
    "........................",
    "........................",
]);

/// How many of each radio's signals the face keeps: the strongest, since the firmware hands them
/// over strongest first. A dot is a drawing call, and twenty-four leave room for the rest of the
/// face within the sixty-four a draw gets.
const KEPT: usize = 24;

/// The strengths the plot spans, in dBm: stronger than [`STRONG`] stands at [`INNER`], weaker
/// than [`WEAK`] at [`OUTER`].
const STRONG: i32 = -35;
const WEAK: i32 = -95;
/// The plot's radii: just outside the lines in the middle, and just inside the rim.
const INNER: i32 = 90;
const OUTER: i32 = 165;
/// Rings for scale, in dBm.
const SCALE: [i32; 3] = [-50, -70, -90];

/// How often the motor pulses while finding, in milliseconds: at [`WEAK`], at [`STRONG`], and in
/// proportion to the dBm between. Decibels are already a logarithm of the power, and in open
/// space six of them are twice the distance, so equal steps in dBm are about equal steps of the
/// walk.
const PULSE_SLOW: i32 = 1500;
const PULSE_FAST: i32 = 200;

/// Where the dots may stand, in steps of five degrees: from 130 degrees clockwise through 280,
/// which leaves the bottom free for the hint the firmware writes there.
const ANGLE_FIRST: i32 = 26;
const ANGLE_STEPS: u32 = 56;

/// sin(5k degrees) * 1024, for k = 0..=18.
const SINE: [i32; 19] = [
    0, 89, 178, 265, 350, 433, 512, 587, 658, 724, 784, 839, 887, 928, 962, 989, 1008, 1020, 1024,
];

/// The longest name shown, in bytes: about what fits across the middle of the plot. Bytes and
/// not characters, because counting characters costs a face the code to walk UTF-8.
const NAME_SHOWN: usize = 22;

const CX: i32 = 180;
const CY: i32 = 180;

/// What one radio heard in its last round, and which of it the knob is on.
struct List {
    signals: [Signal; KEPT],
    len: usize,
    /// The one the knob is on, as it was last heard -- kept when a round misses it, so that the
    /// choice survives a round and the middle can say it was not heard.
    chosen: Option<Signal>,
    /// Whether the last round heard it.
    chosen_heard: bool,
}

impl List {
    const EMPTY: Self = Self {
        signals: [Signal::EMPTY; KEPT],
        len: 0,
        chosen: None,
        chosen_heard: false,
    };

    fn heard(&self) -> &[Signal] {
        &self.signals[..self.len]
    }

    fn fill(&mut self, radio: Radio) {
        self.len = teetotum_face::nearby(radio, &mut self.signals);
        // By index, as in `step`: copied from one reference into another, the compiler cannot
        // tell that the two do not overlap, and pulls in `memmove` -- 1225 bytes of module.
        if let Some(key) = self.chosen.as_ref().map(Signal::key) {
            match self.heard().iter().position(|s| s.key() == key) {
                Some(i) => {
                    self.chosen = Some(self.signals[i]);
                    self.chosen_heard = true;
                }
                None => self.chosen_heard = false,
            }
        }
    }

    /// Moves the choice one entry weaker, or one stronger; from no choice, to the strongest.
    fn step(&mut self, weaker: bool) -> bool {
        if self.len == 0 {
            return false;
        }
        let at = self
            .chosen
            .and_then(|chosen| self.heard().iter().position(|s| s.key() == chosen.key()));
        let next = match at {
            None => 0,
            Some(i) if weaker => (i + 1) % self.len,
            Some(i) => (i + self.len - 1) % self.len,
        };
        self.chosen = Some(self.signals[next]);
        self.chosen_heard = true;
        true
    }
}

struct Nearby {
    radio: Radio,
    wifi: List,
    bluetooth: List,
    /// Whether a round has come in at all.
    heard: bool,
    /// Whether the motor is following the chosen one.
    finding: bool,
}

impl Nearby {
    fn list(&self) -> &List {
        match self.radio {
            Radio::Wifi => &self.wifi,
            Radio::Bluetooth => &self.bluetooth,
        }
    }

    fn list_mut(&mut self) -> &mut List {
        match self.radio {
            Radio::Wifi => &mut self.wifi,
            Radio::Bluetooth => &mut self.bluetooth,
        }
    }

    /// Sets the pulse to the chosen one's strength, or silences it if the last round missed it.
    fn follow(&self) {
        let list = self.list();
        match &list.chosen {
            Some(chosen) if list.chosen_heard => teetotum_face::pulse(pulse_every(chosen.strength())),
            _ => teetotum_face::pulse(0),
        }
    }

    fn stop_finding(&mut self) {
        if self.finding {
            self.finding = false;
            teetotum_face::pulse(0);
        }
    }

    fn draw_plot(&self) {
        for dbm in SCALE {
            teetotum_face::arc(CX, CY, radius(dbm) as u32, 0, 360, 1, Colour::RING);
        }
        let list = self.list();
        let chosen = list.chosen.as_ref().filter(|_| list.chosen_heard);
        for signal in list.heard() {
            if chosen.is_some_and(|c| c.key() == signal.key()) {
                continue;
            }
            let (x, y) = place(signal);
            let colour = if signal.has_name() { Colour::ICON } else { Colour::QUIET };
            teetotum_face::arc(x, y, 2, 0, 360, 5, colour);
        }
        // The chosen one last, so that it lies on top.
        if let Some(chosen) = chosen {
            let (x, y) = place(chosen);
            teetotum_face::arc(x, y, 4, 0, 360, 8, Colour::SELECTED);
        }

        let (radio, one, many, other) = match self.radio {
            Radio::Wifi => ("Wi-Fi", "network", "networks", "swipe for Bluetooth"),
            Radio::Bluetooth => ("Bluetooth", "device", "devices", "swipe for Wi-Fi"),
        };
        if !self.heard {
            teetotum_face::text(radio, CX, 124, Size::Small, Colour::QUIET);
            teetotum_face::text("listening", CX, 160, Size::Body, Colour::QUIET);
            return;
        }
        let noun = if list.len == 1 { one } else { many };
        let mut title = Line::new();
        title
            .push(radio.as_bytes())
            .push(b", ")
            .number(list.len as i32)
            .push(b" ")
            .push(noun.as_bytes());
        teetotum_face::text(title.as_str(), CX, 124, Size::Small, Colour::QUIET);
        match &list.chosen {
            None => {
                teetotum_face::text("turn to choose", CX, 160, Size::Body, Colour::QUIET);
                teetotum_face::text(other, CX, 214, Size::Small, Colour::QUIET);
            }
            Some(chosen) => {
                self.name_line(chosen, 160);
                if list.chosen_heard {
                    let mut line = Line::new();
                    line.number(chosen.strength().into()).push(b" dBm");
                    if let Some(channel) = chosen.channel() {
                        line.push(b", channel ").number(channel.into());
                    }
                    teetotum_face::text(line.as_str(), CX, 186, Size::Small, Colour::VALUE);
                } else {
                    teetotum_face::text("not heard this round", CX, 186, Size::Small, Colour::SELECTED);
                }
                teetotum_face::text("tap to find", CX, 214, Size::Small, Colour::QUIET);
            }
        }
    }

    fn draw_finding(&self, chosen: &Signal, heard: bool) {
        // The strength as a bar round the rim, with the gap at the foot, as the player's volume.
        teetotum_face::arc(CX, CY, 160, 135, 270, 16, Colour::EMPTY);
        if heard {
            let share = (i32::from(chosen.strength()).clamp(WEAK, STRONG) - WEAK) * 270 / (STRONG - WEAK);
            if share > 0 {
                teetotum_face::arc(CX, CY, 160, 135, share, 16, Colour::SELECTED);
            }
        }
        teetotum_face::text("finding", CX, 112, Size::Small, Colour::QUIET);
        self.name_line(chosen, 146);
        if heard {
            let mut line = Line::new();
            line.number(chosen.strength().into()).push(b" dBm");
            teetotum_face::text(line.as_str(), CX, 184, Size::Large, Colour::VALUE);
        } else {
            teetotum_face::text("--", CX, 184, Size::Large, Colour::QUIET);
            teetotum_face::text("not heard this round", CX, 214, Size::Small, Colour::SELECTED);
        }
        teetotum_face::text("tap to stop", CX, 244, Size::Small, Colour::QUIET);
    }

    /// The name of `signal` at height `y`, cut to [`NAME_SHOWN`] bytes -- or what stands in for a
    /// name it did not give.
    fn name_line(&self, signal: &Signal, y: i32) {
        let name = signal.name().as_bytes();
        if name.is_empty() {
            let none = match self.radio {
                Radio::Wifi => "hidden network",
                Radio::Bluetooth => "no name",
            };
            teetotum_face::text(none, CX, y, Size::Body, Colour::QUIET);
            return;
        }
        let mut line = Line::new();
        if name.len() <= NAME_SHOWN {
            line.push(name);
        } else {
            // Back to the start of a character, so that the line stays whole UTF-8: a byte that
            // continues one reads 0b10xxxxxx.
            let mut end = NAME_SHOWN;
            while end > 0 && name[end] & 0xC0 == 0x80 {
                end -= 1;
            }
            line.push(&name[..end]).push(b"..");
        }
        teetotum_face::text(line.as_str(), CX, y, Size::Body, Colour::NAME);
    }
}

impl Face for Nearby {
    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Nearby => {
                self.wifi.fill(Radio::Wifi);
                self.bluetooth.fill(Radio::Bluetooth);
                self.heard = true;
                if self.finding {
                    self.follow();
                }
            }
            // Clockwise is weaker: the list runs from the middle outwards.
            Event::Clockwise if !self.finding => return self.list_mut().step(true),
            Event::Anticlockwise if !self.finding => return self.list_mut().step(false),
            Event::Tap if self.finding => self.stop_finding(),
            Event::Tap if self.list().chosen.is_some() => {
                self.finding = true;
                self.follow();
            }
            Event::WipeLeft | Event::WipeRight => {
                self.stop_finding();
                self.radio = match self.radio {
                    Radio::Wifi => Radio::Bluetooth,
                    Radio::Bluetooth => Radio::Wifi,
                };
            }
            _ => return false,
        }
        true
    }

    fn draw(&self) {
        let list = self.list();
        match &list.chosen {
            Some(chosen) if self.finding => self.draw_finding(chosen, list.chosen_heard),
            _ => self.draw_plot(),
        }
    }
}

/// How far from the middle a strength stands.
fn radius(dbm: i32) -> i32 {
    INNER + (STRONG - dbm.clamp(WEAK, STRONG)) * (OUTER - INNER) / (STRONG - WEAK)
}

/// How often the motor pulses for a strength, in milliseconds.
fn pulse_every(dbm: i8) -> u32 {
    let above = i32::from(dbm).clamp(WEAK, STRONG) - WEAK;
    (PULSE_SLOW - above * (PULSE_SLOW - PULSE_FAST) / (STRONG - WEAK)) as u32
}

/// Where a signal's dot stands: its strength says how far out, its key which way round.
fn place(signal: &Signal) -> (i32, i32) {
    let step = ANGLE_FIRST + (signal.key() % ANGLE_STEPS) as i32;
    let r = radius(signal.strength().into());
    (CX + r * sine(step + 18) / 1024, CY + r * sine(step) / 1024)
}

/// sin(5 * `step` degrees) * 1024, any step. Angles run clockwise from three o'clock, as the
/// picture's y runs down, so a positive sine is below the middle.
fn sine(step: i32) -> i32 {
    let s = step.rem_euclid(72) as usize;
    match s {
        0..=18 => SINE[s],
        19..=36 => SINE[36 - s],
        37..=54 => -SINE[s - 36],
        _ => -SINE[72 - s],
    }
}

/// A line of text put together without `core::fmt`, whose machinery would be most of the
/// module. Built in place through `&mut`, since a builder that hands itself back by value is a
/// copy of the whole buffer per call. **Only whole UTF-8 goes in** -- whole strings, digits, and
/// names cut at a character's start -- so it comes out whole.
struct Line {
    buf: [u8; 48],
    len: usize,
}

impl Line {
    const fn new() -> Self {
        Self { buf: [0; 48], len: 0 }
    }

    /// Appends `bytes` if all of them fit, and nothing otherwise.
    fn push(&mut self, bytes: &[u8]) -> &mut Self {
        if let Some(to) = self.buf.get_mut(self.len..self.len + bytes.len()) {
            to.copy_from_slice(bytes);
            self.len += bytes.len();
        }
        self
    }

    fn number(&mut self, n: i32) -> &mut Self {
        if n < 0 {
            self.push(b"-");
        }
        let mut digits = [0u8; 10];
        let mut at = digits.len();
        let mut rest = n.unsigned_abs();
        loop {
            at -= 1;
            digits[at] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        self.push(&digits[at..])
    }

    fn as_str(&self) -> &str {
        // SAFETY: only whole UTF-8 went in, one piece after another; see the type.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
}

face! {
    name: "Nearby",
    summary: "Wi-Fi and Bluetooth around you",
    icon: ICON,
    rights: Rights::KNOB.union(Rights::RADIO).union(Rights::HAPTIC),
    face: Nearby = Nearby {
        radio: Radio::Wifi,
        wifi: List::EMPTY,
        bluetooth: List::EMPTY,
        heard: false,
        finding: false,
    },
}
