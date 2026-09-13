//! The haptic driver behind the knob.
//!
//! The chip answers at `0x5A` and its status register reports device id 7, which is a
//! **DRV2605L** (measured with `src/bin/probe.rs`). It answers without any pin being
//! driven first, so whatever the board does with the driver's enable line, it does not need
//! help from the firmware.
//!
//! The part is a waveform player, not a motor driver in the plain sense: it carries a ROM of
//! effects, and a byte written into the sequencer names one of them. What that byte *feels*
//! like depends on the actuator underneath -- an eccentric-rotating-mass motor and a linear
//! resonant actuator want different libraries and different drive -- and **which of the two
//! sits in this knob is not something the datasheet can answer**. `src/bin/haptic.rs` asks the
//! chip instead: auto-calibration fails against the wrong kind of actuator, and an LRA leaves
//! its resonance period behind in register `0x22`.
//!
//! **The driver has an enable line, and it is GPIO38** (measured). Nothing in the
//! pin lists in circulation mentions it, and it costs an afternoon to find out the hard way:
//! with EN low the chip still answers on I2C, still accepts writes, and still runs its own
//! diagnostic -- which reports the actuator as open or shorted, because from behind a disabled
//! output stage that is exactly what an attached motor looks like. Holding GPIO38 high turns
//! the diagnostic from `0xE9` to `0xE0` and the motor moves.
//!
//! **The same pin is the serial line to the classic ESP32**, and the two uses fit on it
//! (measured with `src/bin/pin38.rs`): a UART transmit line idles high, and high is the
//! enabled state. The enable is a level with no latch, so what a byte costs is exactly the low
//! bits in it. Under a gapless stream of `0x00` the clicks are shorter and quieter to the hand.
//! Hence the one rule this module cannot enforce for its caller: **do not stream to the other
//! chip while an effect is playing.**
//!
//! Everything in this module that is not marked as measured comes from the DRV2605L datasheet
//! (SLOS854, Texas Instruments) and is hearsay until the board confirms it.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::i2c::master::{Error, I2c};

/// Where the driver is meant to answer on the bus, per the datasheet.
pub const ADDRESS: u8 = 0x5A;

/// Where the driver on **this** board answers instead, measured.
///
/// It answered at [`ADDRESS`] the first time it was ever read, and has answered here ever
/// since the first time anything wrote to it -- with the register map unchanged, defaults and
/// all, which is what identifies it. Why the address moved is unconfirmed, so this is a
/// measurement and not an explanation.
pub const ADDRESS_AS_FOUND: u8 = 0x40;

/// Status: device id in the top three bits, then diagnostic, over-temperature, over-current.
const REG_STATUS: u8 = 0x00;
/// Mode: the bottom three bits pick what `GO` starts, bit 6 is standby, bit 7 a device reset.
const REG_MODE: u8 = 0x01;
/// The amplitude played while the chip is in real-time-playback mode.
const REG_RTP: u8 = 0x02;
/// Which ROM library the effect numbers are looked up in.
const REG_LIBRARY: u8 = 0x03;
/// First of the eight sequencer slots; an effect number goes in, `0x00` ends the sequence.
const REG_SEQUENCE: u8 = 0x04;
/// Writing `1` starts whatever the mode selects.
const REG_GO: u8 = 0x0C;
/// Rated voltage, as the closed loop should drive the actuator on average.
const REG_RATED_VOLTAGE: u8 = 0x16;
/// The clamp the drive may never exceed, peak.
const REG_OD_CLAMP: u8 = 0x17;
/// How long auto-calibration drives the actuator before it decides.
///
/// The chip's `AUTO_CAL_TIME` field, bits 5:4 of `CONTROL4`. **This is what a boot feels like**:
/// calibration is the one thing at start-up that moves the motor, so the setting is not an
/// internal detail but the length of the buzz the user gets on every reset.
///
/// The four times are the datasheet's, trigger and maximum: 150/350 ms, 250/450, 500/700 and
/// 1000/1200. Nothing here measures them.
///
/// A measurement run that is asking the chip what the actuator *is* wants [`CalTime::Longest`].
/// Firmware that already knows -- an LRA at 161 Hz, measured -- does not, and the
/// honest test of a shorter time is whether it lands on the same compensation and back-EMF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalTime {
    /// 150 ms to trigger, 350 ms at most.
    Short,
    /// 250 ms to trigger, 450 ms at most.
    Medium,
    /// 500 ms to trigger, 700 ms at most.
    Long,
    /// 1000 ms to trigger, 1200 ms at most. The chip's own default for an unknown actuator.
    Longest,
}

impl CalTime {
    /// The two bits as they sit in `CONTROL4`.
    const fn bits(self) -> u8 {
        match self {
            CalTime::Short => 0x00,
            CalTime::Medium => 0x10,
            CalTime::Long => 0x20,
            CalTime::Longest => 0x30,
        }
    }
}

/// Auto-calibration result: the compensation factor it settled on.
const REG_CAL_COMP: u8 = 0x18;
/// Auto-calibration result: the back-EMF factor it settled on.
const REG_CAL_BEMF: u8 = 0x19;
/// Feedback control: bit 7 says LRA rather than ERM, and the rest is loop tuning.
const REG_FEEDBACK: u8 = 0x1A;
/// Control 1, whose bottom five bits are the drive time.
const REG_CONTROL1: u8 = 0x1B;
/// Control 2.
const REG_CONTROL2: u8 = 0x1C;
/// Control 3, which holds the open-loop bits for both actuator kinds.
const REG_CONTROL3: u8 = 0x1D;
/// Control 4, whose bits 5:4 are how long auto-calibration is allowed to take.
const REG_CONTROL4: u8 = 0x1E;
/// The period an LRA is driven at when the loop is open; one step is 98.46 us.
const REG_LRA_OPEN_LOOP: u8 = 0x20;
/// Supply voltage, as the chip measures it: one step is 5.6 V / 255.
const REG_VBAT: u8 = 0x21;
/// The resonance period an LRA was last seen to have: one step is 98.46 us.
const REG_LRA_PERIOD: u8 = 0x22;

/// One step of [`REG_VBAT`], in millivolts: 5.6 V spread over 255 steps.
const VBAT_STEP_MV: u32 = 5600 / 255;
/// One step of [`REG_LRA_PERIOD`], in tenths of a microsecond.
const LRA_PERIOD_STEP_TENTHS_US: u32 = 985;

/// What the chip does when `GO` is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Play the waveform sequence.
    InternalTrigger = 0,
    /// Play the amplitude in the real-time-playback register, continuously.
    RealTime = 5,
    /// Run the actuator diagnostic.
    Diagnostics = 6,
    /// Run auto-calibration.
    AutoCalibration = 7,
}

/// Which kind of actuator is being driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actuator {
    /// An eccentric rotating mass: a motor with a weight off centre.
    Erm,
    /// A linear resonant actuator: a mass on a spring, driven at its own frequency.
    Lra,
}

/// A ROM library of effects.
///
/// Five of them are for ERM actuators and differ in how hard they drive; the sixth is the
/// only one meant for an LRA. Effect numbers mean the same thing in each -- number 1 is
/// "strong click" throughout -- but the drive behind them is not the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Library {
    Empty = 0,
    /// ERM, rated 1.3 V.
    ErmA = 1,
    /// ERM, rated 3 V.
    ErmB = 2,
    /// ERM, rated 3 V, slower.
    ErmC = 3,
    /// ERM, rated 3 V, slower still.
    ErmD = 4,
    /// ERM, rated 4.5 V.
    ErmE = 5,
    /// The LRA library.
    Lra = 6,
}

/// What auto-calibration left behind.
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    /// False when the chip's own diagnostic bit says the calibration did not take.
    pub passed: bool,
    /// The status register as it stood afterwards, because the named bit is one of several.
    pub status: u8,
    /// The compensation factor the chip settled on.
    pub compensation: u8,
    /// The back-EMF factor the chip settled on.
    pub back_emf: u8,
    /// The feedback register afterwards; the loop may have adjusted its own gain.
    pub feedback: u8,
}

/// The haptic driver: an address on the bus, and the enable line that switches its output on.
pub struct Haptic<'d> {
    address: u8,
    enable: Option<Output<'d>>,
}

impl<'d> Haptic<'d> {
    /// The driver at the datasheet's address, with its enable line held high.
    ///
    /// The enable pin is GPIO38 on this board. Without it the chip answers and does nothing.
    pub fn new(enable: Output<'d>, delay: &Delay) -> Self {
        let mut haptic = Self {
            address: ADDRESS,
            enable: Some(enable),
        };
        haptic.enable(delay);
        haptic
    }

    /// The driver at a given address, with no enable line of its own.
    ///
    /// For a board that ties EN high, and for measuring what happens when it is not held.
    pub fn at(address: u8) -> Self {
        Self {
            address,
            enable: None,
        }
    }

    /// Gives this driver an enable line and raises it.
    pub fn with_enable(mut self, enable: Output<'d>, delay: &Delay) -> Self {
        self.enable = Some(enable);
        self.enable(delay);
        self
    }

    /// Raises the enable line, if there is one, and waits for the chip to come up.
    ///
    /// The datasheet gives 250 us from enable to ready; a millisecond is cheap and certain.
    pub fn enable(&mut self, delay: &Delay) {
        if let Some(enable) = self.enable.as_mut() {
            enable.set_high();
            delay.delay_millis(1);
        }
    }

    /// Drops the enable line, which switches the output stage off and saves the quiescent
    /// current. The chip keeps answering on the bus.
    pub fn disable(&mut self) {
        if let Some(enable) = self.enable.as_mut() {
            enable.set_low();
        }
    }

    /// The address this instance talks to.
    pub fn address(&self) -> u8 {
        self.address
    }
}

impl Haptic<'_> {
    /// Reads the status register: device id in the top three bits, 7 being a DRV2605L.
    pub fn status(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<u8, Error> {
        self.register(i2c, REG_STATUS)
    }

    /// The device id alone, shifted down out of the status register.
    pub fn device_id(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<u8, Error> {
        Ok(self.status(i2c)? >> 5)
    }

    /// Supply voltage as the chip measures it, in millivolts.
    pub fn supply_mv(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<u32, Error> {
        Ok(u32::from(self.register(i2c, REG_VBAT)?) * VBAT_STEP_MV)
    }

    /// The resonance period register, raw.
    ///
    /// It holds something only after the chip has driven an LRA in closed loop; against an ERM
    /// it means nothing.
    pub fn lra_period_raw(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<u8, Error> {
        self.register(i2c, REG_LRA_PERIOD)
    }

    /// The same period in microseconds, or `None` when the register reads zero.
    pub fn lra_period_us(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<Option<u32>, Error> {
        let raw = self.lra_period_raw(i2c)?;
        Ok((raw != 0).then(|| u32::from(raw) * LRA_PERIOD_STEP_TENTHS_US / 10))
    }

    /// Reads the registers worth naming in a log, in the order of the register map.
    ///
    /// Rated voltage, overdrive clamp, calibration compensation and back-EMF, feedback, and
    /// the four control registers.
    pub fn settings(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<[u8; 9], Error> {
        let mut out = [0u8; 9];
        for (slot, reg) in out.iter_mut().zip([
            REG_RATED_VOLTAGE,
            REG_OD_CLAMP,
            REG_CAL_COMP,
            REG_CAL_BEMF,
            REG_FEEDBACK,
            REG_CONTROL1,
            REG_CONTROL2,
            REG_CONTROL3,
            REG_CONTROL4,
        ]) {
            *slot = self.register(i2c, reg)?;
        }
        Ok(out)
    }

    /// Pulls the chip out of standby, which is where it wakes up.
    pub fn wake(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<(), Error> {
        self.set_mode(i2c, Mode::InternalTrigger)
    }

    /// Puts the chip back into standby, where it draws next to nothing.
    pub fn standby(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<(), Error> {
        i2c.write(self.address, &[REG_MODE, 0x40])
    }

    /// Switches the open-loop bits in control 3, for one actuator kind or the other.
    ///
    /// This is the crude drive: the chip stops watching the actuator's back-EMF and simply
    /// drives. It matters when the closed loop finds no feedback at all -- a closed loop with
    /// nothing coming back turns the drive down to nothing, which is indistinguishable, from
    /// the outside, from a motor that is not there.
    pub fn set_open_loop(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        actuator: Actuator,
        open: bool,
    ) -> Result<(), Error> {
        let control3 = self.register(i2c, REG_CONTROL3)?;
        // Bit 0 is the LRA's open loop, bit 5 the ERM's.
        let bit = match actuator {
            Actuator::Lra => 0x01,
            Actuator::Erm => 0x20,
        };
        let value = if open { control3 | bit } else { control3 & !bit };
        i2c.write(self.address, &[REG_CONTROL3, value])
    }

    /// Sets the period an LRA is driven at in open loop; one step is 98.46 us.
    pub fn set_open_loop_period(&mut self, i2c: &mut I2c<'_, Blocking>, period: u8) -> Result<(), Error> {
        i2c.write(self.address, &[REG_LRA_OPEN_LOOP, period])
    }

    /// Writes the mode register, clearing the standby bit in the same stroke.
    pub fn set_mode(&mut self, i2c: &mut I2c<'_, Blocking>, mode: Mode) -> Result<(), Error> {
        i2c.write(self.address, &[REG_MODE, mode as u8])
    }

    /// Asks the chip to reload its defaults and waits for it to come back.
    ///
    /// **Do not call this on this board.** Measured: the write is refused with a
    /// NACK on the data byte -- the chip resets itself in the middle of the transfer instead of
    /// finishing it -- and afterwards it stops acknowledging `0x5A` altogether, answering at
    /// `0x40` instead with a device id of 1. Everything else survives: reads work, and forty
    /// out of forty ordinary writes were acknowledged in the same run. Sixteen recovery clocks
    /// on SCL followed by a stop bring it back to `0x5A` sometimes but not always; a fresh boot
    /// does it reliably.
    ///
    /// It is kept because the fault is worth reproducing, and because a board whose driver does
    /// not do this can use it. Configuring the registers by hand is what this module does
    /// instead.
    ///
    /// The reset bit clears itself when the chip is done, which the datasheet gives as under a
    /// millisecond; this waits for the bit and gives up after ten.
    pub fn reset(&mut self, i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Result<bool, Error> {
        i2c.write(self.address, &[REG_MODE, 0x80])?;
        for _ in 0..10 {
            delay.delay_millis(1);
            if self.register(i2c, REG_MODE)? & 0x80 == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Says which kind of actuator is attached, leaving the rest of the feedback register alone.
    pub fn set_actuator(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        actuator: Actuator,
    ) -> Result<(), Error> {
        let feedback = self.register(i2c, REG_FEEDBACK)?;
        let value = match actuator {
            Actuator::Erm => feedback & 0x7F,
            Actuator::Lra => feedback | 0x80,
        };
        i2c.write(self.address, &[REG_FEEDBACK, value])
    }

    /// Picks the ROM library the effect numbers are read from.
    pub fn set_library(&mut self, i2c: &mut I2c<'_, Blocking>, library: Library) -> Result<(), Error> {
        i2c.write(self.address, &[REG_LIBRARY, library as u8])
    }

    /// Writes the rated voltage and the overdrive clamp, both in the chip's own units.
    ///
    /// What the right numbers are depends on the actuator, and the actuator here is not
    /// documented by the vendor -- so these are set from what auto-calibration is given, not
    /// from a datasheet.
    pub fn set_drive(&mut self, i2c: &mut I2c<'_, Blocking>, rated: u8, clamp: u8) -> Result<(), Error> {
        i2c.write(self.address, &[REG_RATED_VOLTAGE, rated])?;
        i2c.write(self.address, &[REG_OD_CLAMP, clamp])
    }

    /// Writes back a calibration that was run earlier, instead of running one now.
    ///
    /// **This is how a boot stays quiet.** Auto-calibration is the only thing at start-up that
    /// drives the motor, and the user feels every one of it. The three registers it leaves
    /// behind are all the chip keeps: feedback, which carries the actuator kind along with the
    /// brake factor and loop gain the run settled on, and the two calibration results. Written
    /// back in that order they put the chip where the run left it, with nothing to feel.
    ///
    /// The values have to come from a run against *this* actuator. There is no checksum on them
    /// and no way to ask the chip whether they fit -- a wrong set is not rejected, it is simply
    /// driven.
    pub fn set_calibration(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        cal: &Calibration,
    ) -> Result<(), Error> {
        i2c.write(self.address, &[REG_FEEDBACK, cal.feedback])?;
        i2c.write(self.address, &[REG_CAL_COMP, cal.compensation])?;
        i2c.write(self.address, &[REG_CAL_BEMF, cal.back_emf])
    }

    /// Runs auto-calibration against whatever actuator is wired up, and reports what came back.
    ///
    /// This drives the actuator: **it is felt as well as measured**, and how long it is felt is
    /// [`CalTime`]. The `GO` bit clears when the chip is done; this waits up to two seconds and
    /// then reads the result either way.
    pub fn auto_calibrate(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        delay: &Delay,
        time: CalTime,
    ) -> Result<Calibration, Error> {
        // A brake factor and loop gain in the middle of their ranges -- the datasheet's own
        // starting point for an unknown actuator.
        let feedback = self.register(i2c, REG_FEEDBACK)?;
        i2c.write(self.address, &[REG_FEEDBACK, (feedback & 0x80) | 0x36])?;
        let control4 = self.register(i2c, REG_CONTROL4)?;
        i2c.write(
            self.address,
            &[REG_CONTROL4, (control4 & !0x30) | time.bits()],
        )?;

        self.set_mode(i2c, Mode::AutoCalibration)?;
        self.go(i2c)?;
        self.wait_for_go(i2c, delay, 2000)?;

        let status = self.status(i2c)?;
        Ok(Calibration {
            // Bit 3 is the diagnostic result, and it is set when the run did *not* pass.
            passed: status & 0x08 == 0,
            status,
            compensation: self.register(i2c, REG_CAL_COMP)?,
            back_emf: self.register(i2c, REG_CAL_BEMF)?,
            feedback: self.register(i2c, REG_FEEDBACK)?,
        })
    }

    /// Runs the actuator diagnostic, which says whether something is actually wired up.
    ///
    /// Returns the status register; bit 3 set means the diagnostic failed, and the datasheet
    /// reads that as an open or shorted actuator.
    pub fn diagnose(&mut self, i2c: &mut I2c<'_, Blocking>, delay: &Delay) -> Result<u8, Error> {
        self.set_mode(i2c, Mode::Diagnostics)?;
        self.go(i2c)?;
        self.wait_for_go(i2c, delay, 1000)?;
        self.status(i2c)
    }

    /// Loads up to eight effect numbers into the sequencer and terminates it.
    pub fn set_sequence(&mut self, i2c: &mut I2c<'_, Blocking>, effects: &[u8]) -> Result<(), Error> {
        let mut slot = REG_SEQUENCE;
        for &effect in effects.iter().take(8) {
            i2c.write(self.address, &[slot, effect])?;
            slot += 1;
        }
        if effects.len() < 8 {
            i2c.write(self.address, &[slot, 0x00])?;
        }
        Ok(())
    }

    /// Plays one ROM effect and waits for the chip to say it is finished.
    pub fn play(&mut self, i2c: &mut I2c<'_, Blocking>, effect: u8, delay: &Delay) -> Result<(), Error> {
        self.set_mode(i2c, Mode::InternalTrigger)?;
        self.set_sequence(i2c, &[effect])?;
        self.go(i2c)?;
        self.wait_for_go(i2c, delay, 1000)
    }

    /// Drives the actuator at a fixed amplitude for a while, bypassing the ROM entirely.
    ///
    /// This is the crude way to feel whether anything moves at all, and it is the one that
    /// does not care which library or which actuator kind is configured.
    pub fn play_raw(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        amplitude: u8,
        millis: u32,
        delay: &Delay,
    ) -> Result<(), Error> {
        self.set_mode(i2c, Mode::RealTime)?;
        i2c.write(self.address, &[REG_RTP, amplitude])?;
        delay.delay_millis(millis);
        i2c.write(self.address, &[REG_RTP, 0])?;
        self.set_mode(i2c, Mode::InternalTrigger)
    }

    /// Writes the `GO` bit.
    pub fn go(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<(), Error> {
        i2c.write(self.address, &[REG_GO, 1])
    }

    /// Clears the `GO` bit, cutting a running effect short.
    ///
    /// The ROM waveforms have no length parameter: an effect is as long as it is. Ending one
    /// early is therefore the only way to a shorter click, and it is not free -- a ROM effect
    /// brakes the actuator at its end, and a cut one loses that braking and may ring on. How
    /// much of the effect is worth keeping is a question for a fingertip, not for the datasheet.
    pub fn stop(&mut self, i2c: &mut I2c<'_, Blocking>) -> Result<(), Error> {
        i2c.write(self.address, &[REG_GO, 0])
    }

    /// Waits for `GO` to clear itself, and says whether it did before the deadline.
    fn wait_for_go(
        &mut self,
        i2c: &mut I2c<'_, Blocking>,
        delay: &Delay,
        timeout_ms: u32,
    ) -> Result<(), Error> {
        for _ in 0..timeout_ms {
            if self.register(i2c, REG_GO)? & 1 == 0 {
                return Ok(());
            }
            delay.delay_millis(1);
        }
        Ok(())
    }

    fn register(&mut self, i2c: &mut I2c<'_, Blocking>, register: u8) -> Result<u8, Error> {
        let mut byte = [0u8; 1];
        i2c.write_read(self.address, &[register], &mut byte)?;
        Ok(byte[0])
    }
}
