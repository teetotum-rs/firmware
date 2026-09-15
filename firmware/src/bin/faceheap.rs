//! What a face costs the heap, under each compilation mode wasmi offers.
//!
//! Since faces load on demand, what limits them is no longer how many there are but how large
//! the largest is: Nearby, 7 KB of wasm, holds 29.5 KB of heap and leaves 27 KB. Across the three
//! bundled faces that is roughly 7 KB plus 3.3 bytes per byte of module. This run finds out
//! where that goes and what wasmi's own switches change about it.
//!
//! Each bundled face is loaded through the firmware's own loader, [`Plugin::load_with`], once per
//! [`CompilationMode`], and weighed at four points:
//!
//! - **module**: `Engine` and `Module` alone, as `Module::new` leaves them -- parsing, validation
//!   and, when eager, translation;
//! - **load**: the whole loaded face, store and instance included;
//! - **first**: one tap and one draw, which is where lazy translation pays what it put off;
//! - **all**: every other event and a draw after each.
//!
//! Each point gives what is **held** afterwards and the **peak** while it ran. The peak is the
//! number that decides whether a large face is refused or takes the firmware down: wasmi
//! allocates through the global allocator, and an allocation that fails there is a panic.
//! `max_usage` only ever rises, so before each point the run lifts the heap to the old maximum
//! with a block of ballast; the new maximum less that floor is then the peak of this point alone.
//!
//! **The ballast stacks up**: each point leaves the maximum one peak higher, and the next lifts
//! to that. Weighing every load in a single boot therefore dies on its own ballast once it grows
//! past what the heap holds. So each face and mode gets a boot of its own, counted in RTC memory
//! across a software reset, and the module alone is weighed last, after the face is gone,
//! because it is the other large peak.
//!
//! No screen, no radio, no hand:
//!
//! ```text
//! cargo build --release --bin faceheap && espflash flash -B 921600 target/xtensa-esp32s3-none-elf/release/faceheap
//! espflash monitor
//! ```
//!
//! What it found:
//!
//! - **The compilation mode is not a lever.** Once tap and draw have run, the lazy modes hold as
//!   much as eager or more (the teetotum +4 KB under `Lazy`): a face calls almost all of its
//!   functions at once. They only move the cost from loading to the first call.
//! - **The heap is a base of about 8 KB plus the translated code**: an engine and module of
//!   about 4.5 KB, store and instance about 2 KB, stacks 1.6 KB at the first call. The module's
//!   data segments are freed once it is instantiated, so they cost peak and not held. The peak
//!   of a load is 3 to 5 KB above what it holds afterwards.
//! - **`indirect-dispatch` halves the translated code**: 4.3 bytes per byte of wasm code became
//!   2.4, and Nearby holds 22 964 bytes instead of 31 108, while a call got faster (see the
//!   workspace `Cargo.toml`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::convert::Infallible;
use core::hint::black_box;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::psram::{Psram, PsramConfig, PsramMode};
use esp_hal::rtc_cntl::SocResetReason;
use esp_hal::system::{reset_reason, software_reset};
use esp_hal::time::Instant;
use log::{error, info};
use teetotum::menu::PALETTE;
use teetotum_face::Event;
use teetotum_firmware::plugin::{self, Page, Plugin};
use wasmi::{CompilationMode, Engine, Module};

esp_bootloader_esp_idf::esp_app_desc!();

const FACES: [(&str, &[u8]); 3] = [
    (
        "hid-remote",
        include_bytes!("../../assets/plugins/hid-remote.wasm"),
    ),
    (
        "teetotum",
        include_bytes!("../../assets/plugins/teetotum-plugin.wasm"),
    ),
    ("nearby", include_bytes!("../../assets/plugins/nearby.wasm")),
];

const MODES: [(&str, CompilationMode); 3] = [
    ("eager", CompilationMode::Eager),
    ("lazy-translation", CompilationMode::LazyTranslation),
    ("lazy", CompilationMode::Lazy),
];

/// Every event after the first tap. `Nearby` reaches only the face with the right to it.
const EVENTS: [Event; 7] = [
    Event::WipeLeft,
    Event::WipeRight,
    Event::Clockwise,
    Event::Anticlockwise,
    Event::WipeUp,
    Event::WipeDown,
    Event::Nearby,
];

/// What one point of the run cost.
struct Weight {
    /// Bytes held afterwards, against before; negative when it gave some back.
    held: isize,
    /// The most bytes held at once while it ran, against before.
    peak: usize,
    micros: u64,
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    // The same two regions the firmware has.
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    esp_alloc::heap_allocator!(size: 64 * 1024);
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let psram = Psram::new(
        peripherals.PSRAM,
        PsramConfig {
            mode: PsramMode::OctalSpi,
            ..Default::default()
        },
    );
    let (psram_start, _) = psram.raw_parts();
    // SAFETY: the external RAM is mapped for as long as `psram` lives, which is forever here,
    // and nothing else is handed any of it.
    let buf: &'static mut [u8] =
        unsafe { core::slice::from_raw_parts_mut(psram_start, plugin::PAGE) };
    let page = Page::new(buf).expect("one page of external RAM");

    let rounds = FACES.len() * MODES.len();
    // Only our own reset carries the count on; a reset at EN, and the one after flashing, start
    // from the beginning, whatever the RTC memory held.
    // SAFETY: one core, nothing else touches the static.
    let round = match reset_reason() {
        Some(SocResetReason::CoreSw) => unsafe { ROUND as usize },
        _ => 0,
    };
    if round == 0 {
        info!("--- faceheap: what a face costs the heap ---");
        info!(
            "heap: {} bytes free, {} used",
            esp_alloc::HEAP.free(),
            esp_alloc::HEAP.used()
        );
    }
    if round >= rounds {
        info!("--- faceheap: done ---");
        let _psram = psram;
        teetotum::step::halt()
    }
    let (face, wasm) = FACES[round / MODES.len()];
    let (mode, compilation) = MODES[round % MODES.len()];
    measure(face, wasm, mode, compilation, page);
    // SAFETY: as above.
    unsafe { ROUND = round as u32 + 1 };
    software_reset();
}

/// Which face and mode this boot weighs, kept across the software reset between them.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut ROUND: u32 = 0;

/// Loads one face under one mode and weighs it at the four points.
fn measure(face: &str, wasm: &'static [u8], mode: &str, compilation: CompilationMode, page: Page) {
    let mut config = plugin::config();
    config.compilation_mode(compilation);
    let base = esp_alloc::HEAP.used();

    let (loaded, w) = weigh(|| Plugin::load_with(wasm, page, &config));
    let mut plugin = match loaded {
        Ok(plugin) => plugin,
        Err((e, _)) => {
            error!("{face} {mode}: refused -- {e}");
            return;
        }
    };
    report(face, mode, "load", &w);

    let ((), w) = weigh(|| {
        plugin.event(Event::Tap);
        plugin.draw(&mut Sink, &PALETTE);
    });
    report(face, mode, "first", &w);

    let ((), w) = weigh(|| {
        for event in EVENTS {
            plugin.event(event);
            plugin.draw(&mut Sink, &PALETTE);
        }
    });
    report(face, mode, "all", &w);
    if let Some(fault) = plugin.fault() {
        error!("{face} {mode}: stopped -- {fault}");
    }

    plugin.unload();
    let left = esp_alloc::HEAP.used() as isize - base as isize;
    if left == 0 {
        info!("{face} {mode}: unloaded, heap back where it was");
    } else {
        error!("{face} {mode}: unloaded, {left} bytes still held");
    }

    let (module, w) = weigh(|| {
        let engine = Engine::new(&config);
        Module::new(&engine, wasm).map(|module| (engine, module))
    });
    match module {
        Ok(module) => {
            report(face, mode, "module", &w);
            drop(module);
        }
        Err(e) => error!("{face} {mode}: module refused -- {e}"),
    }
}

/// Runs `f` and weighs it. The ballast lifts the heap to its old maximum first, so that the new
/// maximum is this call's own and not one from before.
fn weigh<T>(f: impl FnOnce() -> T) -> (T, Weight) {
    let lift = esp_alloc::HEAP
        .stats()
        .max_usage
        .saturating_sub(esp_alloc::HEAP.used());
    let ballast: Vec<u8> = Vec::with_capacity(lift);
    black_box(&ballast);
    let floor = esp_alloc::HEAP.used();
    let began = Instant::now();
    let out = f();
    let micros = began.elapsed().as_micros();
    let weight = Weight {
        held: esp_alloc::HEAP.used() as isize - floor as isize,
        peak: esp_alloc::HEAP.stats().max_usage.saturating_sub(floor),
        micros,
    };
    drop(ballast);
    (out, weight)
}

fn report(face: &str, mode: &str, point: &str, w: &Weight) {
    info!(
        "{face} {mode} {point}: held {:+}, peak {}, {}.{} ms",
        w.held,
        w.peak,
        w.micros / 1000,
        w.micros % 1000 / 100
    );
}

/// A screen that forgets what is drawn on it: the drawing is firmware code and costs the heap
/// nothing, but it has to run for the face's list to be read.
struct Sink;

impl OriginDimensions for Sink {
    fn size(&self) -> Size {
        Size::new(360, 360)
    }
}

impl DrawTarget for Sink {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Rgb565>>,
    {
        for pixel in pixels {
            black_box(pixel);
        }
        Ok(())
    }
}
