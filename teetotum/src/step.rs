//! Waiting for the hand that the measurement is actually made with.
//!
//! Half of what this project has established was decided by a finger: whether the knob clicks,
//! whether a note is audible, whether two clicks feel the same. A binary that walks through
//! such a comparison at its own pace is not measuring anything: it has moved on before the
//! prompt has been read.
//!
//! So a step that needs a hand waits for it, and can be repeated without reflashing. Three
//! inputs do the same thing, because the hand is in a different place depending on what is
//! being judged:
//!
//! - **the knob**, turned either way -- next step;
//! - **the screen**: a **swipe** is next, a **tap** repeats this step;
//! - **a key in `espflash monitor`**: Enter or space for next, `r` to repeat, `q` to stop
//!   waiting for good and let the binary run to its end.
//!
//! The screen learned to say "next" for a reason that was not the hand but the
//! terminal. A run that can only be stepped forward with a key has to be watched in an
//! interactive monitor, which means its output cannot be piped to a file, which means the
//! result has to be selected with the mouse out of a window that is still scrolling. With a
//! swipe, `cargo run --release --bin <name> | tee run.log` drives the whole measurement from
//! the device and leaves the log on disk.
//!
//! None of the three is required. A binary that has no touch controller of its own passes
//! `None` and keeps the other two.

use esp_hal::Blocking;
use esp_hal::i2c::master::I2c;
use esp_hal::time::{Duration, Instant};
use esp_hal::usb_serial_jtag::UsbSerialJtagRx;
use log::info;

use crate::encoder::Encoder;
use crate::touch::{Gesture, Touch};

/// How long the wait goes without saying anything.
///
/// A run that waits is indistinguishable from a run that has hung, and the difference belongs
/// in the terminal as text rather than as another beep.
const REMINDER: Duration = Duration::from_secs(15);

/// What the hand said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Go on to the next thing.
    Next,
    /// Do that again.
    Repeat,
    /// Stop asking; run the rest without pausing.
    Quit,
}

/// The three ways to say it, any of which may be absent.
pub struct Prompt<'d> {
    encoder: Option<Encoder>,
    touch: Option<Touch<'d>>,
    keys: Option<UsbSerialJtagRx<'d, Blocking>>,
    /// Set once `q` has been pressed: every later wait returns immediately.
    unattended: bool,
    /// When the current step was announced, for the reminder line.
    started: Instant,
    /// When the reminder was last printed.
    last_reminder: Instant,
    /// When the screen may be asked again; every ask costs a bus transaction.
    next_touch_read: Instant,
    /// Whether a finger was already down at the last read, so that a finger left lying on the
    /// screen is one answer and not a stream of them.
    finger_was_down: bool,
    /// Whether the screen has been seen empty since the step was announced.
    ///
    /// A finger still lying there from the answer to the *previous* step must not answer this
    /// one, and the lift that ends it must not either.
    touch_armed: bool,
    /// Whether the contact currently on the screen has reported a swipe.
    swiped: bool,
}

impl<'d> Prompt<'d> {
    /// A prompt with nothing attached, which waits for nothing.
    pub fn new() -> Self {
        Self {
            encoder: None,
            touch: None,
            keys: None,
            unattended: false,
            started: Instant::now(),
            last_reminder: Instant::now(),
            next_touch_read: Instant::now(),
            finger_was_down: false,
            touch_armed: false,
            swiped: false,
        }
    }

    /// Adds the knob.
    pub fn with_encoder(mut self, encoder: Encoder) -> Self {
        self.encoder = Some(encoder);
        self
    }

    /// Adds the touch screen.
    pub fn with_touch(mut self, touch: Touch<'d>) -> Self {
        self.touch = Some(touch);
        self
    }

    /// Adds the keyboard at the other end of `espflash monitor`.
    pub fn with_keys(mut self, keys: UsbSerialJtagRx<'d, Blocking>) -> Self {
        self.keys = Some(keys);
        self
    }

    /// Whether anything at all is attached to wait on.
    pub fn is_attended(&self) -> bool {
        !self.unattended && (self.encoder.is_some() || self.touch.is_some() || self.keys.is_some())
    }

    /// Announces what is about to happen and waits for the hand to allow it.
    ///
    /// `what` is one line in the imperative -- what the hand should be doing while the step
    /// runs, not what the step measures.
    pub fn wait(&mut self, i2c: &mut I2c<'_, Blocking>, what: &str) -> Step {
        if !self.is_attended() {
            return Step::Next;
        }
        self.announce(what);
        loop {
            if let Some(step) = self.poll(i2c) {
                return step;
            }
        }
    }

    /// Announces a step without waiting for it, for a caller that owns its own loop.
    ///
    /// Together with [`Prompt::poll`] this is [`Prompt::wait`] taken apart. A run that has
    /// something to keep alive while it waits -- a serial line that has to stay drained, so that
    /// what the other end says during the pause is not lost -- cannot hand its loop over, and
    /// this is the same waiting written so that it can borrow the loop instead of owning it.
    pub fn announce(&mut self, what: &str) {
        if !self.is_attended() {
            return;
        }
        info!("");
        info!(">>> {what}");
        // The line names only the inputs that are actually attached. A run that measures the
        // knob itself cannot also step forward with it, and telling the hand to turn it would
        // then be an instruction to walk over the measurement.
        let go_on = match (
            self.encoder.is_some(),
            self.touch.is_some(),
            self.keys.is_some(),
        ) {
            (true, _, true) => "turn the knob or press Enter",
            (true, _, false) => "turn the knob",
            (false, true, true) => "swipe the screen or press Enter",
            (false, true, false) => "swipe the screen",
            (false, false, true) => "press Enter",
            (false, false, false) => "nothing",
        };
        let repeat = match (self.touch.is_some(), self.keys.is_some()) {
            (true, true) => "tap the screen or press r",
            (true, false) => "tap the screen",
            (false, true) => "press r",
            (false, false) => "nothing",
        };
        info!(">>> {go_on} to go on, {repeat} to repeat");
        let now = Instant::now();
        self.started = now;
        self.last_reminder = now;
        self.next_touch_read = now;
        self.finger_was_down = false;
        self.touch_armed = false;
        self.swiped = false;
    }

    /// One pass over all three inputs. `None` means nothing has been said yet.
    ///
    /// Non-blocking, and meant to be called as often as the caller can manage: the knob has to
    /// be polled tightly, because its pulses are shorter than ten milliseconds and a loop that
    /// sleeps between reads walks straight past them (`src/bin/knob.rs`). The screen is asked on
    /// a slower clock of its own, because every ask costs a bus transaction.
    pub fn poll(&mut self, i2c: &mut I2c<'_, Blocking>) -> Option<Step> {
        if !self.is_attended() {
            return Some(Step::Next);
        }

        if let Some(encoder) = self.encoder.as_mut()
            && encoder.poll() != 0
        {
            return Some(Step::Next);
        }

        if let Some(keys) = self.keys.as_mut()
            && let Ok(byte) = keys.read_byte()
        {
            match byte {
                b'\r' | b'\n' | b' ' => return Some(Step::Next),
                b'r' | b'R' => return Some(Step::Repeat),
                b'q' | b'Q' => {
                    self.unattended = true;
                    info!(">>> going on without stopping again");
                    return Some(Step::Next);
                }
                _ => {}
            }
        }

        let now = Instant::now();
        if let Some(touch) = self.touch.as_mut()
            && now >= self.next_touch_read
        {
            self.next_touch_read = now + Duration::from_millis(20);
            // The interrupt line is deliberately not consulted. It was, and it lost taps: several
            // in a row went unanswered while the same screen had worked moments earlier. The
            // controller pulses that line rather than holding it, so
            // sampling it every twenty milliseconds is a coincidence, not a reading -- and
            // `src/touch.rs` established that the contact registers answer without it. One bus
            // transaction per sample is the price, and it buys a tap that always counts.
            if let Ok(report) = touch.read(i2c) {
                let finger_is_down = report.contact.is_some();

                if !self.touch_armed {
                    // Wait for the screen to be empty once. Otherwise the finger that answered
                    // the previous step answers this one too, on the way up.
                    self.touch_armed = !finger_is_down;
                    self.finger_was_down = finger_is_down;
                    self.swiped = false;
                } else {
                    if finger_is_down
                        && matches!(
                            report.gesture,
                            Gesture::SlideUp
                                | Gesture::SlideDown
                                | Gesture::SlideLeft
                                | Gesture::SlideRight
                        )
                    {
                        self.swiped = true;
                    }

                    // Answered when the finger lifts, not while it is down. The controller
                    // reports a gesture *during* the contact (`src/bin/touch.rs`), so a swipe
                    // read on the way in would be answered as a tap before it is a swipe.
                    if self.finger_was_down && !finger_is_down {
                        let step = if self.swiped {
                            Step::Next
                        } else {
                            Step::Repeat
                        };
                        self.swiped = false;
                        self.finger_was_down = false;
                        return Some(step);
                    }
                    self.finger_was_down = finger_is_down;
                }
            }
        }

        if now - self.last_reminder > REMINDER {
            self.last_reminder = now;
            info!(">>> still waiting ({} s)", (now - self.started).as_secs());
        }

        None
    }

    /// Waits, and reports whether the caller should run the step again.
    ///
    /// The shape most callers want:
    ///
    /// ```ignore
    /// loop {
    ///     play_the_thing();
    ///     if !prompt.again(&mut i2c, "feel the click") {
    ///         break;
    ///     }
    /// }
    /// ```
    pub fn again(&mut self, i2c: &mut I2c<'_, Blocking>, what: &str) -> bool {
        self.wait(i2c, what) == Step::Repeat
    }
}

impl Default for Prompt<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Stops a measurement run for good, without burning the core in an empty loop.
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
