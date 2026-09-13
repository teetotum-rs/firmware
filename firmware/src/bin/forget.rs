//! Makes the other chip forget its phone and its BLE bonds, after a hand has said so.
//!
//! This run sends the factory interface's "forget everything": `A3 06` (BLE bonds) and `A3 07`
//! (stored peer, `PEERADDR`). It does **not** fix a phone that lists the board as one merged
//! `TAIJI_KNOB_AUDIO` device: the other chip serves A2DP and BLE HID from the same address with
//! an audio Class of Device, so a host keeps both roles under one entry whatever is bonded.
//!
//! **Both are one-way.** Pairing again needs the phone and a hand, so nothing is sent until the
//! knob turns one detent; a tap, `r` or `q` leaves the chip as it is, and so does a run without
//! a monitor attached. Run it in a terminal:
//! `cargo run --release --bin forget 2>&1 | tee /tmp/forget.log`.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::time::{Duration, Instant, Rate};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info};
use teetotum::companion::{BAUD, Companion, Event};
use teetotum::encoder::Encoder;
use teetotum::step::{Prompt, Step};
use teetotum::touch::Touch;

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
            Output::new(peripherals.GPIO10.reborrow(), Level::High, OutputConfig::default()),
            Input::new(peripherals.GPIO9.reborrow(), pull_up),
        ))
        .with_keys(UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow()).split().0);

    drain_for(&mut link, Duration::from_millis(300));
    link.resync();
    link.request_status();
    drain_for(&mut link, Duration::from_millis(300));

    let go = prompt.is_attended() && {
        prompt.announce(
            "turn one detent to make the other chip forget its phone and BLE bonds; \
             tap or q to leave it as it is",
        );
        loop {
            drain(&mut link);
            if let Some(step) = prompt.poll(&mut i2c) {
                break step == Step::Next;
            }
        }
    };

    if go {
        // Spaced, because it is unread whether these two share the task notification that
        // lets a second `A3 03` overwrite the first.
        info!("sending A3 06: clear BLE bonds");
        link.clear_ble_bonds();
        drain_for(&mut link, Duration::from_millis(500));
        info!("sending A3 07: forget the stored peer");
        link.forget_peer();
        drain_for(&mut link, Duration::from_millis(500));
        link.request_status();
        info!("sent both; now forget TAIJI_KNOB_AUDIO on the phone and pair TAIJI_KNOB_HID alone");
    } else {
        info!("nothing sent; the other chip keeps its pairings");
    }

    info!("--- listening to the other chip from now on ---");
    loop {
        drain_for(&mut link, Duration::from_secs(5));
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
