//! The touch controller on the glass.
//!
//! The chip answers at `0x15` and reports `0xB6` on its identity register, which is a
//! **CST816D** -- the factory image calls `esp_lcd_touch_new_i2c_cst816s`, but the silicon on
//! this board is the D variant (measured with `src/bin/probe.rs`). The register map is
//! the same across the family, which is why the mismatch never troubled the demo.
//!
//! Reading a contact is one six-byte burst from register `0x01`: a gesture code, the number of
//! fingers, then X and Y as twelve bits each. The two high nibbles carry more than coordinate
//! bits -- the top two bits of the X high byte say whether the finger arrived, stayed or left.
//!
//! What the controller reports is **its own** coordinate system, which need not agree with the
//! way the panel is mounted; `src/bin/touch.rs` is the measurement that settles the relation
//! between the two.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, Output};
use esp_hal::i2c::master::I2c;

use crate::framebuffer::{HEIGHT, WIDTH};

/// Where the controller answers on the bus.
pub const ADDRESS: u8 = 0x15;

/// First of the six contact registers: gesture, finger count, X high, X low, Y high, Y low.
const REG_STATUS: u8 = 0x01;
/// Identity register: `0xB4` a CST816T, `0xB5` a CST816S, `0xB6` the CST816D on this board.
const REG_CHIP_ID: u8 = 0xA7;
/// Firmware version, for the record.
const REG_FIRMWARE: u8 = 0xA9;
/// Which motions the controller is allowed to recognise on its own.
///
/// Three bits, per the CST816S datasheet: continuous left-right sliding, continuous up-down
/// sliding, double click. They rest at `0x00` on this board -- and **slides are reported
/// anyway**, measured by holding the register at zero for thirty seconds of
/// continuous swiping and then writing `0x07` with the hand still going: the gestures arrived
/// throughout, in both stretches, indistinguishably. Whether the double click needs the bit is
/// untested; it has only ever been seen with the mask set.
const REG_MOTION_MASK: u8 = 0xEC;
/// Which events pull the interrupt line low: a touch, a change, a motion.
const REG_IRQ_CONTROL: u8 = 0xFA;

/// What the finger did, from the top two bits of the X high byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The finger arrived.
    Down,
    /// The finger left; the coordinates are where it was last seen.
    Up,
    /// The finger is still on the glass.
    Contact,
}

/// A gesture the controller recognised on its own.
///
/// **The directions are the controller's, not the picture's.** They are named in the same
/// frame as the coordinates, which on this board is the frame the panel is mounted in -- half
/// a turn from what the viewer sees. A slide the user makes from the left of the picture to
/// the right arrives here as [`Gesture::SlideLeft`], and anything that acts on a gesture has
/// to turn it the same way it turns the coordinates.
///
/// Measured against the picture (`src/bin/touch.rs`), which is worth saying
/// because the CST816 datasheets in circulation disagree with each other about `0x01` and
/// `0x02`: a swipe down the picture reports `0x01` and a swipe up reports `0x02`, so in the
/// mounting frame `0x01` is up and `0x02` is down. `0x05` is a tap and `0x0B` a double tap.
/// `0x0C` for a long press is the one name still taken on trust; it has not been seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    None,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
    SingleTap,
    DoubleTap,
    /// Listed by the datasheet, never yet seen on this board.
    LongPress,
    /// A code the datasheet does not list.
    Unknown(u8),
}

impl Gesture {
    /// The same slide as the viewer made it, rather than as the controller names it.
    ///
    /// The controller reports in the frame the panel is mounted in, which on this board is half
    /// a turn from what the viewer sees, so every direction is its own opposite -- see
    /// [`Contact::in_view`]. A tap is a tap either way round.
    pub fn in_picture_mount(self) -> Self {
        match self {
            Self::SlideUp => Self::SlideDown,
            Self::SlideDown => Self::SlideUp,
            Self::SlideLeft => Self::SlideRight,
            Self::SlideRight => Self::SlideLeft,
            other => other,
        }
    }

    /// The same slide as the *picture* sees it, when the picture is being shown turned
    /// `quarters` quarter turns clockwise.
    ///
    /// The slide has to come the other way round than the picture went: the picture's own north
    /// mark points to the viewer's right once the picture is a quarter turn clockwise, so a
    /// finger sliding to the viewer's right is sliding up the picture. The picture turns in
    /// whole quarters, so every slide keeps a name.
    ///
    /// Nothing happens to a tap, and nothing happens at all when `quarters` is 0.
    pub fn in_picture(self, quarters: usize) -> Self {
        // Clockwise, so that stepping back through it is subtraction.
        const ROUND: [Gesture; 4] = [
            Gesture::SlideUp,
            Gesture::SlideRight,
            Gesture::SlideDown,
            Gesture::SlideLeft,
        ];
        let Some(at) = ROUND.iter().position(|&slide| slide == self) else {
            return self;
        };
        ROUND[(at + 4 - quarters % 4) % 4]
    }

    fn from_code(code: u8) -> Self {
        match code {
            0x00 => Self::None,
            0x01 => Self::SlideUp,
            0x02 => Self::SlideDown,
            0x03 => Self::SlideLeft,
            0x04 => Self::SlideRight,
            0x05 => Self::SingleTap,
            0x0B => Self::DoubleTap,
            0x0C => Self::LongPress,
            other => Self::Unknown(other),
        }
    }
}

/// One finger on the glass, in the controller's own coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Contact {
    pub x: u16,
    pub y: u16,
    pub event: Event,
}

impl Contact {
    /// Where the finger is in the viewer's coordinates -- the ones the picture is drawn in
    /// while it stands upright.
    ///
    /// The controller reports in the frame the panel is **mounted** in, and this board mounts
    /// the glass upside down: `x' = 359 - x`, `y' = 359 - y`, measured by
    /// drawing both candidates and looking at which one is under the fingertip
    /// (`src/bin/touch.rs`). It is the same half turn that
    /// [`PANEL_MOUNT_MADCTL`](crate::panel::PANEL_MOUNT_MADCTL) applies to the pixels, and a
    /// board that fits the panel the other way up corrects both together.
    ///
    /// This is only half the way to the picture. A picture shown turned needs the turn undone
    /// as well, which is [`Screen::picture_point`](crate::screen::Screen::picture_point).
    pub fn in_view(self) -> (i32, i32) {
        (
            WIDTH as i32 - 1 - i32::from(self.x),
            HEIGHT as i32 - 1 - i32::from(self.y),
        )
    }
}

/// What one read of the contact registers found.
///
/// The gesture is kept apart from the finger on purpose. **The controller names a slide as the
/// finger leaves**, in the same read that reports no finger at all, so a driver that returns
/// early when the finger count is zero throws every gesture away -- which is exactly what an
/// earlier version of this module did, and why four deliberate swipes came back as
/// [`Gesture::None`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub gesture: Gesture,
    /// The gesture byte as it came off the bus, because the names above are hearsay.
    pub gesture_code: u8,
    /// The finger, if one is on the glass.
    pub contact: Option<Contact>,
}

/// The touch controller, with the two pins that belong to it alone.
pub struct Touch<'d> {
    reset: Output<'d>,
    interrupt: Input<'d>,
}

impl<'d> Touch<'d> {
    /// Takes the reset and interrupt lines and pulses reset before use.
    ///
    /// The delays are the generous ones the vendor drivers use: ten milliseconds low, fifty to
    /// settle afterwards. Pulsing costs sixty milliseconds at start-up and puts the controller
    /// into a known state after a warm boot, which is why it is the default here even though
    /// [`Self::attached`] shows the chip answers without it.
    pub fn new(mut reset: Output<'d>, interrupt: Input<'d>, delay: &Delay) -> Self {
        reset.set_low();
        delay.delay_millis(10);
        reset.set_high();
        delay.delay_millis(50);
        Self { reset, interrupt }
    }

    /// Takes the same two lines but leaves reset alone, holding it at whatever level it has.
    ///
    /// Measured: the controller answers its identity and contact registers with
    /// the pin never pulsed at all, so **the reset is not needed to talk to it**. That is worth
    /// having for anything that must not spend the sixty milliseconds, and for the day GPIO10
    /// turns out to be something other than the touch reset -- that row of the pin table is
    /// hearsay from a third party's ESPHome configuration and has never been measured.
    pub fn attached(reset: Output<'d>, interrupt: Input<'d>) -> Self {
        Self { reset, interrupt }
    }

    /// Whether the controller is currently asserting its interrupt line.
    ///
    /// The line is active low and rests high. Reading a contact does not require it -- polling
    /// the registers works on its own -- but it says when a read is worth doing.
    pub fn is_asserted(&self) -> bool {
        self.interrupt.is_low()
    }

    /// Pulses reset again, for a controller that has stopped answering.
    pub fn reset(&mut self, delay: &Delay) {
        self.reset.set_low();
        delay.delay_millis(10);
        self.reset.set_high();
        delay.delay_millis(50);
    }

    /// Reads the identity register: `0xB6` is the CST816D on this board.
    pub fn chip_id(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
    ) -> Result<u8, esp_hal::i2c::master::Error> {
        self.register(i2c, REG_CHIP_ID)
    }

    /// Reads the firmware version register.
    pub fn firmware(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
    ) -> Result<u8, esp_hal::i2c::master::Error> {
        self.register(i2c, REG_FIRMWARE)
    }

    /// Sets the two registers that are meant to govern gesture reporting.
    ///
    /// The values are the datasheet's: all three motion bits, and an interrupt on touch, change
    /// and motion. Neither is needed for slides -- see [`REG_MOTION_MASK`], and note that the
    /// interrupt register already rests at `0x70` on this board, so only the bottom bit changes
    /// here. Worth calling for the double click, which has not been seen without it.
    pub fn enable_gestures(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
    ) -> Result<(), esp_hal::i2c::master::Error> {
        i2c.write(ADDRESS, &[REG_MOTION_MASK, 0x07])?;
        i2c.write(ADDRESS, &[REG_IRQ_CONTROL, 0x71])?;
        Ok(())
    }

    /// Writes the motion mask by hand, for measuring what it actually does.
    pub fn set_motion_mask(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        mask: u8,
    ) -> Result<(), esp_hal::i2c::master::Error> {
        i2c.write(ADDRESS, &[REG_MOTION_MASK, mask])
    }

    /// Reads back the two registers [`Self::enable_gestures`] writes, in that order.
    ///
    /// Worth having because a write that the chip quietly ignores looks exactly like a chip
    /// that has no gestures to offer.
    pub fn gesture_registers(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
    ) -> Result<(u8, u8), esp_hal::i2c::master::Error> {
        Ok((
            self.register(i2c, REG_MOTION_MASK)?,
            self.register(i2c, REG_IRQ_CONTROL)?,
        ))
    }

    /// Reads the six contact registers in one burst.
    ///
    /// The finger comes back as `None` when none is on the glass; the gesture comes back
    /// either way, because that is when the controller reports it.
    pub fn read(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
    ) -> Result<Report, esp_hal::i2c::master::Error> {
        let mut status = [0u8; 6];
        i2c.write_read(ADDRESS, &[REG_STATUS], &mut status)?;
        let [gesture_code, fingers, x_high, x_low, y_high, y_low] = status;

        // Twelve bits each: the low nibble of the high byte, then the whole low byte.
        let x = u16::from(x_high & 0x0F) << 8 | u16::from(x_low);
        let y = u16::from(y_high & 0x0F) << 8 | u16::from(y_low);

        let event = match x_high >> 6 {
            0 => Event::Down,
            1 => Event::Up,
            _ => Event::Contact,
        };

        Ok(Report {
            gesture: Gesture::from_code(gesture_code),
            gesture_code,
            contact: (fingers > 0).then_some(Contact { x, y, event }),
        })
    }

    fn register(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        register: u8,
    ) -> Result<u8, esp_hal::i2c::master::Error> {
        let mut byte = [0u8; 1];
        i2c.write_read(ADDRESS, &[register], &mut byte)?;
        Ok(byte[0])
    }
}

/// One answer per contact, given when the finger lifts.
///
/// The controller reports a gesture **while the finger is still down** (measured in
/// `src/bin/touch.rs`), and it goes on reporting it for as long as the contact lasts. A loop
/// that acts on whatever the last read said therefore acts several times on one tap -- how many
/// depends on how long the loop's own work takes, which makes it look like an intermittent
/// glass rather than a counting error. `src/bin/jpegshow.rs` hit exactly this: one tap stepped
/// its own scaler selection twice, because the same tap was read as a live gesture on every
/// pass while the finger sat there.
///
/// `src/step.rs` has the rule already, folded into its own loop. This is the same rule with
/// nothing around it: feed every read in, and take the one answer it gives back.
#[derive(Default)]
pub struct Taps {
    /// Whether a finger was down at the previous read.
    was_down: bool,
    /// The gesture seen during the contact that is still in progress.
    seen: Option<Gesture>,
    /// Where the contact in progress began, and when, if the caller keeps time.
    first: Option<Contact>,
    since: Option<u64>,
    /// Where the finger was last seen, which is where a tap happened: the read that ends a
    /// contact carries no finger any more.
    last: Option<Contact>,
    /// Whether the contact in progress has strayed too far to be a long press.
    wandered: bool,
    /// Whether the contact in progress has been answered as a long press already.
    held: bool,
}

/// How long a finger has to rest on the glass to be a long press, in milliseconds.
///
/// **Timed here, because the controller does not say.** `0x0C` is the datasheet's code for a long
/// press and has never been seen on this board. 600 ms is a little over the half second phones
/// use, so that a slow tap stays a tap.
pub const HOLD_MS: u64 = 600;

/// How far a resting finger may drift, in pixels either way, and still be resting.
const HOLD_SLOP: i32 = 16;

/// How far a finger has to travel along one axis, in pixels, for a contact the controller left
/// unnamed to count as a slide rather than a tap.
///
/// **The controller does not name every slide.** Measured with the HID face: of 26
/// contacts made while wiping, 10 came back without a gesture and so as taps -- the
/// player stopped and started where it should have skipped. A tap drifts a few pixels; a tenth
/// of the glass is far outside that and well inside any wipe.
const SLIDE_MIN: i32 = 36;

/// The slide a contact from `first` to `last` made, in the controller's own frame, if it went
/// far enough to be one.
///
/// The frame is the controller's because the codes it names are: a wipe that runs left to right
/// in the picture is `0x03` and lowers `x`, both half a turn from what the viewer sees, so the
/// answer goes through [`Gesture::in_picture_mount`] like any named one.
fn slide_between(first: Contact, last: Contact) -> Option<Gesture> {
    let dx = i32::from(last.x) - i32::from(first.x);
    let dy = i32::from(last.y) - i32::from(first.y);
    if dx.abs().max(dy.abs()) < SLIDE_MIN {
        return None;
    }
    let slide = if dx.abs() >= dy.abs() {
        if dx < 0 {
            Gesture::SlideLeft
        } else {
            Gesture::SlideRight
        }
    } else if dy < 0 {
        Gesture::SlideUp
    } else {
        Gesture::SlideDown
    };
    log::info!("Touch: unnamed contact moved {dx:+}, {dy:+} px, taken as {slide:?}");
    Some(slide)
}

/// What one contact came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// A contact that ended without a gesture, at the place the finger was last seen.
    Tap(Contact),
    /// A finger that rested for [`HOLD_MS`] without sliding. Answered while it is still down --
    /// the hand wants to know it has been understood before it lets go -- and the lift that
    /// follows answers nothing.
    Hold(Contact),
    /// A gesture the controller named during the contact, answered on the lift.
    Gesture(Gesture),
}

impl Taps {
    /// Nothing touched yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one reading in, with the time in milliseconds; get one [`Press`] back per contact.
    ///
    /// The same rule as [`Self::feed`], and two things more: **where** a tap happened, which a
    /// menu needs to know what was tapped, and the **long press**, which needs a clock. The
    /// clock is the caller's, so this works with whichever one the caller runs on.
    ///
    /// The long press is what leads home, from anywhere -- see `teetotum::menu`.
    pub fn press(&mut self, report: &Report, now_ms: u64) -> Option<Press> {
        self.step(report, Some(now_ms))
    }

    fn step(&mut self, report: &Report, now_ms: Option<u64>) -> Option<Press> {
        if let Some(contact) = report.contact {
            if !self.was_down {
                self.first = Some(contact);
                self.since = now_ms;
                self.wandered = false;
                self.held = false;
            }
            self.was_down = true;
            self.last = Some(contact);
            if report.gesture != Gesture::None {
                self.seen = Some(report.gesture);
            }
            if let Some(first) = self.first {
                let drift = (i32::from(contact.x) - i32::from(first.x))
                    .abs()
                    .max((i32::from(contact.y) - i32::from(first.y)).abs());
                self.wandered |= drift > HOLD_SLOP;
            }
            let slid = matches!(
                self.seen,
                Some(
                    Gesture::SlideUp
                        | Gesture::SlideDown
                        | Gesture::SlideLeft
                        | Gesture::SlideRight
                )
            );
            if !self.held
                && !self.wandered
                && !slid
                && let (Some(now), Some(since)) = (now_ms, self.since)
                && now.saturating_sub(since) >= HOLD_MS
            {
                self.held = true;
                return Some(Press::Hold(contact));
            }
            return None;
        }

        // No finger, so this is either a lift or the quiet after one.
        if !core::mem::take(&mut self.was_down) {
            return None;
        }
        let seen = self.seen.take();
        let last = self.last.take();
        if core::mem::take(&mut self.held) {
            return None;
        }
        match seen {
            // Unnamed is not the same as still: a finger that travelled slid, named or not.
            None | Some(Gesture::SingleTap) => {
                match self
                    .first
                    .zip(last)
                    .and_then(|(first, last)| slide_between(first, last))
                {
                    Some(slide) => Some(Press::Gesture(slide)),
                    None => last.map(Press::Tap),
                }
            }
            Some(gesture) => Some(Press::Gesture(gesture)),
        }
    }

    /// Feed one reading in; get one gesture back per contact, on the lift that ends it.
    ///
    /// A gesture arriving during the contact is remembered rather than answered, because a
    /// swipe reads as a tap on the way in: the finger is down and the swipe has not happened
    /// yet. The last gesture seen during the contact is the one returned.
    ///
    /// **A contact that ends without any gesture is a [`Gesture::SingleTap`]**, and that is not
    /// a convenience -- it is what the controller does. A swipe is named while the finger is
    /// still down, but a tap is not: by the time `0x05` appears the
    /// contact is already gone, so a lift with nothing remembered is the only shape a tap has --
    /// unless the finger travelled [`SLIDE_MIN`] or more, which makes it a slide the controller
    /// failed to name.
    ///
    /// `src/bin/companion.rs` keeps its own copy of this rule; any other caller with its own
    /// read loop needs the same rule and not a shortcut that returns early on an empty contact.
    ///
    /// The gesture keeps being reported for a few reads *after* the lift as well. Those are the
    /// echo of the one already answered and are dropped: a tap that fires twice looks like a
    /// player that will not stay paused.
    pub fn feed(&mut self, report: &Report) -> Option<Gesture> {
        // Without a clock there is no long press, so every contact ends in a tap or a gesture.
        self.step(report, None).map(|press| match press {
            Press::Gesture(gesture) => gesture,
            Press::Tap(_) | Press::Hold(_) => Gesture::SingleTap,
        })
    }
}
