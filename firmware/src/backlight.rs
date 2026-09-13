//! How bright the glass is.
//!
//! The panel's LEDs hang from 3V3 through 3.9 ohm and are switched on the low side by an
//! AO3400A, whose gate is GPIO47 with a 10 k pull-down (Waveshare's schematic, net `LCD_BLK`;
//! that the pin lights the glass is measured, the rest of the circuit is read off the drawing).
//! A gate is either open or shut, so the glass is dimmed in time rather than in current: the LED
//! controller switches the pin at 5 kHz, and the brightness is the share of each period it is on.
//! 5 kHz and 10 bits are what Espressif's board support package uses for the same job -- taken
//! over, not measured.
//!
//! **The step a user picks is not the duty.** Brightness is seen roughly logarithmically, so
//! equal steps in duty look like big jumps at the dark end and like nothing at the bright end.
//! The duty goes with the square of the step instead, the usual cheap stand-in for a perceptual
//! curve: the dimmest of the ten steps is on 1 % of the time, the middle one 25 %.
//!
//! It lives in the firmware and not in the SDK for the reason the settings do: how bright the
//! glass is, is the device's to say and not a face's.

use esp_hal::gpio::DriveMode;
use esp_hal::gpio::interconnect::PeripheralOutput;
use esp_hal::ledc::channel::{self, Channel, ChannelHW, ChannelIFace};
use esp_hal::ledc::timer::{self, Timer, TimerIFace};
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::peripherals::LEDC;
use esp_hal::time::Rate;
use static_cell::StaticCell;

use crate::settings::Brightness;

/// The resolution of the duty, and the count that means "on for the whole period".
const RESOLUTION: timer::config::Duty = timer::config::Duty::Duty10Bit;
const FULL: u32 = 1 << 10;

/// How often the pin is switched.
const FREQUENCY: Rate = Rate::from_khz(5);

/// The backlight pin, driven by the LED controller.
pub struct Backlight {
    channel: Channel<'static, LowSpeed>,
}

/// Why the LED controller would not take the configuration.
#[derive(Debug)]
pub enum Error {
    Timer(timer::Error),
    Channel(channel::Error),
}

impl Backlight {
    /// Takes the LED controller and the backlight pin, and starts dark.
    ///
    /// **Dark on purpose.** The brightness is a setting, and the settings are read before the
    /// first picture is drawn; starting dark lets the glass light up once, at the step it keeps,
    /// instead of at full and then a moment later dimmer. It also means the panel's power-up
    /// contents are never lit.
    ///
    /// There is one of these: the timer it configures lives in a static, and a second call
    /// panics.
    pub fn new(ledc: LEDC<'static>, pin: impl PeripheralOutput<'static>) -> Result<Self, Error> {
        static TIMER: StaticCell<Timer<'static, LowSpeed>> = StaticCell::new();

        let mut ledc = Ledc::new(ledc);
        ledc.set_global_slow_clock(LSGlobalClkSource::APBClk);
        let timer = TIMER.init(ledc.timer::<LowSpeed>(timer::Number::Timer0));
        timer
            .configure(timer::config::Config {
                duty: RESOLUTION,
                clock_source: timer::LSClockSource::APBClk,
                frequency: FREQUENCY,
            })
            .map_err(Error::Timer)?;
        let timer: &'static Timer<'static, LowSpeed> = timer;

        let mut channel = ledc.channel(channel::Number::Channel0, pin);
        channel
            .configure(channel::config::Config {
                timer,
                duty_pct: 0,
                drive_mode: DriveMode::PushPull,
            })
            .map_err(Error::Channel)?;
        Ok(Self { channel })
    }

    /// Lights the glass at `level`.
    pub fn set(&self, level: Brightness) {
        self.channel.set_duty_hw(duty(level));
    }
}

/// The count `level` switches the pin on for, out of [`FULL`].
fn duty(level: Brightness) -> u32 {
    let step = u32::from(level.step());
    let top = u32::from(Brightness::MAX.step());
    FULL * step * step / (top * top)
}
