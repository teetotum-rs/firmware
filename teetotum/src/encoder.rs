//! The knob, which turns out not to be a quadrature encoder.
//!
//! Two pins reach the ESP32-S3 from the knob, and the obvious reading of them -- channel A and
//! channel B of a rotary encoder, a quarter cycle apart -- is wrong. Measured over 134 steps,
//! the two lines are never low at the same time, and each direction of turn
//! pulses exactly one of them: one pin low and back for a step one way, the other pin for a
//! step the other way. Whatever turns the encoder's phases into direction does it before these
//! pins.
//!
//! That makes the driver simpler than a quadrature decoder, not harder. A falling edge is a
//! step, its pin is the direction, and the only care needed is against contact bounce.
//!
//! **GPIO8 pulses for a turn clockwise**, GPIO7 for anticlockwise. How many
//! steps a turn makes depends on who counts: see [`PULSES_PER_REVOLUTION`].
//!
//! # An edge is latched, not looked for
//!
//! This module used to sample the two pins whenever it was asked to, which only sees every
//! pulse if the caller runs often enough. **The firmware's loop runs about twenty-five times a
//! second**, because it waits ten milliseconds and then redraws the glass -- and it redraws
//! precisely because the knob moved. At a hand's speed detents are about forty milliseconds
//! apart and a pulse stays low for fifteen to thirty, so a single slow pass through the loop
//! steps straight over one.
//!
//! **A polled input is only ever as good as the slowest pass of the loop that polls it**, and
//! that loop gets slower for reasons which have nothing to do with the knob. So the edge is
//! caught by the GPIO interrupt and added to a counter; [`Encoder::poll`] collects what the
//! counter holds and never looks at a pin. A pass through the loop may now take as long as it
//! likes.
//!
//! Checked against the second encoder on the same shaft, which reports every detent it sees:
//! polling counted six detents to its twelve on a brisk turn; latching against the interrupt,
//! seven turns of fast and slow gave 93 detents here against 91 there, the difference falling on
//! both sides of nought rather than always the same way -- a detent counted here whose frame
//! arrives from over there after the report was printed lands in the next one.
//!
//! # What this takes from the rest of the chip
//!
//! The ESP32-S3 has **one** GPIO interrupt for all of its pins, so an [`Encoder`] takes the
//! handler for the whole port -- that is why it asks for [`Io`] rather than reaching for it
//! quietly. esp-hal's own async pin API keeps working alongside it; what does not is a second
//! user handler. Dropping the encoder gives the pins and the knob back.

use core::cell::RefCell;
use core::sync::atomic::{AtomicI32, Ordering};

use critical_section::Mutex;
use esp_hal::gpio::{Event, Input, Io};
use esp_hal::handler;
use esp_hal::time::{Duration, Instant};

/// How long a line is ignored after an edge has been taken from it.
///
/// The measured pulses are 15 milliseconds low at their shortest, and across 134 of them only
/// two transitions landed within the same millisecond. Five milliseconds is well clear of the
/// bounce and well under the shortest real pulse.
const DEBOUNCE: Duration = Duration::from_millis(5);

/// Steps this driver counts in one full revolution, turned slowly.
///
/// **It is not one number.** Marked revolutions counted 203, 200 and 207 turned slowly and 190
/// and 187 turned fast, so 37 to 41 a revolution, slow at the top; 40 is the slow end. The menu
/// ring used to run by this count and came out early or late by that spread; it now counts
/// detents directly, and no part of the firmware depends on this number.
///
/// The polled driver this one replaced counted a clean 30: ten marked revolutions gave 300 with
/// no remainder. Why the two differ, and which of them counts the true detents, is open;
/// restarting the debounce window on every edge does not change either count.
pub const PULSES_PER_REVOLUTION: i32 = 40;

/// The knob's two direction lines, once they belong to the interrupt.
struct Lines {
    /// Pulses low for a step in the direction that counts up.
    up: Input<'static>,
    /// Pulses low for a step in the direction that counts down.
    down: Input<'static>,
    up_settles: Instant,
    down_settles: Instant,
}

/// The lines, reachable from the interrupt handler and from nowhere else.
///
/// There is one knob and one GPIO interrupt, so there is one of these.
static LINES: Mutex<RefCell<Option<Lines>>> = Mutex::new(RefCell::new(None));

/// Detents counted by the handler and not yet collected.
static STEPS: AtomicI32 = AtomicI32::new(0);

/// The knob.
pub struct Encoder {
    position: i32,
}

impl Encoder {
    /// Takes the two direction lines, both of which rest high, and the GPIO interrupt with them.
    ///
    /// Which physical direction counts up is a matter of wiring, not of truth: swap the two
    /// arguments to turn the knob the other way round.
    ///
    /// Only one encoder exists at a time. A second one built while the first is alive takes the
    /// lines over, and the first then counts nothing -- which is the honest outcome, because
    /// there is only one knob to count.
    pub fn new(io: &mut Io<'_>, mut up: Input<'static>, mut down: Input<'static>) -> Self {
        io.set_interrupt_handler(edge);

        critical_section::with(|cs| {
            up.listen(Event::AnyEdge);
            down.listen(Event::AnyEdge);
            // Whatever the pins did before anyone was listening is not a turn of the knob.
            up.clear_interrupt();
            down.clear_interrupt();
            STEPS.store(0, Ordering::Relaxed);

            let now = Instant::now();
            LINES.borrow_ref_mut(cs).replace(Lines {
                up,
                down,
                up_settles: now,
                down_settles: now,
            });
        });

        Self { position: 0 }
    }

    /// Returns the steps counted since the last call.
    ///
    /// Positive is the direction of the pin passed as `up`. This reads a counter rather than a
    /// pin: a call that comes late collects everything that happened while it was away, and a
    /// call that sees nothing returns zero.
    pub fn poll(&mut self) -> i32 {
        let steps = STEPS.swap(0, Ordering::Relaxed);
        self.position += steps;
        steps
    }

    /// Steps counted since the encoder was created, or since [`Self::reset`].
    pub fn position(&self) -> i32 {
        self.position
    }

    /// Sets the position back to zero without disturbing the edge tracking.
    pub fn reset(&mut self) {
        self.position = 0;
    }
}

impl Drop for Encoder {
    /// Gives the knob back: the lines stop listening and the pins go with them.
    fn drop(&mut self) {
        critical_section::with(|cs| {
            if let Some(mut lines) = LINES.borrow_ref_mut(cs).take() {
                lines.up.unlisten();
                lines.down.unlisten();
            }
        });
    }
}

/// Counts a detent for every edge that finds its line low.
///
/// **An accepted edge on a low line is a step**, whichever edge it was. A fall is the obvious
/// case; a line that is low at an edge which is not a fall has fallen since the last edge this
/// handler accepted, and the pulse in between was too short or too bouncy to be seen whole.
/// Either way the knob moved a detent, and that is the thing being counted.
///
/// Edges arriving inside a line's settling window are cleared and dropped. That is the bounce
/// filter, and it costs nothing at the hand speeds measured: detents are about 40 ms apart.
#[handler]
fn edge() {
    let now = Instant::now();
    let mut steps = 0;

    critical_section::with(|cs| {
        let mut lines = LINES.borrow_ref_mut(cs);
        let Some(lines) = lines.as_mut() else {
            return;
        };

        if lines.up.is_interrupt_set() {
            lines.up.clear_interrupt();
            if now >= lines.up_settles {
                lines.up_settles = now + DEBOUNCE;
                if lines.up.is_low() {
                    steps += 1;
                }
            }
        }
        if lines.down.is_interrupt_set() {
            lines.down.clear_interrupt();
            if now >= lines.down_settles {
                lines.down_settles = now + DEBOUNCE;
                if lines.down.is_low() {
                    steps -= 1;
                }
            }
        }
    });

    if steps != 0 {
        STEPS.fetch_add(steps, Ordering::Relaxed);
    }
}
