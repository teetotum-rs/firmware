//! The calls into the firmware. Only on wasm32: anywhere else there is no firmware to call.

use crate::{Colour, Icon, Radio, Signal, Size, Source, Usage};

// The module is `abi::MODULE`, and the names are `abi`'s too; an attribute takes only literals.
#[link(wasm_import_module = "teetotum")]
unsafe extern "C" {
    fn send_usage(usage: u32);
    #[link_name = "text"]
    fn host_text(at: *const u8, len: usize, x: i32, y: i32, size: u32, colour: u32);
    #[link_name = "arc"]
    fn host_arc(cx: i32, cy: i32, radius: u32, start: i32, sweep: i32, width: u32, colour: u32);
    #[link_name = "icon"]
    fn host_icon(rows: *const u32, x: i32, y: i32, colour: u32);
    #[link_name = "random"]
    fn host_random(at: *mut u8, len: usize) -> u32;
    #[link_name = "nearby"]
    fn host_nearby(radio: u32, at: *mut u8, max: usize) -> u32;
    #[link_name = "pulse"]
    fn host_pulse(every_ms: u32);
}

/// Fills `into` with what the radio heard in its last round, strongest first, and answers how
/// many entries it filled; the rest of `into` is left as it was.
///
/// Needs [`Rights::RADIO`](crate::Rights::RADIO). Only from [`Face::event`](crate::Face::event),
/// like [`random`] -- the time to call it is [`Event::Nearby`](crate::Event::Nearby), which says
/// a new round is in. A round is one pass over the Wi-Fi channels, or one window of listening
/// for Bluetooth; while a face with this right is on the glass, the firmware runs them back to
/// back, and a new one comes every few seconds.
pub fn nearby(radio: Radio, into: &mut [Signal]) -> usize {
    // SAFETY: the firmware writes whole records, at most `into.len()` of them, at `into` during
    // the call, inside this module's memory; `Signal` is laid out as a record is.
    unsafe { host_nearby(radio as u32, into.as_mut_ptr().cast(), into.len()) as usize }
}

/// Keeps the motor pulsing, once every `every_ms` milliseconds, until the next call; `0` stops
/// it.
///
/// Needs [`Rights::HAPTIC`](crate::Rights::HAPTIC). Only from
/// [`Face::event`](crate::Face::event). **The firmware keeps the time**, since a face has no
/// clock, and it pulses only while the face is on the glass, at the strength the user has set
/// for clicks -- which may be none, so a face should not rely on the pulse alone. Intervals
/// outside [`PULSE_MIN_MS`](crate::abi::PULSE_MIN_MS) to
/// [`PULSE_MAX_MS`](crate::abi::PULSE_MAX_MS) are taken to the nearer end.
pub fn pulse(every_ms: u32) {
    // SAFETY: a plain number across the boundary.
    unsafe { host_pulse(every_ms) }
}

/// Fills `buf` from the chip's random number generator, and says where the bytes came from.
///
/// Needs [`Rights::RANDOM`](crate::Rights::RANDOM) in the manifest. Only from
/// [`Face::event`](crate::Face::event), never from [`Face::draw`](crate::Face::draw): the
/// firmware draws a face again whenever it needs the picture, and a draw that rolled would
/// change what it shows without anything having happened. At most
/// [`RANDOM_MAX`](crate::abi::RANDOM_MAX) bytes per event, over all calls; past that the face
/// traps.
///
/// The bytes are [`Source::Physical`] while the radio runs, which in the firmware is always --
/// Wi-Fi and Bluetooth feed noise from a high-speed ADC into the generator. Should a later
/// firmware run without them, the same call gives [`Source::Pseudo`], and a face that promises
/// chance should say so.
pub fn random(buf: &mut [u8]) -> Source {
    // SAFETY: the firmware writes `buf.len()` bytes at `buf` during the call, inside this
    // module's memory, which is where a `&mut [u8]` of ours points.
    match unsafe { host_random(buf.as_mut_ptr(), buf.len()) } {
        crate::abi::PHYSICAL => Source::Physical,
        _ => Source::Pseudo,
    }
}

/// Sends one consumer-control usage to the phone, over the other chip's BLE HID link.
///
/// Needs [`Rights::HID`](crate::Rights::HID) in the manifest; a face that uses this without it
/// is refused when it is loaded, not when it calls. **Nothing comes back**: the other chip sends
/// the report whether or not a phone is paired with it (as `TAIJI_KNOB_HID`), and a report that
/// nobody receives is lost without a word. At most [`USAGES_MAX`](crate::abi::USAGES_MAX) per
/// event; the rest are dropped.
pub fn send(usage: Usage) {
    // SAFETY: a plain number across the boundary.
    unsafe { send_usage(usage as u32) }
}

/// Sets a line of text, centred on (`x`, `y`). Only from [`Face::draw`](crate::Face::draw),
/// and at most [`TEXT_MAX`](crate::abi::TEXT_MAX) bytes.
///
/// A character the font does not have is left out; the rest of the line still comes out.
pub fn text(line: &str, x: i32, y: i32, size: Size, colour: Colour) {
    // SAFETY: the firmware reads `line` during the call, and only inside this module's memory,
    // which is where a `&str` of ours points.
    unsafe {
        host_text(
            line.as_ptr(),
            line.len(),
            x,
            y,
            size as u32,
            colour.to_raw(),
        )
    }
}

/// Draws an arc around (`cx`, `cy`): `radius` to the middle of the stroke, `width` wide,
/// clockwise from `start` degrees through `sweep` degrees. Zero degrees is three o'clock, and
/// clockwise is clockwise on the glass. Only from [`Face::draw`](crate::Face::draw).
pub fn arc(cx: i32, cy: i32, radius: u32, start: i32, sweep: i32, width: u32, colour: Colour) {
    // SAFETY: plain numbers across the boundary.
    unsafe { host_arc(cx, cy, radius, start, sweep, width, colour.to_raw()) }
}

/// Draws an icon centred on (`x`, `y`). Only from [`Face::draw`](crate::Face::draw).
pub fn icon(icon: &Icon, x: i32, y: i32, colour: Colour) {
    // SAFETY: as for `text`: 24 rows inside this module's memory, read during the call.
    unsafe { host_icon(icon.rows().as_ptr(), x, y, colour.to_raw()) }
}
