//! The other chip as a device we use, rather than one we are still measuring.
//!
//! `src/bin/uarttalk.rs` and `src/bin/ec2.rs` worked out the link itself. This one is the result:
//! it opens the link through [`teetotum::companion`], takes the second encoder for itself, and
//! drives the phone from the screen. Nothing here decodes a frame -- that is the driver's job now.
//!
//! What it does:
//!
//! * asks for the second encoder in [`Mode::Events`] and prints every detent, with a running
//!   position so that a turn can be counted against the fingers,
//! * polls the status once a second and reports it when it changes, which is how the phone's
//!   volume becomes visible,
//! * prints the title, artist and album of every track change as the other chip pushes it,
//! * turns a **tap** on the screen into play/pause and a **swipe left** / **swipe right** into
//!   the next and previous track -- as the finger sees it, which is the opposite of what the
//!   controller calls it, and acted on when the finger lifts, because the controller names a
//!   slide while the finger is still down,
//! * and puts the two other paths on the two other swipes, so that the three can be told apart
//!   by hand: **down** sends the same "next track" as a `A3 04` HID report, which goes out over
//!   **BLE HID** and reaches nobody unless something is paired with `TAIJI_KNOB_HID`, and
//!   **up** sends `A3 12`, which suspends the audio stream itself rather than pressing a key.
//!
//! The left/right swipes are the ones the factory firmware cannot swallow: its dispatcher puts
//! no guard and no state test in front of `A3 03` with 3 or 4, so each one reaches
//! `esp_avrc_ct_send_passthrough_cmd` every time. **If the track changes, the other chip says so
//! unprompted** -- a `BD 06` with the new title -- so this run proves itself in the monitor
//! without a look at the phone.
//!
//! It expects the phone to be paired with the classic ESP32 and playing; without that, the
//! encoder still reports and the status still answers, and everything about the music stays
//! empty. Run it in the monitor, because it is meant to be touched:
//! `cargo run --release --bin companion`.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart};
use teetotum::companion::{BAUD, Companion, Direction, Event, MediaKey, Mode, QueueKey, Status};

/// What a swipe does. Four gestures, three different ways to reach the music -- genuinely
/// different paths, not three guesses at one.
#[derive(Clone, Copy, Debug)]
enum Action {
    /// `A3 03`, into the other chip's own dispatcher, which sends an **AVRCP passthrough key**
    /// over the link the phone is already using. [`QueueKey::Next`] and [`QueueKey::Previous`]
    /// pass no guard and no state test in the factory firmware, so this is the one command in
    /// the run that the chip cannot swallow silently.
    Queue(QueueKey),
    /// `A3 04`, a HID consumer usage id -- which goes out as a **BLE HID report** and not over
    /// AVRCP at all. It reaches the phone only if something is connected to `TAIJI_KNOB_HID`.
    /// Kept in the run as the contrast: if this stays dead while the swipe next to it works, the
    /// two paths are told apart at the screen.
    Key(MediaKey),
    /// `A3 03` with 5: play/pause, and the one code in the set the other chip may decide to
    /// swallow -- see [`QueueKey::PlayPause`]. This is what a tap sends.
    TogglePlayback,
    /// `A3 12`, `esp_a2d_media_ctrl(SUSPEND)`: not a key at all, but flow control on the audio
    /// stream itself. If this stops the sound while a key does nothing, the fault is above the
    /// stream and not on our side of it.
    SuspendStream,
}
use log::{error, info, warn};
use teetotum::touch::{Gesture, Taps, Touch};

/// How often the volume is asked for. Nothing pushes it: it changes at the phone.
const STATUS_PERIOD: Duration = Duration::from_secs(1);

/// How often the screen is sampled. Fast enough that no contact is missed between two reads,
/// slow enough that the bus is not the only thing this loop does.
const TOUCH_PERIOD: Duration = Duration::from_millis(20);

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
    let mut companion = Companion::new(rx, tx);

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

    let mut touch = Touch::new(
        Output::new(
            peripherals.GPIO10.reborrow(),
            Level::High,
            OutputConfig::default(),
        ),
        Input::new(
            peripherals.GPIO9.reborrow(),
            InputConfig::default().with_pull(Pull::Up),
        ),
        &delay,
    );
    match touch.chip_id(&mut i2c) {
        Ok(id) => info!("touch controller answers with id {id:#04x} (0xB6 is the CST816D here)"),
        Err(e) => warn!("touch controller does not answer: {e:?} -- the screen will do nothing"),
    }

    // Whatever the other chip was in the middle of saying when we booted.
    companion.resync();

    info!("");
    info!("tap the screen to play or pause, swipe left for the next track, right for the previous");
    info!("turn the knob: every detent should arrive as an event of its own");
    info!("");

    companion.set_mode(Mode::Events, true);
    companion.request_status();

    let mut position: i32 = 0;
    // One gesture per contact; the rule lives in `touch::Taps`.
    let mut taps = Taps::new();
    let mut next_touch_read = Instant::now();
    let mut last_status: Option<Status> = None;
    let mut next_probe = Instant::now() + STATUS_PERIOD;

    loop {
        while let Some(event) = companion.poll() {
            match event {
                Event::Encoder(direction) => {
                    // The raw command byte stays in the line: a direction name is an
                    // interpretation, the byte is what arrived.
                    let (step, frame) = match direction {
                        Direction::Clockwise => (1, 7),
                        Direction::Anticlockwise => (-1, 8),
                    };
                    position += step;
                    info!("knob {direction:?} (BD {frame:02}), position {position}");
                }
                Event::Status(status) => {
                    if Some(status) != last_status {
                        last_status = Some(status);
                        info!(
                            "status: volume {}, encoder {}, mode {:?}",
                            status.volume,
                            if status.encoder_enabled() {
                                "on"
                            } else {
                                "OFF"
                            },
                            status.mode(),
                        );
                    }
                }
                Event::Metadata => {
                    let meta = companion.metadata();
                    info!(
                        "now playing: {} -- {} ({})",
                        meta.title(),
                        meta.artist(),
                        meta.album()
                    );
                }
                other => info!("{other:?}"),
            }
        }

        let now = Instant::now();
        if now >= next_touch_read {
            next_touch_read = now + TOUCH_PERIOD;
            // The interrupt line is deliberately not consulted; the reasoning is in
            // `src/step.rs`, which lost taps to it. One bus transaction every twenty
            // milliseconds buys a tap that always counts.
            if let Ok(report) = touch.read(&mut i2c) {
                // One answer per contact, on the lift: `Taps` holds the rule, because a swipe
                // is named while the finger is still down and a tap only after it has gone.
                //
                // The names below are the hand's, with the device held **USB pointing away** --
                // the orientation this project works in. `in_picture_mount` is what makes them
                // so: touch reports in the mounting frame and this panel sits half a turn
                // round, so the controller's own `SlideDown` is a swipe *towards* the USB port.
                // A direction belongs in the frame the reader is standing in, not the frame the
                // touch controller reports.
                let action = match taps.feed(&report).map(Gesture::in_picture_mount) {
                    Some(Gesture::SlideLeft) => Some(Action::Queue(QueueKey::Next)),
                    Some(Gesture::SlideRight) => Some(Action::Queue(QueueKey::Previous)),
                    Some(Gesture::SlideUp) => Some(Action::Key(MediaKey::Next)),
                    Some(Gesture::SlideDown) => Some(Action::SuspendStream),
                    Some(Gesture::SingleTap) => Some(Action::TogglePlayback),
                    _ => None,
                };
                match action {
                    Some(Action::Queue(key)) => {
                        info!("swipe -> {key:?} (A3 03, {})", key as u8);
                        companion.queue_key(key);
                    }
                    Some(Action::Key(key)) => {
                        info!("swipe up -> {key:?} (A3 04, {:#04x}, BLE HID)", key as u8);
                        companion.media_key(key);
                    }
                    Some(Action::SuspendStream) => {
                        info!("swipe down -> suspend the stream (A3 12)");
                        companion.stream_suspend();
                    }
                    Some(Action::TogglePlayback) => {
                        info!("tap -> play/pause (A3 03, 5)");
                        companion.toggle_playback();
                    }
                    None => {}
                }
            }
        }

        if now >= next_probe {
            next_probe = now + STATUS_PERIOD;
            companion.request_status();
        }
    }
}
