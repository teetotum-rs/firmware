//! Whether a face that does not fit the heap is refused rather than taking the firmware down.
//!
//! wasmi allocates through the global allocator and cannot fail, so a face too large for what
//! is free used to be a panic in `handle_alloc_error` -- from the point of view of the glass,
//! a device that reboots when a plugin is opened. `Plugin::load` now estimates the cost from
//! the size of the module first (`plugin::heap_needed`) and refuses before the engine exists.
//!
//! An estimate has two ways to be wrong, and this run tries both:
//!
//! - **too eager**: it turns away a face that would have fitted. Each bundled face is loaded
//!   with the heap as the firmware has it, and all three have to load.
//! - **too generous**: it lets a face through that then panics. Ballast fills the heap to just
//!   under what the largest face is estimated to need, and that face has to come back refused
//!   -- with the run still alive to say so.
//!
//! Then the ballast goes and the same face loads, which is the proof that the refusal was the
//! guard and not a heap the run had wrecked.
//!
//! No screen, no radio, no hand:
//!
//! ```text
//! cargo build --release --bin heapguard && espflash flash -B 921600 target/xtensa-esp32s3-none-elf/release/heapguard
//! tools/listen.py --seconds 20
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::hint::black_box;

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::psram::{Psram, PsramConfig, PsramMode};
use esp_hal::time::Instant;
use log::{error, info};
use teetotum_firmware::plugin::{self, LoadError, Page, Plugin};

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

/// How large a piece the ballast is laid in. **Not one block**: the heap is two regions, and the
/// first try asked for 99 944 bytes at once against 139 280 free and died in `handle_alloc_error`
/// -- which is the same panic this guard is here to prevent, and the proof that `HEAP.free()` is
/// a sum and not a block.
const BALLAST_BLOCK: usize = 4 * 1024;

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
    let mut page = Some(Page::new(buf).expect("one page of external RAM"));

    info!("--- heapguard: a face that does not fit is refused ---");
    info!("heap: {} bytes free", esp_alloc::HEAP.free());

    for (face, wasm) in FACES {
        info!(
            "{face}: {} bytes of module, needs about {}, {} free",
            wasm.len(),
            plugin::heap_needed(wasm.len()),
            esp_alloc::HEAP.free()
        );
        page = Some(load(face, wasm, page.take().expect("the page"), true));
    }

    // The largest face against a heap filled to just under what it is estimated to need.
    let (face, wasm) = FACES[FACES.len() - 1];
    let need = plugin::heap_needed(wasm.len());
    let free = esp_alloc::HEAP.free();
    if free < need {
        error!("{face}: {free} bytes free is already under {need}; nothing to prove here");
        loop {}
    }
    let mut ballast: Vec<Vec<u8>> = Vec::new();
    while esp_alloc::HEAP.free() >= need {
        ballast.push(Vec::with_capacity(BALLAST_BLOCK));
    }
    black_box(&ballast);
    info!(
        "{face}: {} blocks of ballast, {} free against {need} needed",
        ballast.len(),
        esp_alloc::HEAP.free()
    );
    page = Some(load(face, wasm, page.take().expect("the page"), false));

    drop(ballast);
    info!(
        "{face}: ballast gone, {} bytes free",
        esp_alloc::HEAP.free()
    );
    let _ = load(face, wasm, page.take().expect("the page"), true);

    info!("--- heapguard: done ---");
    let _psram = psram;
    loop {}
}

/// Loads a face, says what happened, and gives the page back either way. `expected` is whether
/// it should have loaded; anything else is logged as an error, so the run reads as a verdict
/// rather than a list of numbers.
fn load(face: &str, wasm: &'static [u8], page: Page, expected: bool) -> Page {
    let before = esp_alloc::HEAP.used();
    let began = Instant::now();
    match Plugin::load(wasm, page) {
        Ok(plugin) => {
            let took = began.elapsed().as_micros();
            let cost = esp_alloc::HEAP.used().saturating_sub(before);
            if expected {
                info!("{face}: loaded in {took} us, heap +{cost} bytes");
            } else {
                error!(
                    "{face}: loaded in {took} us, heap +{cost} bytes -- should have been refused"
                );
            }
            plugin.unload()
        }
        Err((e, page)) => {
            let refused = matches!(e, LoadError::Heap { .. });
            if expected {
                error!("{face}: refused -- {e}");
            } else if refused {
                info!("{face}: refused -- {e}");
            } else {
                error!("{face}: refused for the wrong reason -- {e}");
            }
            page
        }
    }
}
