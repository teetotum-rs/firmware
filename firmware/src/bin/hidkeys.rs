//! Every HID usage the other chip can send, one at a time, judged at the phone.
//!
//! The other chip takes the usage from `data[0]` alone, ignores `data[1..3]`, and always sends a
//! press, one tick, then a release `[00 00]`. A key cannot be left pressed by anything in our
//! frame. This run takes the face and the loader out of the picture entirely: one `A3 04` per
//! step, sent by hand, with the phone in the other hand -- so what is left to find out is what
//! the phone does with each usage, and whether a second press of the same one does anything.
//!
//! **Pair the phone with `TAIJI_KNOB_HID`** before starting; `A3 04` goes out over the other
//! chip's BLE HID link and vanishes without a word when nothing is connected to it.
//!
//! **Turning the knob only selects** the next usage, and after the last one the list starts
//! over; **a tap sends** the selected one, as often as wanted. Enter and `r` in the monitor do
//! the same, `q` ends the run. A hand that turns one detent too far just turns on until the
//! usage comes round again -- nothing is sent on the way. The second encoder sits on the same
//! shaft, so it is switched off here -- otherwise every step forward would also move the phone's
//! volume over AVRCP. Since no key is needed, the run can be logged:
//! `cargo run --release --bin hidkeys 2>&1 | tee /tmp/hidkeys.log`.
//!
//! **Power is last**, because it may lock the phone.

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
use log::{error, info};
use teetotum::companion::{BAUD, Companion, Event, MediaKey, Mode};
use teetotum::encoder::Encoder;
use teetotum::step::{Prompt, Step};
use teetotum::touch::Touch;

/// The ten usages `0x400da420` maps, in the order a hand at a player would try them.
const KEYS: [(MediaKey, &str); 10] = [
    (MediaKey::PlayPause, "PlayPause"),
    (MediaKey::Next, "Next"),
    (MediaKey::Previous, "Previous"),
    (MediaKey::Play, "Play"),
    (MediaKey::Pause, "Pause"),
    (MediaKey::Stop, "Stop"),
    (MediaKey::FastForward, "FastForward"),
    (MediaKey::Rewind, "Rewind"),
    (MediaKey::Record, "Record"),
    (MediaKey::Power, "Power"),
];

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let delay = Delay::new();

    // The monitor needs a moment after a reset before it is attached to anything.
    delay.delay_millis(500);

    let uart = match Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(BAUD)) {
        Ok(uart) => uart
            .with_tx(peripherals.GPIO40.reborrow())
            .with_rx(peripherals.GPIO39.reborrow()),
        Err(e) => {
            error!("UART1 refused {BAUD} baud: {e:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };
    let (rx, tx) = uart.split();
    let mut link = Companion::new(rx, tx);

    // A slave left mid-transfer by the last binary holds the bus and answers to the wrong
    // address; sixteen clocks and a stop cost nothing. The reasoning is in `src/bin/haptic.rs`.
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

    // Whatever the other chip was in the middle of saying when we booted.
    drain_for(&mut link, Duration::from_millis(300));
    link.resync();
    link.set_mode(Mode::Idle, false);
    link.request_status();
    drain_for(&mut link, Duration::from_millis(300));

    // A turn only selects and a tap sends, and the list wraps: nothing is sent while turning,
    // only on a tap, so overshooting the wanted usage costs nothing.
    let mut sent = [0u32; KEYS.len()];
    let mut last: [Option<Instant>; KEYS.len()] = [None; KEYS.len()];
    let mut n = 0;
    let mut shown = None;
    while prompt.is_attended() {
        let (key, name) = KEYS[n];
        if shown != Some(n) {
            info!("");
            info!(
                "--- {}/{}: {name} ({:#04x}), sent {} so far ---",
                n + 1,
                KEYS.len(),
                key as u8,
                sent[n]
            );
            shown = Some(n);
        }
        let what = if key == MediaKey::Power {
            "tap to send Power (it may lock the phone), turn for the next usage"
        } else {
            "tap to send it, turn for the next usage"
        };
        match waiting(&mut prompt, &mut i2c, &mut link, what) {
            Step::Repeat => {
                link.media_key(key);
                sent[n] += 1;
                let now = Instant::now();
                match last[n] {
                    Some(before) => info!(
                        "  sent A3 04 04 00 {:02X} 00 00 00, press {}, {} ms after the last",
                        key as u8,
                        sent[n],
                        (now - before).as_millis()
                    ),
                    None => info!("  sent A3 04 04 00 {:02X} 00 00 00, press 1", key as u8),
                }
                last[n] = Some(now);
            }
            Step::Next => n = (n + 1) % KEYS.len(),
            Step::Quit => break,
        }
    }

    info!("");
    for (&(_, name), &count) in KEYS.iter().zip(sent.iter()) {
        info!("  {name}: {count} press(es)");
    }
    info!("--- done; listening to the other chip from now on ---");
    loop {
        drain_for(&mut link, Duration::from_secs(5));
    }
}

/// Waits for the hand without letting the line run dry, and says what it asked for.
///
/// The other chip talks during the pause -- a `BD 06` would say it also has an AVRCP link,
/// which changes what a key can reach -- so the wait drains the wire and polls the inputs in
/// one loop, as `src/bin/ec2.rs` does.
fn waiting(
    prompt: &mut Prompt<'_>,
    i2c: &mut I2c<'_, Blocking>,
    link: &mut Companion<'_>,
    what: &str,
) -> Step {
    prompt.announce(what);
    loop {
        drain(link);
        if let Some(step) = prompt.poll(i2c) {
            return step;
        }
    }
}

/// Logs everything the other chip has said since the last call.
fn drain(link: &mut Companion<'_>) {
    while let Some(event) = link.poll() {
        match event {
            Event::Metadata => {
                let m = link.metadata();
                info!("  BD 06: \"{}\" by {}", m.title(), m.artist());
            }
            other => info!("  {other:?}"),
        }
    }
}

fn drain_for(link: &mut Companion<'_>, how_long: Duration) {
    let until = Instant::now() + how_long;
    while Instant::now() < until {
        drain(link);
    }
}
