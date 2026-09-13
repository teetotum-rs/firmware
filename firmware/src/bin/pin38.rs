//! Which of the two things GPIO38 is, and whether it has to choose.
//!
//! Two readings of the same pin stand against each other:
//!
//! - **Waveshare's schematic** calls it `ESP32S3_TX`, the S3's half of the serial link to the
//!   classic ESP32, and ties the haptic driver's enable to 3V3 for good.
//! - **Measured instead** (`src/bin/haptic.rs`): holding GPIO38 high turns the DRV2605L's own
//!   diagnostic from `0xE9`, an open or shorted actuator, into `0xE0`, and the motor moves as
//!   it happens.
//!
//! Audio belongs to the other chip, which says nothing until it is spoken to
//! (`src/bin/uartlisten.rs`), and speaking to it means driving GPIO38. So the pin has to be
//! settled before a byte is sent.
//!
//! The cheap part of the test is a repeat of the old one, run in one piece so the two states
//! are compared inside a single boot:
//!
//! 1. **Untouched.** GPIO38 is never configured. The chip is asked for its status and its
//!    diagnostic. `0xE0` here would mean the enable was never our business and the schematic is
//!    right; `0xE9` means the pin does something the schematic does not describe.
//! 2. **Probed.** GPIO38 as an input against an internal pull-down and then a pull-up. A pin
//!    tied to 3V3 on the board reads high against both; a floating one follows the pull.
//! 3. **Held high**, then **held low**, then high again, with the diagnostic run after each.
//!    Repeating tells a level-sensitive enable from a one-time strobe: if low brings `0xE9`
//!    back, the chip is watching the line and not remembering an edge.
//!
//! The expensive part is the question underneath, and it is not "which of the two is it":
//!
//! **A UART's transmit line idles high.** If GPIO38 is both the enable and the serial output,
//! the enable is high whenever nothing is being said, and it dips only for the low bits of a
//! byte -- at 115200 baud, 8.7 us at a time, and at most 78 us in a row for a byte of all
//! zeros. An enable line with a 1 uF decoupling capacitor behind it may not notice that at all.
//! So step 4 sends a solid stream of `0x00`, the harshest pattern a UART can produce, *while*
//! the diagnostic runs and *while* an effect plays. If the diagnostic still reads `0xE0` and
//! the knob still clicks, the pin does not have to choose, and the serial link to the other
//! chip is open without giving up the haptics.
//!
//! GPIO48 is held open as a receiver throughout the last step, because a stream of zeros is
//! still a stream: if the other chip answers anything at all, it should not go unheard.
//!
//! Half of this measurement is in the fingers, so the run does not set the pace: every step
//! that needs a hand waits for one, and repeats on request. Turn the knob or press Enter to go
//! on, touch the glass or press `r` to have the same thing again, `q` to let the rest run
//! through -- `src/step.rs` holds each step until answered, so the run cannot outrun its own
//! instructions.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info, warn};
use teetotum::encoder::Encoder;
use teetotum::haptic::{ADDRESS, ADDRESS_AS_FOUND, Actuator, CalTime, Haptic, Library, Mode};
use teetotum::step::Prompt;
use teetotum::touch::Touch;

/// The baud rate the factory firmware's `UART1` task is configured for, and the rate at which
/// a low bit is shortest -- the friendliest case for an enable line and the honest one to try
/// first, since it is the rate anything real would use.
const BAUD: u32 = 115_200;

/// One FIFO's worth of the worst byte there is: a start bit and eight zero bits, so the line
/// spends nine of every ten bit times low.
const ZEROS: [u8; 64] = [0u8; 64];

/// How long a phase pumps bytes while the chip is busy.
const PUMP_MS: u64 = 300;

/// The effect played whenever the log asks for a finger on the knob.
const CLICK: u8 = 1;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    // A slave left mid-transfer by the last binary holds the bus and answers to the wrong
    // address. Sixteen clocks and a stop cost nothing and rule that out; the reasoning is in
    // `src/bin/haptic.rs`, where it was needed.
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
        for _ in 0..16 {
            scl.set_low();
            delay.delay_micros(5);
            scl.set_high();
            delay.delay_micros(5);
        }
        sda.set_low();
        delay.delay_micros(5);
        scl.set_high();
        delay.delay_micros(5);
        sda.set_high();
        delay.delay_micros(5);
    }

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11.reborrow())
    .with_scl(peripherals.GPIO12.reborrow());

    // The three ways to answer, all equivalent: the knob under the hand that is about to judge
    // a click, the glass under the other one, and the keyboard at the far end of the monitor.
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut prompt = Prompt::new()
        .with_encoder(Encoder::new(
            &mut io,
            Input::new(peripherals.GPIO8, pull_up),
            Input::new(peripherals.GPIO7, pull_up),
        ))
        .with_touch(Touch::attached(
            Output::new(
                peripherals.GPIO10.reborrow(),
                Level::High,
                OutputConfig::default(),
            ),
            Input::new(peripherals.GPIO9.reborrow(), pull_up),
        ))
        .with_keys(
            UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow())
                .split()
                .0,
        );

    // Which address the driver answers on today. Both are measurements; the datasheet's first.
    let address = if answers(&mut i2c, ADDRESS) {
        ADDRESS
    } else if answers(&mut i2c, ADDRESS_AS_FOUND) {
        warn!("Haptic: answering at {ADDRESS_AS_FOUND:#04x} rather than {ADDRESS:#04x}");
        ADDRESS_AS_FOUND
    } else {
        error!("Haptic: nothing on the bus at either address -- nothing below means anything");
        loop {
            delay.delay_millis(2000);
        }
    };
    // No enable line: this instance is deliberately not allowed to touch GPIO38.
    let mut haptic = Haptic::at(address);
    match haptic.device_id(&mut i2c) {
        Ok(7) => info!("Haptic: device id 7 at {address:#04x}, a DRV2605L"),
        Ok(other) => warn!("Haptic: device id {other} at {address:#04x}, not the expected 7"),
        Err(err) => error!("Haptic: {err:?}"),
    }

    // ---- 1. Untouched -------------------------------------------------------------------
    //
    // GPIO38 has not been configured by this binary and will not be until the next section.
    // Whatever the diagnostic says here, the board said without our help.
    info!("--- 1. GPIO38 untouched ---");
    prompt.wait(
        &mut i2c,
        "nothing to feel yet -- this reads the driver with the pin left alone",
    );
    let untouched = diagnose_three_times(&mut haptic, &mut i2c, &delay);
    match untouched {
        Some(0xE0) => info!(
            "  0xE0 with nobody driving the pin: the enable is not ours, and the schematic's fixed 3V3 fits"
        ),
        Some(0xE9) => info!(
            "  0xE9 with nobody driving the pin: the output stage is off until something raises it"
        ),
        Some(other) => {
            warn!("  {other:#04x}: neither of the two answers this test was written for")
        }
        None => error!("  the chip did not answer at all"),
    }

    // ---- 2. Probed ----------------------------------------------------------------------
    //
    // An input with a pull is still not a driver: 45 kOhm against whatever the board does.
    info!("--- 2. what the board itself holds GPIO38 at ---");
    {
        let down = Input::new(
            peripherals.GPIO38.reborrow(),
            InputConfig::default().with_pull(Pull::Down),
        );
        delay.delay_millis(2);
        let low_reading = down.level();
        let up = Input::new(
            peripherals.GPIO38.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        );
        delay.delay_millis(2);
        let high_reading = up.level();
        match (low_reading, high_reading) {
            (Level::High, Level::High) => {
                info!("  high against a pull-down as well: something on the board holds it up")
            }
            (Level::Low, Level::Low) => {
                info!("  low against a pull-up as well: something on the board holds it down")
            }
            (Level::Low, Level::High) => {
                info!("  it follows the pull, so nothing on the board drives it -- the pin is ours")
            }
            (Level::High, Level::Low) => {
                warn!("  inverted readings; something is wrong with this test")
            }
        }
    }

    // ---- 3. Held high, held low, held high ----------------------------------------------
    //
    // Three states in one boot. If low undoes what high did, the chip reads the line
    // continuously; if it does not, high was a strobe and the line is free between strobes.
    info!("--- 3. GPIO38 driven ---");
    let mut enable = Output::new(
        peripherals.GPIO38.reborrow(),
        Level::High,
        OutputConfig::default(),
    );
    delay.delay_millis(5);
    info!("  held high:");
    let held_high = diagnose_three_times(&mut haptic, &mut i2c, &delay);
    enable.set_low();
    delay.delay_millis(5);
    info!("  held low again:");
    let held_low = diagnose_three_times(&mut haptic, &mut i2c, &delay);
    enable.set_high();
    delay.delay_millis(5);
    info!("  high once more:");
    let held_high_again = diagnose_three_times(&mut haptic, &mut i2c, &delay);

    match (untouched, held_high, held_low, held_high_again) {
        (Some(a), Some(b), Some(c), Some(d)) if a == b && b == c && c == d => info!(
            "  the diagnostic never moved: on this board GPIO38 is not what switches the output stage"
        ),
        (_, Some(0xE0), Some(0xE9), Some(0xE0)) => info!(
            "  it follows the line both ways: GPIO38 is a level-sensitive enable, high means on"
        ),
        (_, Some(0xE0), Some(0xE0), _) => info!(
            "  high turned it on and low did not turn it off: the pin is a strobe, not a switch"
        ),
        _ => warn!("  the four readings do not fit any of the shapes this test expected"),
    }

    // Calibrate here, while the line is steady, so that the effect played under traffic later
    // is the same effect and not a differently configured one.
    info!("--- calibrating as an LRA, with GPIO38 held high ---");
    if let Err(err) = haptic.set_actuator(&mut i2c, Actuator::Lra) {
        error!("  could not select the LRA: {err:?}");
    }
    match haptic.auto_calibrate(&mut i2c, &delay, CalTime::Longest) {
        Ok(cal) if cal.passed => {
            let period = haptic.lra_period_us(&mut i2c).ok().flatten();
            info!(
                "  calibration passed: compensation {:#04x}, back-EMF {:#04x}, resonance {:?} us",
                cal.compensation, cal.back_emf, period
            );
        }
        Ok(cal) => warn!("  calibration did not pass: status {:#04x}", cal.status),
        Err(err) => error!("  calibration failed on the bus: {err:?}"),
    }
    let _ = haptic.set_library(&mut i2c, Library::Lra);

    loop {
        info!("--- three clicks with the line held steadily high: the reference ---");
        for _ in 0..3 {
            if let Err(err) = haptic.play(&mut i2c, CLICK, &delay) {
                error!("  {err:?}");
            }
            delay.delay_millis(400);
        }
        if !prompt.again(&mut i2c, "a hand on the knob: how strong is that click?") {
            break;
        }
    }

    // ---- 4. Under traffic ---------------------------------------------------------------
    //
    // The pin stops being an output and becomes UART1's transmit line. From here on it is
    // high whenever nothing is being sent, and chopped into bit times whenever something is.
    info!("--- 4. GPIO38 as UART1 TX at {BAUD} baud, GPIO48 listening ---");
    let uart = match Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(BAUD)) {
        Ok(uart) => uart
            .with_tx(peripherals.GPIO38.reborrow())
            .with_rx(peripherals.GPIO48.reborrow()),
        Err(err) => {
            error!("  UART1 refused {BAUD} baud: {err:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };
    let (mut rx, mut tx) = uart.split();

    // Idle first: the line is high because a UART holds it there, not because we do.
    delay.delay_millis(20);
    info!("  with the line idle in UART hands:");
    let uart_idle = diagnose_three_times(&mut haptic, &mut i2c, &delay);

    // Now the hard case: the diagnostic runs while zeros go out without a gap.
    info!("  with a solid stream of 0x00 going out underneath it:");
    let mut traffic_runs = [None; 3];
    for run in traffic_runs.iter_mut() {
        if haptic.set_mode(&mut i2c, Mode::Diagnostics).is_err() {
            break;
        }
        if haptic.go(&mut i2c).is_err() {
            break;
        }
        let sent = pump(&mut tx, PUMP_MS);
        match haptic.status(&mut i2c) {
            Ok(status) => {
                info!("    {status:#04x} after {sent} bytes");
                *run = Some(status);
            }
            Err(err) => error!("    {err:?}"),
        }
    }

    // Every one of the three has to pass: a summary that reads only the last of the three
    // can call an intermittent failure a clean pass.
    let traffic_passes = traffic_runs.iter().filter(|r| **r == Some(0xE0)).count();
    match (uart_idle, traffic_passes) {
        (Some(0xE0), 3) => info!(
            "  the driver does not notice the traffic: GPIO38 can be the serial line and the enable at once"
        ),
        (Some(0xE0), 0) => {
            warn!("  idle is fine and traffic is not: the two uses of this pin do not fit together")
        }
        (Some(0xE0), passed) => warn!(
            "  {passed} of 3 diagnostics passed under traffic: it works, but not every time -- and a\n               diagnostic that runs while the line is being chopped up is measuring the chopping too"
        ),
        (Some(other), _) => warn!("  the idle line alone already reads {other:#04x}"),
        (None, _) => error!("  no answer from the driver once the UART had the pin"),
    }

    loop {
        info!("--- the same three clicks, each under a stream of zeros ---");
        for _ in 0..3 {
            let _ = haptic.set_mode(&mut i2c, Mode::InternalTrigger);
            let _ = haptic.set_sequence(&mut i2c, &[CLICK]);
            let _ = haptic.go(&mut i2c);
            let sent = pump(&mut tx, PUMP_MS);
            info!("  click sent under {sent} bytes of traffic");
            delay.delay_millis(400);
        }
        if !prompt.again(
            &mut i2c,
            "the same hand: weaker than the reference, or the same?",
        ) {
            break;
        }
    }

    // Anything the other chip made of that. It has never said a word unprompted; a stream of
    // zeros is not a sentence in any protocol, but a chip that answers noise says more about
    // the link than a chip that answers nothing.
    let mut buf = [0u8; 64];
    match rx.read_buffered(&mut buf) {
        Ok(0) => {
            info!("GPIO48: nothing came back, which is what a chip that ignores nonsense does")
        }
        Ok(n) => info!("GPIO48: {n} bytes came back: {:02x?}", &buf[..n]),
        Err(err) => {
            info!("GPIO48: {err:?} -- a framing complaint means edges arrived from somewhere")
        }
    }

    info!("--- done. The pin stays in UART hands; reset to start over. ---");
    loop {
        delay.delay_millis(5000);
        match haptic.status(&mut i2c) {
            Ok(status) => info!("Haptic: still {status:#04x}"),
            Err(err) => warn!("Haptic: {err:?}"),
        }
    }
}

/// Pushes zeros for a while and reports how many went out.
///
/// The write blocks once the FIFO is full, which is the point: the line stays busy for the
/// whole window instead of emptying between calls.
fn pump(tx: &mut esp_hal::uart::UartTx<'_, Blocking>, millis: u64) -> u32 {
    let start = Instant::now();
    let mut sent = 0u32;
    while start.elapsed() < Duration::from_millis(millis) {
        match tx.write(&ZEROS) {
            Ok(n) => sent += n as u32,
            Err(_) => break,
        }
    }
    let _ = tx.flush();
    sent
}

/// Runs the actuator diagnostic three times and returns the status if all three agreed.
///
/// Once is an anecdote: the diagnostic drives the actuator and reads what comes back, and a
/// single run that disagrees with its neighbours is worth seeing rather than averaging away.
fn diagnose_three_times(
    haptic: &mut Haptic<'_>,
    i2c: &mut I2c<'_, Blocking>,
    delay: &Delay,
) -> Option<u8> {
    let mut first = None;
    let mut agreed = true;
    for _ in 0..3 {
        match haptic.diagnose(i2c, delay) {
            Ok(status) => {
                info!("    diagnostic {status:#04x}");
                match first {
                    None => first = Some(status),
                    Some(seen) if seen != status => agreed = false,
                    _ => {}
                }
            }
            Err(err) => {
                error!("    {err:?}");
                return None;
            }
        }
        delay.delay_millis(50);
    }
    if !agreed {
        warn!("    the three runs did not agree with each other");
    }
    first
}

/// Whether anything acknowledges its address on the bus.
fn answers(i2c: &mut I2c<'_, Blocking>, address: u8) -> bool {
    let mut byte = [0u8; 1];
    i2c.write_read(address, &[0x00], &mut byte).is_ok()
}
