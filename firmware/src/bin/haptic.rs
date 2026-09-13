//! What the knob can be made to feel like.
//!
//! The DRV2605L answers on the bus (`src/bin/probe.rs`), but answering is not moving. Two
//! things are unknown before this runs, and neither can be looked up: **whether the actuator
//! is an ERM or an LRA**, and what the chip should be told to drive it with. Waveshare
//! publishes no schematic for the knob, and the factory image only shows that its own driver
//! is initialised, not what it is initialised for.
//!
//! So the chip is asked instead. Auto-calibration drives the actuator, watches it come back,
//! and sets a diagnostic bit when what it saw does not fit what it was told to expect. Running
//! it once as an ERM and once as an LRA is therefore a question the hardware answers:
//!
//! 1. reset, and read what the chip holds out of reset -- supply voltage included, because
//!    the drive limits mean nothing without it;
//! 2. run the built-in actuator diagnostic, which says whether anything is wired up at all;
//! 3. auto-calibrate as an **ERM** and keep the result;
//! 4. reset, auto-calibrate as an **LRA** and keep that too, along with the resonance period
//!    register, which only an LRA leaves behind;
//! 5. then play, over and over, a short round of effects with the configuration that
//!    calibrated -- announced one by one in the log, so that a hand on the knob can be matched
//!    against a name.
//!
//! Reading it: the calibration that passes names the actuator. If both pass, the resonance
//! period decides -- an LRA reports a plausible one, in the region of four to six milliseconds,
//! and an ERM has none to report. If neither passes, nothing is moving and the log's own
//! numbers say why.
//!
//! **The enable line is GPIO38.** Holding a free pin high and asking the chip's own diagnostic
//! again turns `0xE9` into `0xE0` only on that pin, and the motor buzzes once as it happens.
//! Ruled out for reasons that have nothing to do with haptics: GPIO7-12 (encoder, touch, the bus
//! itself), GPIO13-18, 21 and 47 (display), GPIO19/20 (the USB this log arrives over), GPIO26-37
//! (flash and PSRAM), GPIO45/46 (strapping).
//!
//! There is no picture in this binary on purpose. The measurement is in the fingers.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Level, Output, OutputConfig};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::Rate;
use teetotum::haptic::{
    ADDRESS, ADDRESS_AS_FOUND, Actuator, CalTime, Calibration, Haptic, Library,
};
use log::{error, info, warn};

/// Whether this run is allowed to write to the driver at all.
const WRITES_ALLOWED: bool = true;

/// The overdrive clamp used while looking for any sign of life. Close to the chip's own default
/// but not it: that is `0x8C`, as read back at boot.
///
/// Haptic drive is overdriven on purpose -- a click is a short burst above the rated voltage --
/// and every burst here is 300 ms at most.
const DRIVE_CLAMP: u8 = 0x89;
/// The rated voltage that goes with it; the chip's own is `0x3E`.
const DRIVE_RATED: u8 = 0x3F;

/// The effects played in the round, with the datasheet's names for them.
const ROUND: &[(u8, &str)] = &[
    (1, "strong click, 100%"),
    (14, "strong buzz, 100%"),
    (47, "buzz 1, 100%"),
    (52, "pulsing strong 1, 100%"),
];

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    // A slave that was interrupted mid-transfer can sit there holding the bus and matching a
    // different address than its own. The cure is older than the chip: release SDA, clock SCL
    // by hand until the slave has given up whatever byte it was in the middle of, then send a
    // stop. Sixteen clocks is twice what a stuck byte can need.
    {
        let mut scl = Output::new(
            peripherals.GPIO12.reborrow(),
            Level::High,
            OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
        );
        let mut sda = Output::new(
            peripherals.GPIO11.reborrow(),
            Level::High,
            OutputConfig::default().with_drive_mode(DriveMode::OpenDrain),
        );
        sda.set_high();
        for _ in 0..16 {
            scl.set_low();
            delay.delay_micros(5);
            scl.set_high();
            delay.delay_micros(5);
        }
        // A stop condition: SDA rises while SCL is high.
        sda.set_low();
        delay.delay_micros(5);
        scl.set_high();
        delay.delay_micros(5);
        sda.set_high();
        delay.delay_micros(5);
        info!("Bus: sixteen recovery clocks sent on GPIO12, then a stop");
    }

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11.reborrow())
    .with_scl(peripherals.GPIO12.reborrow());

    // Before anything is asked of the haptic driver: who is on the bus at all? A silent
    // driver and a silent bus look the same from one failed read, and they are not the same
    // fault. The touch controller at 0x15 is the witness -- it answers in every other binary.
    scan(&mut i2c);
    // Who is 0x40? The only way to tell a chip from a ghost is to ask it for bytes and see
    // whether they look like a register map or like a floating bus. 0x15 is the control: a
    // device known to be real.
    dump(&mut i2c, 0x40);
    dump(&mut i2c, 0x15);
    dump(&mut i2c, 0x5A);

    // Which address to talk to. The datasheet's 0x5A is tried first and the one this board
    // actually answers on is the fallback, so that a board where nothing odd has happened
    // behaves the ordinary way.
    // The enable line: only GPIO38 turns the chip's diagnostic `0xE9` into `0xE0`, and the
    // motor buzzes as it happens.
    let enable = Output::new(
        peripherals.GPIO38.reborrow(),
        Level::High,
        OutputConfig::default(),
    );
    delay.delay_millis(1);

    let mut haptic = if answers(&mut i2c, ADDRESS) {
        Haptic::at(ADDRESS).with_enable(enable, &delay)
    } else if answers(&mut i2c, ADDRESS_AS_FOUND) {
        warn!("Haptic: not at {ADDRESS:#04x}, but something with its register map is at {ADDRESS_AS_FOUND:#04x}");
        Haptic::at(ADDRESS_AS_FOUND).with_enable(enable, &delay)
    } else {
        Haptic::at(ADDRESS).with_enable(enable, &delay)
    };
    info!("Haptic: talking to {:#04x}", haptic.address());

    match haptic.device_id(&mut i2c) {
        Ok(7) => info!("Haptic: device id 7, a DRV2605L"),
        Ok(3) => warn!("Haptic: device id 3, a DRV2605 -- not the L variant this expects"),
        Ok(other) => warn!("Haptic: device id {other}, which is not in the datasheet's list"),
        Err(err) => {
            error!("Haptic: nothing answers at 0x5A: {err:?}");
            // Keep scanning rather than stopping: if the driver comes back on its own, that
            // says something about power or enable that a dead binary never would.
            loop {
                delay.delay_millis(2000);
                scan(&mut i2c);
            }
        }
    }

    // How writes behave, counted rather than assumed.
    //
    // Reads at 0x5A work on a fresh boot and the first write is refused with a NACK on the
    // data byte, after which the chip stops answering its address at all. Three things could
    // do that, and they are told apart by counting: a chip that refuses *every* write (the
    // count is zero), a second master on the bus colliding with ours (the count is ragged),
    // or the device reset specifically (harmless writes succeed and only 0x01 fails).
    //
    // The writes chosen are the harmless ones: setting the register pointer, and writing the
    // real-time-playback amplitude to zero, which is what it already is.
    {
        let mut pointer_ok = 0;
        let mut rtp_ok = 0;
        let mut first_failure = None;
        for attempt in 0..20 {
            if i2c.write(haptic.address(), &[0x00]).is_ok() {
                pointer_ok += 1;
            } else if first_failure.is_none() {
                first_failure = Some((attempt, "pointer"));
            }
            if i2c.write(haptic.address(), &[0x02, 0x00]).is_ok() {
                rtp_ok += 1;
            } else if first_failure.is_none() {
                first_failure = Some((attempt, "rtp"));
            }
        }
        info!(
            "Haptic: of 20 attempts, {pointer_ok} pointer writes and {rtp_ok} amplitude writes were acknowledged"
        );
        if let Some((attempt, which)) = first_failure {
            info!("Haptic: the first refusal was the {which} write on attempt {attempt}");
        }
        match haptic.status(&mut i2c) {
            Ok(status) => info!("Haptic: status after the write attempts: {status:#04x}"),
            Err(err) => warn!("Haptic: no answer after the write attempts: {err:?}"),
        }
    }

    // Read-only until the driver has been seen to survive a look: writing the device reset
    // (`MODE = 0x80`) wedges the chip's I2C interface until the next boot, so the writes below
    // stay behind a switch until a scan says what state the bus is in.
    if !WRITES_ALLOWED {
        warn!("Haptic: writes are switched off; scanning and reading only");
        loop {
            if let Ok(status) = haptic.status(&mut i2c) {
                info!("Haptic: status {:#04x}, device id {}", status, status >> 5);
            }
            delay.delay_millis(2000);
            scan(&mut i2c);
        }
    }

    // No device reset here. It is the one write this chip refuses, and refusing it costs the
    // whole I2C interface until the next boot -- see `Haptic::reset`. Everything below
    // configures the registers by hand instead.
    //
    // The chip wakes in standby, where writes land but nothing moves.
    let _ = haptic.wake(&mut i2c);

    if let Ok(mv) = haptic.supply_mv(&mut i2c) {
        info!("Haptic: supply {} mV as the chip measures it", mv);
    }
    report_settings(&mut haptic, &mut i2c, "out of reset");

    // Does anything hang on the output at all? The diagnostic drives it briefly and looks.
    match haptic.diagnose(&mut i2c, &delay) {
        Ok(status) => info!(
            "Haptic: diagnostic status {:#04x} -- {}",
            status,
            if status & 0x08 == 0 {
                "an actuator is connected"
            } else {
                "open or shorted, says the chip"
            }
        ),
        Err(err) => error!("Haptic: the diagnostic failed to run: {err:?}"),
    }

    // The two calibrations, each from a clean reset so that neither inherits the other's work.
    let erm = calibrate_as(&mut haptic, &mut i2c, &delay, Actuator::Erm);
    let lra = calibrate_as(&mut haptic, &mut i2c, &delay, Actuator::Lra);

    let period = haptic.lra_period_us(&mut i2c).unwrap_or(None);
    match period {
        Some(us) => info!(
            "Haptic: resonance period {} us, which is {} Hz",
            us,
            1_000_000 / us.max(1)
        ),
        None => info!("Haptic: the resonance register is empty -- no LRA period was measured"),
    }

    // Which configuration to play with: whichever calibrated, and the LRA when both did, since
    // only an LRA leaves a period behind.
    let passed_erm = erm.map(|c| c.passed).unwrap_or(false);
    let passed_lra = lra.map(|c| c.passed).unwrap_or(false);
    let (actuator, library, verdict) = match (passed_erm, passed_lra, period) {
        (_, true, Some(_)) => (Actuator::Lra, Library::Lra, "an LRA, and it named its frequency"),
        (false, true, None) => (Actuator::Lra, Library::Lra, "an LRA, on the calibration alone"),
        (true, false, _) => (Actuator::Erm, Library::ErmB, "an ERM"),
        (true, true, None) => (Actuator::Erm, Library::ErmB, "ambiguous: both passed, no period"),
        (false, false, _) => (
            Actuator::Lra,
            Library::Lra,
            "neither passed -- playing as an LRA regardless, to see whether anything moves",
        ),
    };
    info!("Haptic: the actuator is {verdict}");

    if let Err(err) = haptic.set_actuator(&mut i2c, actuator) {
        error!("Haptic: could not set the actuator kind: {err:?}");
    }
    if let Err(err) = haptic.set_library(&mut i2c, library) {
        error!("Haptic: could not select the library: {err:?}");
    }
    let _ = haptic.wake(&mut i2c);
    report_settings(&mut haptic, &mut i2c, "after calibration");

    info!("Haptic: hold the knob. The round starts in three seconds and then repeats.");
    delay.delay_millis(3000);

    let mut round = 0u32;
    loop {
        round += 1;
        info!("Haptic: round {round}");

        // Four ways of driving the same output, from the most polite to the most stubborn.
        // The closed loop asks the actuator for feedback and gives up when there is none; the
        // open loop does not ask. If nothing at all is felt in the open-loop passes, the drive
        // is not the reason, and the actuator is not on these pins.
        for &(actuator, open, amplitude, what) in &[
            (Actuator::Erm, false, 0x7Fu8, "ERM, closed loop, half amplitude"),
            (Actuator::Erm, true, 0xFF, "ERM, OPEN loop, full amplitude"),
            (Actuator::Lra, false, 0xFF, "LRA, closed loop, full amplitude"),
            (Actuator::Lra, true, 0xFF, "LRA, OPEN loop, full amplitude, 5 ms period"),
        ] {
            info!("Haptic:   {what}");
            let _ = haptic.set_actuator(&mut i2c, actuator);
            let _ = haptic.set_open_loop(&mut i2c, actuator, open);
            // A period of 0x33 is 5.0 ms, i.e. 199 Hz -- where small LRAs live.
            let _ = haptic.set_open_loop_period(&mut i2c, 0x33);
            let _ = haptic.set_drive(&mut i2c, DRIVE_RATED, DRIVE_CLAMP);
            if let Err(err) = haptic.play_raw(&mut i2c, amplitude, 300, &delay) {
                error!("Haptic: {what} failed: {err:?}");
            }
            delay.delay_millis(900);
        }

        // And the ROM, in both libraries: the effects are what the factory demo plays.
        for &(library, actuator, name) in &[
            (Library::ErmB, Actuator::Erm, "ERM library B"),
            (Library::Lra, Actuator::Lra, "LRA library"),
        ] {
            let _ = haptic.set_actuator(&mut i2c, actuator);
            let _ = haptic.set_open_loop(&mut i2c, actuator, false);
            let _ = haptic.set_library(&mut i2c, library);
            let _ = haptic.set_drive(&mut i2c, DRIVE_RATED, DRIVE_CLAMP);
            for &(effect, effect_name) in ROUND {
                info!("Haptic:   {name}, effect {effect}: {effect_name}");
                if let Err(err) = haptic.play(&mut i2c, effect, &delay) {
                    error!("Haptic: effect {effect} failed: {err:?}");
                }
                delay.delay_millis(700);
            }
        }

        delay.delay_millis(2000);
    }
}

/// Walks the seven-bit addresses and logs who acknowledges, asked in two different ways.
///
/// The distinction matters. A bus scan built on a **read** and one built on a **write** do not
/// find the same devices: a chip can acknowledge its address and then refuse the transfer, and
/// an address that answers one way but not the other is a hint that it is not a chip at all.
fn scan(i2c: &mut I2c<'_, esp_hal::Blocking>) {
    let mut found = 0;
    for address in 0x08..=0x77u8 {
        let mut byte = [0u8; 1];
        let reads = i2c.read(address, &mut byte).is_ok();
        // A zero-length write: the address goes out and the stop follows immediately, so
        // nothing but the acknowledge is being asked about.
        let writes = i2c.write(address, &[]).is_ok();
        if reads || writes {
            info!(
                "Bus: {address:#04x} answers ({}{}{})",
                if reads { "read" } else { "" },
                if reads && writes { " and " } else { "" },
                if writes { "write" } else { "" }
            );
            found += 1;
        }
    }
    if found == 0 {
        warn!("Bus: nobody answers on GPIO11/GPIO12 at all");
    }
}

/// Reads the first sixteen registers of a device and logs them as one line.
///
/// Bytes that are all `0xFF` or all `0x00` are what an absent chip and a floating bus look
/// like; a real register map is uneven.
fn dump(i2c: &mut I2c<'_, esp_hal::Blocking>, address: u8) {
    let mut bytes = [0u8; 16];
    for (register, slot) in bytes.iter_mut().enumerate() {
        let mut byte = [0u8; 1];
        *slot = match i2c.write_read(address, &[register as u8], &mut byte) {
            Ok(()) => byte[0],
            Err(_) => 0xEE,
        };
    }
    info!("Bus {address:#04x}: registers 0x00-0x0F = {bytes:02x?}");
}

/// Whether an address acknowledges at all.
fn answers(i2c: &mut I2c<'_, esp_hal::Blocking>, address: u8) -> bool {
    let mut byte = [0u8; 1];
    i2c.read(address, &mut byte).is_ok()
}

/// Reads all 256 registers of a device and logs them sixteen to a line.
///
/// Not called in the ordinary run -- it is the tool that identified the chip at `0x40` by its
/// register defaults, and it stays for the next unknown address.
#[allow(dead_code)]
fn dump_all(i2c: &mut I2c<'_, esp_hal::Blocking>, address: u8) {
    for page in 0..16u16 {
        let mut bytes = [0u8; 16];
        for (offset, slot) in bytes.iter_mut().enumerate() {
            let register = (page * 16 + offset as u16) as u8;
            let mut byte = [0u8; 1];
            *slot = match i2c.write_read(address, &[register], &mut byte) {
                Ok(()) => byte[0],
                Err(_) => 0xEE,
            };
        }
        info!("Bus {address:#04x}: {:#04x} = {bytes:02x?}", page * 16);
    }
}

/// Resets, declares the actuator kind, calibrates, and logs everything that came back.
fn calibrate_as(
    haptic: &mut Haptic<'_>,
    i2c: &mut I2c<'_, esp_hal::Blocking>,
    delay: &Delay,
    actuator: Actuator,
) -> Option<Calibration> {
    let name = match actuator {
        Actuator::Erm => "ERM",
        Actuator::Lra => "LRA",
    };

    let _ = haptic.wake(i2c);
    if let Err(err) = haptic.set_actuator(i2c, actuator) {
        error!("Haptic: could not declare the actuator as {name}: {err:?}");
        return None;
    }
    // Something has to be given as a starting point; these are the chip's own defaults, and
    // the point of the run is the diagnostic bit rather than the numbers.
    let _ = haptic.set_drive(i2c, DRIVE_RATED, DRIVE_CLAMP);

    match haptic.auto_calibrate(i2c, delay, CalTime::Longest) {
        Ok(result) => {
            info!(
                "Haptic: calibration as {name}: {}, status {:#04x}, compensation {:#04x}, back-EMF {:#04x}, feedback {:#04x}",
                if result.passed { "passed" } else { "FAILED" },
                result.status,
                result.compensation,
                result.back_emf,
                result.feedback
            );
            Some(result)
        }
        Err(err) => {
            error!("Haptic: the {name} calibration could not be run: {err:?}");
            None
        }
    }
}

/// Logs the nine registers worth reading, with a note saying when they were read.
fn report_settings(haptic: &mut Haptic<'_>, i2c: &mut I2c<'_, esp_hal::Blocking>, when: &str) {
    match haptic.settings(i2c) {
        Ok([rated, clamp, comp, bemf, feedback, c1, c2, c3, c4]) => info!(
            "Haptic ({when}): rated {rated:#04x}, clamp {clamp:#04x}, comp {comp:#04x}, bemf {bemf:#04x}, feedback {feedback:#04x} ({}), control {c1:#04x} {c2:#04x} {c3:#04x} {c4:#04x}",
            if feedback & 0x80 == 0 { "ERM" } else { "LRA" }
        ),
        Err(err) => error!("Haptic ({when}): the registers could not be read: {err:?}"),
    }
}
