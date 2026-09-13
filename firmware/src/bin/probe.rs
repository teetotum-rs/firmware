//! What is on the I2C bus, and do the touch and encoder pins do anything?
//!
//! Six rows of the pin table in `docs/hardware/pins.md` are marked verified, because each of
//! them produced an effect only the right pin could produce. Four are still hearsay copied from a
//! third party's ESPHome configuration: touch reset and interrupt, the two I2C lines, and the
//! encoder's A and B. This binary asks the board about all four.
//!
//! It works in two steps. First it scans the I2C bus, once with SDA and SCL as the table
//! claims and, if nothing answers, once with the two swapped -- a bus wired the other way
//! round is silent rather than wrong, so the swap costs one pass and rules out the likeliest
//! mistake. An address that answers is a device that exists on pins that carry; 0x15 would be
//! the CST816S touch controller and 0x5A the DRV2605 haptic driver, and both are asked for an
//! identifying register afterwards so that "something acked" becomes "this chip is there".
//!
//! Then it holds, reading the touch controller and the knob and logging every change. Nothing
//! here draws on the glass: the point is the serial log, so that a finger on the glass or a
//! turn of the knob shows up as a line or does not.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::Rate;
use log::{info, warn};
use teetotum::encoder::Encoder;

esp_bootloader_esp_idf::esp_app_desc!();

/// The CST816S answers here, if the touch controller is what the firmware image suggested.
const TOUCH_ADDRESS: u8 = 0x15;
/// First of the six status registers: gesture, finger count, then X and Y as two bytes each.
const TOUCH_STATUS: u8 = 0x01;
/// Holds a chip identifier -- 0xB5 for a CST816S, other values for its relatives.
const TOUCH_CHIP_ID: u8 = 0xA7;
/// Firmware version of the touch controller, for the record.
const TOUCH_FIRMWARE: u8 = 0xA9;

/// The DRV2605 haptic driver, named in the factory image.
const HAPTICS_ADDRESS: u8 = 0x5A;
/// Status register; its top three bits are the device identifier.
const HAPTICS_STATUS: u8 = 0x00;

/// How often the knob is polled.
///
/// Its pulses stay low for 15 milliseconds at their shortest, so a millisecond is comfortably
/// faster than the fastest turn.
const POLL_INTERVAL_MS: u32 = 1;
/// How many polls pass between reads of the touch controller.
const TOUCH_EVERY: u32 = 20;

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    // The library's display module pulls in a crate that allocates; nothing here does.
    esp_alloc::heap_allocator!(size: 8 * 1024);

    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    // The touch controller holds its bus lines quiet until reset is released, so a scan that
    // skipped this would find nothing and blame the wiring.
    let mut touch_reset = Output::new(
        peripherals.GPIO10.reborrow(),
        Level::High,
        OutputConfig::default(),
    );
    delay.delay_millis(10);
    touch_reset.set_low();
    delay.delay_millis(20);
    touch_reset.set_high();
    delay.delay_millis(100);

    let config = I2cConfig::default().with_frequency(Rate::from_khz(100));

    let mut swapped = false;
    let found = {
        let mut i2c = I2c::new(peripherals.I2C0.reborrow(), config)
            .expect("the I2C peripheral could not be configured")
            .with_sda(peripherals.GPIO11.reborrow())
            .with_scl(peripherals.GPIO12.reborrow());
        info!("Probe: scanning with SDA on GPIO11, SCL on GPIO12");
        scan(&mut i2c)
    };

    let found = if found > 0 {
        found
    } else {
        swapped = true;
        let mut i2c = I2c::new(peripherals.I2C0.reborrow(), config)
            .expect("the I2C peripheral could not be configured")
            .with_sda(peripherals.GPIO12.reborrow())
            .with_scl(peripherals.GPIO11.reborrow());
        info!("Probe: nothing answered; scanning with SDA and SCL swapped");
        scan(&mut i2c)
    };

    if found == 0 {
        warn!("Probe: no device answered on either wiring -- GPIO11 and GPIO12 are not this bus");
        swapped = false;
    } else {
        info!(
            "Probe: {found} device(s) answered with SDA on GPIO{}, SCL on GPIO{}",
            if swapped { 12 } else { 11 },
            if swapped { 11 } else { 12 }
        );
    }

    let mut i2c = if swapped {
        I2c::new(peripherals.I2C0.reborrow(), config)
            .expect("the I2C peripheral could not be configured")
            .with_sda(peripherals.GPIO12.reborrow())
            .with_scl(peripherals.GPIO11.reborrow())
    } else {
        I2c::new(peripherals.I2C0.reborrow(), config)
            .expect("the I2C peripheral could not be configured")
            .with_sda(peripherals.GPIO11.reborrow())
            .with_scl(peripherals.GPIO12.reborrow())
    };

    identify(&mut i2c);

    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let touch_interrupt = Input::new(peripherals.GPIO9.reborrow(), pull_up);
    // Which of the two lines counts up is arbitrary until something on the glass says
    // otherwise; see `Encoder::new`.
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    info!("Probe: holding -- turn the knob and touch the glass");

    let mut reported = 0;
    let mut interrupt_was_low = touch_interrupt.is_low();
    let mut touching = false;
    let mut ticks: u32 = 0;

    loop {
        encoder.poll();

        let interrupt_is_low = touch_interrupt.is_low();
        if interrupt_is_low != interrupt_was_low {
            info!(
                "Touch: INT on GPIO9 went {}",
                if interrupt_is_low { "low" } else { "high" }
            );
            interrupt_was_low = interrupt_is_low;
        }

        ticks += 1;
        if ticks.is_multiple_of(TOUCH_EVERY) {
            let position = encoder.position();
            if position != reported {
                info!("Encoder: position {position} (was {reported})");
                reported = position;
            }
            touching = read_touch(&mut i2c, touching);
        }

        delay.delay_millis(POLL_INTERVAL_MS);
    }
}

/// Asks every address on the bus whether anything is there.
///
/// A one-byte read is the question: what comes back does not matter, only whether the address
/// was acknowledged. The reserved ranges at either end of the address space are skipped.
fn scan(i2c: &mut I2c<'_, Blocking>) -> usize {
    let mut found = 0;
    for address in 0x08..=0x77u8 {
        let mut byte = [0u8; 1];
        if i2c.read(address, &mut byte).is_ok() {
            info!("Probe: address {address:#04x} answered");
            found += 1;
        }
    }
    found
}

/// Turns "something acked" into "this chip is there", for the two devices we expect.
fn identify(i2c: &mut I2c<'_, Blocking>) {
    let mut byte = [0u8; 1];

    match i2c.write_read(TOUCH_ADDRESS, &[TOUCH_CHIP_ID], &mut byte) {
        Ok(()) => {
            let chip = byte[0];
            let name = match chip {
                0xB4 => " (CST816T)",
                0xB5 => " (CST816S)",
                0xB6 => " (CST816D)",
                _ => "",
            };
            info!("Touch: chip id {chip:#04x}{name}");
            if i2c
                .write_read(TOUCH_ADDRESS, &[TOUCH_FIRMWARE], &mut byte)
                .is_ok()
            {
                info!("Touch: firmware version {:#04x}", byte[0]);
            }
        }
        Err(err) => warn!("Touch: nothing at {TOUCH_ADDRESS:#04x}: {err:?}"),
    }

    match i2c.write_read(HAPTICS_ADDRESS, &[HAPTICS_STATUS], &mut byte) {
        // The identifier lives in the top three bits: 3 is a DRV2605, 7 a DRV2605L.
        Ok(()) => info!(
            "Haptics: status {:#04x}, device id {}",
            byte[0],
            byte[0] >> 5
        ),
        Err(err) => warn!("Haptics: nothing at {HAPTICS_ADDRESS:#04x}: {err:?}"),
    }
}

/// Reads the touch controller's status and logs a finger arriving, moving or leaving.
///
/// Returns whether a finger is on the glass, so that the caller can tell a new touch from a
/// continuing one; a touch held still would otherwise fill the log.
fn read_touch(i2c: &mut I2c<'_, Blocking>, was_touching: bool) -> bool {
    let mut status = [0u8; 6];
    if i2c
        .write_read(TOUCH_ADDRESS, &[TOUCH_STATUS], &mut status)
        .is_err()
    {
        return was_touching;
    }

    let [gesture, fingers, x_high, x_low, y_high, y_low] = status;
    // Both coordinates are twelve bits: four in the low nibble of the high byte, eight below.
    let x = u16::from(x_high & 0x0F) << 8 | u16::from(x_low);
    let y = u16::from(y_high & 0x0F) << 8 | u16::from(y_low);
    let touching = fingers > 0;

    if touching {
        info!("Touch: {x},{y} gesture {gesture:#04x}");
    } else if was_touching {
        info!("Touch: released");
    }
    touching
}
