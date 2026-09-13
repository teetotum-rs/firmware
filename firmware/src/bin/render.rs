//! What a frame costs, measured rather than estimated.
//!
//! Everything drawn on this panel so far was a single colour streamed at it. This is the first
//! run that assembles a picture in memory and shows it, and its purpose is to put numbers on
//! the three things that decide whether a user interface is possible here at all:
//!
//! 1. **Does the PSRAM come up, and how fast is it to write?** The framebuffer is 253 KiB and
//!    lives there; clearing it is a pure write of that size and is timed on its own.
//! 2. **What does drawing cost?** A scene with text, rings and tick marks, drawn through
//!    `embedded-graphics` into external RAM.
//! 3. **What does showing it cost, and where is the ceiling?** The same frame is sent at
//!    10, 20, 40 and 80 MHz. Every earlier run used 10 MHz because that was the first value
//!    that worked, and nobody has since asked what else does. A frame at 10 MHz cannot be
//!    faster than 52 ms by arithmetic alone -- 259200 bytes over four data lines -- so if the
//!    panel takes a higher clock, that is the largest single lever there is.
//!
//! The picture is deliberately made of the things rotation is hard on: small text, a thin
//! ring, and radial marks at every 30 degrees. When the rotating blit arrives, this is the
//! scene to judge it by, and the numbers here are what it will be compared against.
//!
//! **It waits for a hand at every step**, because the result of each one is on the glass and
//! not in the log. Swipe or turn the knob for the next step, tap to repeat one, `q` to let it
//! run to the end unattended. Run it in the monitor:
//! `cargo run --release --bin render`.

#![no_std]
#![no_main]

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, Line, PrimitiveStyle};
use embedded_graphics::text::{Alignment, Text};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::psram::{Psram, PsramConfig, PsramMode};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::{Instant, Rate};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info, warn};
use st77916::{ColorMode, DisplaySize, St77916};
use teetotum::display::{DisplayBus, DisplayReset};
use teetotum::framebuffer::{BYTES, Framebuffer, HEIGHT, WIDTH};
use teetotum::panel::{INIT_COMMANDS, POST_INIT_COMMANDS};
use teetotum::rotate::{Filter, STEPS, rotate_rows};
use teetotum::step::Prompt;
use teetotum::touch::Touch;

/// Bytes staged for one DMA transfer, and therefore the size of the bus's own buffer.
///
/// Thirty rows, a value `src/bin/chunktest.rs` established as safe. A frame is twelve of these.
const SPI_CHUNK: usize = WIDTH * 2 * 30;

/// The clocks the panel is asked to take, in the order they are tried.
///
/// 10 MHz is what every run so far has used. The others are the question.
const CLOCKS: [u32; 4] = [10, 20, 40, 80];

/// How many frames each clock is timed over, so that one unlucky transfer does not become the
/// measurement.
const FRAMES_PER_CLOCK: u32 = 10;

/// The clock the turned frames are sent at, so that the turning is what is being compared and
/// not the bus.
const BLIT_CLOCK: u32 = 80;

/// Rows turned into the staging buffer before it is pushed.
const ROWS_PER_BAND: usize = 30;

/// Where a band of turned rows is assembled.
///
/// It has to be in internal RAM: it is what the SPI bus copies from, and the whole reason the
/// picture is turned a band at a time is that a second 253 KiB framebuffer would not fit there.
static mut STAGING: [u8; SPI_CHUNK] = [0; SPI_CHUNK];

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let mut peripherals = esp_hal::init(config);
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);
    let mut delay = Delay::new();

    // The monitor needs a moment after a reset before it is attached to anything.
    delay.delay_millis(500);
    info!("--- render: what a frame costs ---");

    // The PSRAM is Octal, and that is read rather than guessed: the factory image on this chip
    // carries the string `octal_psram`, which is the log tag of ESP-IDF's octal implementation,
    // and does not carry `quad_psram`. Naming the mode rather than letting the driver probe is
    // also what esp-hal recommends -- detection works, but not reliably.
    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            ..Default::default()
        },
    );
    let (psram_start, psram_size) = psram.raw_parts();
    info!("PSRAM: {} KiB mapped at {psram_start:p}", psram_size / 1024);
    if psram_size < BYTES {
        error!("PSRAM: {psram_size} bytes is not enough for a {BYTES}-byte frame -- stopping");
        loop {
            delay.delay_millis(1000);
        }
    }

    // SAFETY: the PSRAM is mapped and nothing else has been handed a pointer into it; this
    // slice is made once and moved into the framebuffer, which owns it from here on.
    let memory: &'static mut [u8] =
        unsafe { core::slice::from_raw_parts_mut(psram_start, psram_size) };
    let Some(mut frame) = Framebuffer::new(memory) else {
        error!("the framebuffer did not fit -- stopping");
        loop {
            delay.delay_millis(1000);
        }
    };

    let _backlight = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(1, SPI_CHUNK);
    let dma_rx =
        DmaRxBuf::new(rx_descriptors, rx_buffer).expect("the DMA read buffer is malformed");
    let dma_tx =
        DmaTxBuf::new(tx_descriptors, tx_buffer).expect("the DMA write buffer is malformed");

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default().with_frequency(Rate::from_mhz(CLOCKS[0])),
    )
    .expect("the display SPI peripheral could not be configured")
    .with_sck(peripherals.GPIO13)
    .with_sio0(peripherals.GPIO15)
    .with_sio1(peripherals.GPIO16)
    .with_sio2(peripherals.GPIO17)
    .with_sio3(peripherals.GPIO18)
    .with_dma(peripherals.DMA_CH0)
    .with_buffers(dma_rx, dma_tx);

    let reset = DisplayReset {
        pin: Output::new(peripherals.GPIO21, Level::High, OutputConfig::default()),
        delay,
    };
    let bus = DisplayBus::new(
        spi,
        Output::new(peripherals.GPIO14, Level::High, OutputConfig::default()),
    );

    let mut display =
        match St77916::builder(bus, reset, DisplaySize::new(WIDTH as u16, HEIGHT as u16))
            .with_init_commands(INIT_COMMANDS)
            .build(ColorMode::Rgb565, &mut delay)
        {
            Ok(display) => display,
            Err(err) => {
                error!("display initialisation failed: {err:?} -- stopping");
                loop {
                    delay.delay_millis(1000);
                }
            }
        };
    delay.delay_millis(150);
    for &(cmd, data, wait) in POST_INIT_COMMANDS {
        if let Err(err) = display.send_command_with_data(cmd, data) {
            error!("post-init command {cmd:#04x} failed: {err:?}");
        }
        delay.delay_millis(u32::from(wait));
    }
    if let Err(err) = display.set_window(0, 0, WIDTH as u16 - 1, HEIGHT as u16 - 1) {
        error!("could not set the window: {err:?}");
    }
    info!("display: initialised, window is the whole panel");

    // Nothing has written to the panel's own memory since it powered up, and what is in it is
    // whatever it is -- on this panel, white and coloured noise. That was taken for our own
    // first frame once, during the ninety seconds this run spends waiting for a hand, so the
    // glass is blacked out before anything is asked of anybody.
    {
        // SAFETY: single-threaded, and the staging buffer is still the zeroes it started as.
        let black: &[u8; SPI_CHUNK] = unsafe { &*core::ptr::addr_of!(STAGING) };
        let bus = display.interface_mut();
        if let Err(err) = bus.fill_repeating(black, HEIGHT / ROWS_PER_BAND) {
            error!("could not black out the panel: {err:?}");
        }
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
    if let Err(e) = touch.chip_id(&mut i2c) {
        warn!("touch controller does not answer: {e:?} -- steps cannot be repeated by hand");
    }
    let mut prompt = Prompt::new().with_touch(touch).with_keys(
        UsbSerialJtag::new(peripherals.USB_DEVICE.reborrow())
            .split()
            .0,
    );

    // --- 1. what the external RAM costs to write ---
    let started = Instant::now();
    let _ = frame.clear(Rgb565::BLACK);
    let clear = started.elapsed();
    info!(
        "clear: {} KiB of PSRAM written in {} us",
        BYTES / 1024,
        clear.as_micros()
    );

    // --- 2. what drawing costs ---
    let started = Instant::now();
    draw_scene(&mut frame);
    let drawn = started.elapsed();
    info!("draw: the scene took {} us", drawn.as_micros());

    // --- 3. what showing it costs, at four clocks ---
    for clock in CLOCKS {
        prompt.wait(
            &mut i2c,
            "look at the glass while the frame is sent at the next clock",
        );
        loop {
            let bus = display.interface_mut();
            let clock_config = SpiConfig::default().with_frequency(Rate::from_mhz(clock));
            if let Err(err) = bus.apply_config(&clock_config) {
                error!("{clock} MHz refused by the SPI peripheral: {err:?}");
                break;
            }

            let started = Instant::now();
            let mut failed = false;
            for _ in 0..FRAMES_PER_CLOCK {
                if let Err(err) = bus.send_frame(frame.bytes(), SPI_CHUNK) {
                    error!("{clock} MHz: sending the frame failed: {err:?}");
                    failed = true;
                    break;
                }
            }
            let total = started.elapsed();
            if failed {
                break;
            }
            let per_frame = total.as_micros() / u64::from(FRAMES_PER_CLOCK);
            info!(
                "blit at {clock:2} MHz: {per_frame} us per frame, {} per second",
                1_000_000_u64.checked_div(per_frame).unwrap_or(0)
            );

            if !prompt.again(&mut i2c, "tap to send it again at this clock") {
                break;
            }
        }
    }

    // --- 4. what turning the picture costs ---
    //
    // Each case is announced, then shown: the CPU cost of turning alone, and the cost of
    // turning and sending together, at a clock that is no longer the bottleneck.
    let bus = display.interface_mut();
    if let Err(err) =
        bus.apply_config(&SpiConfig::default().with_frequency(Rate::from_mhz(BLIT_CLOCK)))
    {
        error!("{BLIT_CLOCK} MHz refused by the SPI peripheral: {err:?}");
    }

    // SAFETY: single-threaded, and nothing else refers to the staging buffer.
    let staging: &mut [u8; SPI_CHUNK] = unsafe { &mut *core::ptr::addr_of_mut!(STAGING) };
    let bands = HEIGHT / ROWS_PER_BAND;

    for (step, filter, what) in [
        (
            3,
            Filter::Nearest,
            "a quarter turn, nearest: this one has to be exact",
        ),
        (
            1,
            Filter::Nearest,
            "30 degrees, nearest: look at the thin ring and the small text",
        ),
        (
            1,
            Filter::Bilinear,
            "30 degrees, bilinear: the same picture, four samples per pixel",
        ),
    ] {
        prompt.wait(&mut i2c, what);
        loop {
            let started = Instant::now();
            for band in 0..bands {
                rotate_rows(
                    &frame,
                    step,
                    filter,
                    band * ROWS_PER_BAND,
                    ROWS_PER_BAND,
                    staging,
                );
            }
            let turning = started.elapsed();

            let started = Instant::now();
            let bus = display.interface_mut();
            bus.pixels_begin();
            let mut failed = false;
            for band in 0..bands {
                rotate_rows(
                    &frame,
                    step,
                    filter,
                    band * ROWS_PER_BAND,
                    ROWS_PER_BAND,
                    staging,
                );
                if let Err(err) = bus.pixels_push(staging) {
                    error!("pushing a turned band failed: {err:?}");
                    failed = true;
                    break;
                }
            }
            bus.pixels_end();
            let whole = started.elapsed();

            if !failed {
                info!(
                    "turn {:3} deg {:8}: {} us to turn, {} us turned and sent, {} per second",
                    step * 30,
                    if filter == Filter::Nearest {
                        "nearest"
                    } else {
                        "bilinear"
                    },
                    turning.as_micros(),
                    whole.as_micros(),
                    if whole.as_micros() == 0 {
                        0
                    } else {
                        1_000_000 / whole.as_micros()
                    }
                );
            }

            if !prompt.again(&mut i2c, "tap to draw this one again") {
                break;
            }
        }
    }

    // --- 5. all twelve orientations, one after the other ---
    //
    // The point of the whole exercise: the user turns the knob and the picture follows, in the
    // steps the setting will offer. What it shows by eye is whether the long mark still points
    // where it should after twelve steps, which is the arithmetic checking itself.
    prompt.wait(
        &mut i2c,
        "watch the picture go round once, 30 degrees at a time",
    );
    let started = Instant::now();
    for step in 0..STEPS {
        let bus = display.interface_mut();
        bus.pixels_begin();
        for band in 0..bands {
            rotate_rows(
                &frame,
                step,
                Filter::Bilinear,
                band * ROWS_PER_BAND,
                ROWS_PER_BAND,
                staging,
            );
            if bus.pixels_push(staging).is_err() {
                break;
            }
        }
        bus.pixels_end();
        delay.delay_millis(250);
    }
    let spin = started.elapsed();
    info!(
        "one full turn in twelve steps took {} ms, of which {} ms was waiting",
        spin.as_millis(),
        250 * STEPS as u64
    );

    info!("--- render: done. The last frame stays on the glass. ---");
    loop {
        delay.delay_millis(1000);
    }
}

/// A scene made of what rotation is hard on.
///
/// Small text loses its stems to a nearest-neighbour sample, a one-pixel ring turns into a
/// dotted line, and radial marks at 30 degrees are exactly the steps the orientation setting is
/// meant to offer -- so a mark that lands on the vertical after a 30-degree turn is the
/// rotation working, visible without a measurement.
fn draw_scene(frame: &mut Framebuffer) {
    let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
    let white = PrimitiveStyle::with_stroke(Rgb565::WHITE, 1);
    let dim = PrimitiveStyle::with_stroke(Rgb565::CSS_DIM_GRAY, 1);

    // The glass is round: a ring just inside the bezel says where the picture actually ends.
    let _ = Circle::with_center(centre, 356)
        .into_styled(dim)
        .draw(frame);
    let _ = Circle::with_center(centre, 240)
        .into_styled(white)
        .draw(frame);

    // Twelve marks, one every 30 degrees -- the steps the orientation setting will offer.
    // Sine and cosine from a small table, because this runs on a chip without an FPU worth
    // calling for twelve values.
    const SIN30: [(i32, i32); 12] = [
        (0, -1000),
        (500, -866),
        (866, -500),
        (1000, 0),
        (866, 500),
        (500, 866),
        (0, 1000),
        (-500, 866),
        (-866, 500),
        (-1000, 0),
        (-866, -500),
        (-500, -866),
    ];
    for (index, (sx, sy)) in SIN30.into_iter().enumerate() {
        let inner = 150;
        let outer = if index == 0 { 100 } else { 165 };
        let from = Point::new(centre.x + sx * inner / 1000, centre.y + sy * inner / 1000);
        let to = Point::new(centre.x + sx * outer / 1000, centre.y + sy * outer / 1000);
        let _ = Line::new(from, to)
            .into_styled(if index == 0 {
                PrimitiveStyle::with_stroke(Rgb565::CSS_ORANGE, 3)
            } else {
                white
            })
            .draw(frame);
    }

    let big = MonoTextStyle::new(&FONT_10X20, Rgb565::WHITE);
    let small = MonoTextStyle::new(&FONT_6X10, Rgb565::CSS_LIGHT_GRAY);
    let _ = Text::with_alignment("TeeToTum", centre, big, Alignment::Center).draw(frame);
    let _ = Text::with_alignment(
        "six by ten, the size rotation ruins first",
        Point::new(centre.x, centre.y + 24),
        small,
        Alignment::Center,
    )
    .draw(frame);
    let _ = Text::with_alignment(
        "the long mark is up",
        Point::new(centre.x, centre.y - 30),
        small,
        Alignment::Center,
    )
    .draw(frame);
}
