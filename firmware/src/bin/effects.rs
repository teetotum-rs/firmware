//! One effect per detent: the ROM library under a thumb.
//!
//! The haptic driver carries 123 waveforms in ROM, and their names in the datasheet ("sharp
//! tick", "pulsing strong", "transition ramp down long smooth") are words for something only
//! fingers can check. Playing them in a fixed round, as `src/bin/haptic.rs` does, produces a
//! blur: by the time a buzz has registered, the log line naming it has scrolled away.
//!
//! So this hands the round over. **One step of the knob is one effect**, played once, with its
//! number drawn large enough on the screen to read at arm's length; a tap on the screen plays the
//! current one again. Nothing needs to be read in a terminal, and the thing being measured --
//! whether effect 24 feels like a tick and effect 47 like a buzz -- is measured where it lives.
//!
//! What it configures, from the measurements in `docs/hardware/haptics.md`: the actuator is an
//! LRA resonating at about 161 Hz, the enable line is GPIO38, and the device reset must not be
//! written. The chip is auto-calibrated at start-up, which is felt as a short growl before the
//! first effect.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use log::{error, info};
use st77916::{ColorMode, DisplaySize, St77916};
use teetotum::display::{DisplayBus, DisplayReset};
use teetotum::encoder::Encoder;
use teetotum::haptic::{Actuator, CalTime, Haptic, Library};
use teetotum::panel::{INIT_COMMANDS, POST_INIT_COMMANDS};
use teetotum::touch::{Event, Gesture, Touch};

/// The panel is 360x360, and the visible screen is a circle inside it.
const PANEL_WIDTH: u16 = 360;
const PANEL_HEIGHT: u16 = 360;
const DISPLAY_SIZE: DisplaySize = DisplaySize::new(PANEL_WIDTH, PANEL_HEIGHT);

/// How many effects the ROM holds. Effect 0 is silence and is not worth a detent.
const EFFECT_COUNT: u8 = 123;

/// Background, RGB565.
const BACKGROUND: u16 = 0x0009;
/// The digits.
const DIGIT: u16 = 0xFFFF;
/// The bar that shows how far through the library the knob has come.
const BAR: u16 = 0x049F;

/// Geometry of one seven-segment digit, in pixels.
const DIGIT_WIDTH: u16 = 62;
const DIGIT_HEIGHT: u16 = 120;
const STROKE: u16 = 14;
/// Space between digits.
const DIGIT_GAP: u16 = 18;
/// Where the three digits start, so that they sit centred on the screen.
const DIGITS_LEFT: u16 = (PANEL_WIDTH - (3 * DIGIT_WIDTH + 2 * DIGIT_GAP)) / 2;
const DIGITS_TOP: u16 = (PANEL_HEIGHT - DIGIT_HEIGHT) / 2;

/// Which of the seven segments each digit lights, in the order a, b, c, d, e, f, g.
const SEGMENTS: [[bool; 7]; 10] = [
    [true, true, true, true, true, true, false],     // 0
    [false, true, true, false, false, false, false], // 1
    [true, true, false, true, true, false, true],    // 2
    [true, true, true, true, false, false, true],    // 3
    [false, true, true, false, false, true, true],   // 4
    [true, false, true, true, false, true, true],    // 5
    [true, false, true, true, true, true, true],     // 6
    [true, true, true, false, false, false, false],  // 7
    [true, true, true, true, true, true, true],      // 8
    [true, true, true, true, false, true, true],     // 9
];

/// How often the knob and the screen are asked, in milliseconds.
const POLL_MS: u32 = 5;

/// Staging buffer for rectangle fills, in static memory.
const BUFFER_BYTES: usize = 21600;
static mut BUFFER: [u8; BUFFER_BYTES] = [0; BUFFER_BYTES];

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(size: 32 * 1024);

    let mut peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let _backlight = Output::new(peripherals.GPIO47, Level::High, OutputConfig::default());

    let mut delay = Delay::new();

    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(1, BUFFER_BYTES);
    let dma_rx =
        DmaRxBuf::new(rx_descriptors, rx_buffer).expect("the DMA read buffer is malformed");
    let dma_tx =
        DmaTxBuf::new(tx_descriptors, tx_buffer).expect("the DMA write buffer is malformed");

    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default().with_frequency(Rate::from_mhz(10)),
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

    let mut display = match St77916::builder(bus, reset, DISPLAY_SIZE)
        .with_init_commands(INIT_COMMANDS)
        .build(ColorMode::Rgb565, &mut delay)
    {
        Ok(display) => display,
        Err(err) => {
            error!("Display: initialisation failed: {err:?}");
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    delay.delay_millis(150);
    for &(cmd, data, wait) in POST_INIT_COMMANDS {
        if let Err(err) = display.send_command_with_data(cmd, data) {
            error!("Display: post-init command {cmd:#04x} failed: {err:?}");
        }
        delay.delay_millis(u32::from(wait));
    }

    // SAFETY: `main` runs once, and nothing else in this binary touches BUFFER.
    let buffer: &mut [u8; BUFFER_BYTES] = unsafe { &mut *core::ptr::addr_of_mut!(BUFFER) };
    fill_rect(
        &mut display,
        buffer,
        0,
        0,
        PANEL_WIDTH,
        PANEL_HEIGHT,
        BACKGROUND,
    );

    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11)
    .with_scl(peripherals.GPIO12);

    let mut touch = Touch::new(
        Output::new(peripherals.GPIO10, Level::High, OutputConfig::default()),
        Input::new(
            peripherals.GPIO9,
            InputConfig::default().with_pull(Pull::Up),
        ),
        &delay,
    );
    if let Err(err) = touch.enable_gestures(&mut i2c) {
        error!("Touch: the gesture registers could not be written: {err:?}");
    }

    // GPIO8 is the clockwise direction, measured against a dot on the screen.
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    // The enable line first: without it the driver talks and does nothing at all.
    let mut haptic = Haptic::new(
        Output::new(
            peripherals.GPIO38.reborrow(),
            Level::High,
            OutputConfig::default(),
        ),
        &delay,
    );
    let _ = haptic.wake(&mut i2c);
    let _ = haptic.set_actuator(&mut i2c, Actuator::Lra);
    match haptic.auto_calibrate(&mut i2c, &delay, CalTime::Longest) {
        Ok(result) if result.passed => info!(
            "Haptic: calibrated, compensation {:#04x}, back-EMF {:#04x}",
            result.compensation, result.back_emf
        ),
        Ok(result) => error!("Haptic: calibration failed, status {:#04x}", result.status),
        Err(err) => error!("Haptic: calibration could not be run: {err:?}"),
    }
    if let Ok(Some(period)) = haptic.lra_period_us(&mut i2c) {
        info!("Haptic: resonance {} Hz", 1_000_000 / period.max(1));
    }
    let _ = haptic.set_library(&mut i2c, Library::Lra);

    let mut effect: u8 = 1;
    draw_number(&mut display, buffer, effect);
    play(&mut haptic, &mut i2c, &delay, effect);

    info!("Effects: turn the knob for the next effect, tap the screen to feel it again");

    loop {
        let steps = encoder.poll();
        if steps != 0 {
            let moved = i32::from(effect) + steps;
            let clamped = moved.clamp(1, i32::from(EFFECT_COUNT)) as u8;
            if clamped != effect {
                effect = clamped;
                draw_number(&mut display, buffer, effect);
                play(&mut haptic, &mut i2c, &delay, effect);
            }
        }

        if touch.is_asserted()
            && let Ok(report) = touch.read(&mut i2c)
        {
            let tapped = report.gesture == Gesture::SingleTap
                || report
                    .contact
                    .is_some_and(|contact| contact.event == Event::Down);
            if tapped {
                play(&mut haptic, &mut i2c, &delay, effect);
            }
        }

        delay.delay_millis(POLL_MS);
    }
}

/// Plays one effect and says which, so that the log can be read back afterwards.
fn play(haptic: &mut Haptic<'_>, i2c: &mut I2c<'_, esp_hal::Blocking>, delay: &Delay, effect: u8) {
    info!("Effects: {effect}");
    if let Err(err) = haptic.play(i2c, effect, delay) {
        error!("Effects: effect {effect} failed: {err:?}");
    }
}

/// Draws the number as up to three digits, and the bar that shows where in the ROM it sits.
fn draw_number(
    display: &mut St77916<DisplayBus<'_>, DisplayReset<'_>>,
    buffer: &mut [u8],
    value: u8,
) {
    let digits = [value / 100, (value / 10) % 10, value % 10];
    // A leading zero is drawn as nothing at all, so that "7" is one digit and not "007".
    let first = if digits[0] > 0 {
        0
    } else if digits[1] > 0 {
        1
    } else {
        2
    };

    for (index, &digit) in digits.iter().enumerate() {
        let left = DIGITS_LEFT + index as u16 * (DIGIT_WIDTH + DIGIT_GAP);
        let lit = if index >= first {
            SEGMENTS[usize::from(digit)]
        } else {
            [false; 7]
        };
        draw_digit(display, buffer, left, DIGITS_TOP, lit);
    }

    // The bar: as wide a share of the screen as the effect is a share of the library.
    let width = u16::from(value) * 240 / u16::from(EFFECT_COUNT);
    fill_rect(display, buffer, 60, 300, 240, 10, BACKGROUND);
    if width > 0 {
        fill_rect(display, buffer, 60, 300, width, 10, BAR);
    }
}

/// Draws one seven-segment digit, lit segments in white and dark ones in the background.
///
/// The segments are the conventional a to g: top, upper right, lower right, bottom, lower left,
/// upper left, middle.
fn draw_digit(
    display: &mut St77916<DisplayBus<'_>, DisplayReset<'_>>,
    buffer: &mut [u8],
    left: u16,
    top: u16,
    lit: [bool; 7],
) {
    let half = DIGIT_HEIGHT / 2;
    let segments: [(u16, u16, u16, u16); 7] = [
        (left, top, DIGIT_WIDTH, STROKE),
        (left + DIGIT_WIDTH - STROKE, top, STROKE, half),
        (left + DIGIT_WIDTH - STROKE, top + half, STROKE, half),
        (left, top + DIGIT_HEIGHT - STROKE, DIGIT_WIDTH, STROKE),
        (left, top + half, STROKE, half),
        (left, top, STROKE, half),
        (left, top + half - STROKE / 2, DIGIT_WIDTH, STROKE),
    ];

    for (segment, &(x, y, width, height)) in segments.iter().enumerate() {
        let colour = if lit[segment] { DIGIT } else { BACKGROUND };
        fill_rect(display, buffer, x, y, width, height, colour);
    }
}

/// Fills a rectangle with one colour, staging as much of it as the buffer holds.
fn fill_rect(
    display: &mut St77916<DisplayBus<'_>, DisplayReset<'_>>,
    buffer: &mut [u8],
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    colour: u16,
) {
    let total = usize::from(width) * usize::from(height) * 2;
    let staged = buffer.len().min(total);
    let [high, low] = colour.to_be_bytes();
    for pixel in buffer[..staged].chunks_exact_mut(2) {
        pixel[0] = high;
        pixel[1] = low;
    }

    if let Err(err) = display.set_window(x, y, x + width - 1, y + height - 1) {
        error!("Effects: window {x},{y} {width}x{height} failed: {err:?}");
        return;
    }
    if let Err(err) = display.interface_mut().fill_bytes(&buffer[..staged], total) {
        error!("Effects: fill at {x},{y} failed: {err:?}");
    }
}
