//! Does a plugin survive a round trip through a slot of the `plugins` partition?
//!
//! Headless and self-judging. On the last slot it checks that a write without its header reads
//! as empty, that a full write waits to be accepted and reads back byte for byte with a holding
//! signature, that accepting it leaves the module readable, that one cleared bit fails the hash, and that an erased slot reads as empty. The slot is erased at the
//! end.
//!
//! ```text
//! cargo build --release --bin slots
//! espflash flash -B 921600 --partition-table partitions.csv target/xtensa-esp32s3-none-elf/release/slots
//! tools/listen.py --seconds 20
//! ```

#![no_std]
#![no_main]

use embedded_storage::nor_flash::NorFlash;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::time::Instant;
use esp_storage::FlashStorage;
use log::{error, info};
use teetotum_firmware::flash::{self, TABLE_SCRATCH};
use teetotum_firmware::plugin::{self, PluginId};
use teetotum_firmware::slots::{self, Error, Slots, slot};

esp_bootloader_esp_idf::esp_app_desc!();

const WASM: &[u8] = include_bytes!("../../assets/plugins/hid-remote.wasm");

#[repr(align(4))]
struct Aligned([u8; 4096]);

fn check(ok: bool, what: &str, failed: &mut u32) {
    if ok {
        info!("PASS {what}");
    } else {
        error!("FAIL {what}");
        *failed += 1;
    }
}

fn halt() -> ! {
    teetotum::step::halt()
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    // The partition table's types go through `enumset`, which allocates.
    esp_alloc::heap_allocator!(size: 16 * 1024);
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();
    delay.delay_millis(1500);

    let mut flash = FlashStorage::new(peripherals.FLASH);
    let mut table = [0u8; TABLE_SCRATCH];
    let mut buf = Aligned([0u8; 4096]);
    let mut failed = 0;
    assert!(WASM.len() <= buf.0.len());

    info!("--- slots: a plugin through the plugins partition ---");

    let mut region = match flash::nvs(&mut flash, &mut table) {
        Ok(region) => region,
        Err(e) => {
            error!("no nvs partition: {e:?}");
            halt()
        }
    };
    info!("nvs: {} bytes", region.partition_size());

    region = match flash::plugins(&mut flash, &mut table) {
        Ok(region) => region,
        Err(e) => {
            error!("no plugins partition: {e:?}");
            halt()
        }
    };
    let count = region.partition_size() / slots::SLOT;
    info!("plugins: {} bytes, {count} slots", region.partition_size());
    if count == 0 {
        error!("no slot fits");
        halt()
    }
    let last = count - 1;
    let start = (last * slots::SLOT) as u32;

    // A write cut short before its header: the module is there, the header is not.
    let rounded = WASM.len().div_ceil(4) * 4;
    buf.0.fill(0xff);
    buf.0[..WASM.len()].copy_from_slice(WASM);
    let torn = region
        .erase(start, start + slots::SLOT as u32)
        .and_then(|()| region.write(start + slots::HEADER as u32, &buf.0[..rounded]));
    check(torn.is_ok(), "module written without header", &mut failed);

    let mut slots = Slots::new(region);
    for n in 0..count {
        match slots.header(n) {
            Ok(Some(h)) => info!("slot {n}: id {:02x?}, {} bytes", h.id.bytes(), h.len),
            Ok(None) => {}
            Err(e) => error!("slot {n}: {e:?}"),
        }
    }
    check(
        matches!(slots.header(last), Ok(None)),
        "slot without header reads as empty",
        &mut failed,
    );

    let began = Instant::now();
    let written = slots.write(last, WASM);
    let write_ms = began.elapsed().as_millis();
    info!("write: {written:?} in {write_ms} ms");
    let id = PluginId::of(WASM).ok();
    check(
        written.is_ok_and(|h| Some(h.id) == id && h.len == WASM.len()),
        "write names the module's id and length",
        &mut failed,
    );
    check(
        written.is_ok_and(|h| !h.accepted)
            && slots
                .header(last)
                .is_ok_and(|h| h.is_some_and(|h| !h.accepted)),
        "a written slot waits to be accepted",
        &mut failed,
    );

    buf.0.fill(0);
    let began = Instant::now();
    let read = slots.read(last, &mut buf.0);
    let read_us = began.elapsed().as_micros();
    info!("read: {read:?} in {read_us} us (hash and id checked)");
    check(
        read.is_ok_and(|h| h.is_some()) && &buf.0[..WASM.len()] == WASM,
        "module reads back byte for byte",
        &mut failed,
    );
    let began = Instant::now();
    let verified = plugin::verify(&buf.0[..WASM.len()]);
    info!(
        "signature: {verified:?} in {} ms",
        began.elapsed().as_millis()
    );
    check(verified.is_ok(), "signature holds on the copy", &mut failed);

    let began = Instant::now();
    let accepted = slots.accept(last);
    info!("accept: {accepted:?} in {} us", began.elapsed().as_micros());
    check(
        accepted.is_ok()
            && slots
                .header(last)
                .is_ok_and(|h| h.is_some_and(|h| h.accepted)),
        "accepting marks the header",
        &mut failed,
    );
    buf.0.fill(0);
    let reread = slots.read(last, &mut buf.0);
    check(
        reread.is_ok_and(|h| h.is_some()) && &buf.0[..WASM.len()] == WASM,
        "accepted module still reads back byte for byte",
        &mut failed,
    );

    // Clearing bits needs no erase: zero the module's first word.
    let mut region = slots.into_inner();
    let cleared = region.write(start + slots::HEADER as u32, &[0u8; 4]);
    check(cleared.is_ok(), "first module word zeroed", &mut failed);
    let mut slots = Slots::new(region);
    let damaged = slots.read(last, &mut buf.0);
    info!("read after damage: {damaged:?}");
    check(
        damaged == Err(Error::Module(slot::Error::Hash)),
        "damaged module fails its hash",
        &mut failed,
    );

    let began = Instant::now();
    let erased = slots.erase(last);
    info!("erase: {erased:?} in {} ms", began.elapsed().as_millis());
    check(
        erased.is_ok() && matches!(slots.header(last), Ok(None)),
        "erased slot reads as empty",
        &mut failed,
    );

    if failed == 0 {
        info!("--- slots: all checks passed ---");
    } else {
        error!("--- slots: {failed} checks failed ---");
    }
    halt()
}
