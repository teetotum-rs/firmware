//! The names and numbers both sides of the boundary agree on.
//!
//! A face never needs these: the functions and the [`face!`](crate::face) macro use them. They
//! are public for the other side -- the firmware's loader is written against this module too,
//! so the two cannot drift apart.

/// The import module every call into the firmware comes from.
pub const MODULE: &str = "teetotum";
/// Where a face's memory is imported from, and under which name: what
/// `wasm-ld --import-memory` writes.
pub const MEMORY_MODULE: &str = "env";
pub const MEMORY: &str = "memory";
/// How many 64 KiB pages a face gets. Exactly one; see the crate's notes on building.
pub const PAGES: u64 = 1;

/// `send_usage(usage: u32)`
pub const SEND_USAGE: &str = "send_usage";
/// `text(at: *const u8, len: u32, x: i32, y: i32, size: u32, colour: u32)`
pub const TEXT: &str = "text";
/// `arc(cx: i32, cy: i32, radius: u32, start: i32, sweep: i32, width: u32, colour: u32)`
pub const ARC: &str = "arc";
/// `icon(rows: *const u32, x: i32, y: i32, colour: u32)`, 24 rows.
pub const ICON: &str = "icon";
/// `random(at: *mut u8, len: u32) -> u32`: fills `len` bytes, answers [`PHYSICAL`] or
/// [`PSEUDO`].
pub const RANDOM: &str = "random";

/// `nearby(radio: u32, at: *mut u8, max: u32) -> u32`: writes up to `max` records of
/// [`SIGNAL_BYTES`] at `at`, strongest first, and answers how many.
pub const NEARBY: &str = "nearby";
/// `pulse(every_ms: u32)`: keeps the motor pulsing, `0` stops it.
pub const PULSE: &str = "pulse";

/// One record of `nearby`, laid out the way `Signal` is:
///
/// ```text
/// 0..4     key, u32 little-endian
/// 4        strength in dBm, i8
/// 5        Wi-Fi channel, 0 for Bluetooth
/// 6        length of the name in bytes, 0 to NAME_BYTES
/// 7        0
/// 8..40    the name, UTF-8, padded with zeros
/// ```
pub const SIGNAL_BYTES: usize = 40;
/// The longest name a record carries: an SSID's 32 bytes.
pub const NAME_BYTES: usize = 32;

/// The shortest and longest interval `pulse` keeps, in milliseconds; anything outside is taken
/// to the nearer one. Shorter than the minimum, the clicks run together into a buzz.
pub const PULSE_MIN_MS: u32 = 150;
pub const PULSE_MAX_MS: u32 = 5000;

/// What `random` answers when the bytes came from physical noise.
pub const PHYSICAL: u32 = 1;
/// What `random` answers when they did not: no entropy source was running, and the chip's
/// generator gave what it had.
pub const PSEUDO: u32 = 0;

/// `on_event(event: u32) -> u32`, nonzero to be drawn again.
pub const ON_EVENT: &str = "on_event";
/// `draw()`
pub const DRAW: &str = "draw";

/// What one call may cost, in wasmi's fuel.
///
/// Measured on the board: an event took 24 and a draw with three calls 58. This is
/// over three hundred draws' worth, and at the rate of those two calls -- a third of a
/// microsecond per unit, an estimate from two points -- about 7 ms: enough for a face that
/// names what to draw, and short enough that one caught in a loop costs the device a stutter
/// rather than its knob.
pub const FUEL: u64 = 20_000;

/// How many drawing calls one `draw` may make.
pub const DRAWS_MAX: usize = 64;
/// The longest line `text` takes, in bytes.
pub const TEXT_MAX: usize = 128;
/// The largest radius and stroke `arc` takes. The glass is 360 pixels across.
pub const RADIUS_MAX: u32 = 512;
pub const WIDTH_MAX: u32 = 64;
/// How many usages one event may send. More are dropped: a handful per touch is a remote, more
/// is a face that has lost count.
pub const USAGES_MAX: usize = 4;
/// How many random bytes one event may ask for, over all its calls. Past this the face traps:
/// unlike a usage, a random byte that is quietly not delivered is a bug nobody sees.
///
/// Sixty-four is sixteen reads of the generator, which paces its reads to let noise in; a die
/// takes one or two of them.
pub const RANDOM_MAX: usize = 64;
