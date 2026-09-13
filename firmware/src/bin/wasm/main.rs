//! A plugin in WebAssembly, on the board: what it costs to load, to call, and to keep.
//!
//! The format is decided (wasmi 2.0, a manifest beside the module), and everything the decision
//! rests on was measured on the host. This is the first run on the chip, and it answers the
//! three questions the host could not:
//!
//! - **Load time.** `Module::new` parses, validates and -- with eager compilation -- translates
//!   the module into wasmi's own code. Validation is the part a firmware for strangers cannot
//!   skip, and the only part whose cost grows with what the stranger wrote.
//! - **Latency per event.** A wipe goes in as `on_event(2)` and must come out as usage `0xB5`
//!   through the `send_usage` import. `draw` makes three calls back into the firmware, two of
//!   which read out of the plugin's memory -- the boundary, crossed the way a real host crosses
//!   it. The drawing itself is left out: that is firmware code and costs what it costs anyway.
//! - **Heap, and where the page lives.** A plugin's memory is 64 KiB at the least, one wasm
//!   page, and the internal heap is 136 KiB with the radio still to come. The run loads the same
//!   plugin built three ways:
//!
//!   1. **`hid-own`** declares its page itself, so wasmi allocates it through the global
//!      allocator -- which is internal RAM only, so the page must land there.
//!   2. **`hid-import`** imports its page, and the firmware makes it with `Memory::new_static`
//!      out of a slice of external RAM. The global allocator never sees it. Adding the external
//!      RAM to the global allocator instead would be the easy way and is the wrong one: the
//!      allocator would then place whatever it liked out there, and **atomics do not work in
//!      PSRAM on the S3** (esp-alloc says so above its own `psram_allocator!`) -- the engine's
//!      reference count is an atomic.
//!   3. **`hid-default`** is what rustc builds unasked: a 1 MiB stack and 17 pages. It must be
//!      turned away by `StoreLimits` at instantiation, with an error -- not by the allocator, with
//!      a panic of the whole firmware.
//!
//! Then two things a runtime for strangers has to survive: a call that runs out of fuel must
//! come back as a trap, and the instance must answer the next call as if nothing had happened.
//! And dropping everything must give back the whole heap, because uninstalling is planned for
//! from the start -- even for the plugin that ships with the firmware.
//!
//! The run has no screen, no radio and no hand. It is read with `tools/listen.py`:
//!
//! ```text
//! # the three modules are this run's frozen inputs, built once from plugins/hid-remote;
//! # its build.sh no longer makes them
//! cargo build --release --bin wasm && espflash flash -B 921600 target/xtensa-esp32s3-none-elf/release/wasm
//! tools/listen.py --seconds 15
//! ```
//!
//! The plan was to load the plugin from the card. It is embedded instead: the card sits inside
//! the housing, the FAT code only reads, and a 620-byte file is one sector either way.
//!
//! What it answers, at 240 MHz with the portable dispatch loop:
//!
//! - **Loading takes 9.5 ms and 7 KB**, instantiating 1.1 ms. A wipe is answered in **10.4 µs**
//!   for 24 fuel, a `draw` with three calls back into the firmware in **19.0 µs** for 58.
//! - **The page belongs in external RAM, imported.** `hid-own` costs the internal heap 76.6 KB,
//!   because its page lands there; `hid-import` costs **11.3 KB**, its page sits at the start of
//!   the external RAM, and both answer in exactly the same time. So the SDK template imports
//!   its memory, and the loader refuses a module that brings its own.
//! - `hid-default` is refused by the limiter, a run out of fuel comes back as `OutOfFuel` and
//!   the instance answers the next call, and every case gives the heap back to the byte.
//! - **wasmi counts an imported memory twice** against `StoreLimits::memories`: once when the
//!   firmware makes it, once when the importing module is instantiated. Hence the 2 below.

#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::psram::{Psram, PsramConfig, PsramMode};
use esp_hal::time::Instant;
use log::{error, info, warn};
use wasmi::{
    Caller, CompilationMode, Config, Engine, Error, Linker, Memory, MemoryType, Module, Store,
    StoreLimits, StoreLimitsBuilder, TrapCode,
};

esp_bootloader_esp_idf::esp_app_desc!();

const HID_OWN: &[u8] = include_bytes!("../../../assets/plugins/hid-own.wasm");
const HID_IMPORT: &[u8] = include_bytes!("../../../assets/plugins/hid-import.wasm");
const HID_DEFAULT: &[u8] = include_bytes!("../../../assets/plugins/hid-default.wasm");

/// One wasm page, and all the memory a plugin is allowed.
const PAGE: usize = 64 * 1024;
/// How many calls a latency is averaged over.
const CALLS: u32 = 1000;
/// What `on_event` answers to a wipe right: the next title.
const NEXT: u32 = 0xB5;
/// Enough fuel for every timed call; the fuel test gives half of what one `draw` was measured
/// to take. A fixed number would guess at wasmi's cost table, and the first guess here was 100
/// against a `draw` that costs 58.
const PLENTY: u64 = 1_000_000_000;

/// What the firmware keeps for one plugin.
struct Host {
    limits: StoreLimits,
    /// The plugin's memory, whichever side made it. Kept here because an imported memory is
    /// not an export, so `Caller::get_export` would not find it.
    memory: Option<Memory>,
    usages: u32,
    last_usage: u32,
    /// Bytes the firmware read out of the plugin's memory, so the reads cannot be optimised away.
    bytes_read: usize,
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    // The same two regions the firmware has, and nothing external: see the module comment.
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
    let (psram_start, psram_size) = psram.raw_parts();
    info!("--- wasm: a plugin under wasmi ---");
    info!(
        "heap: {} bytes free, {} used; external RAM {} KiB at {:#010x}",
        esp_alloc::HEAP.free(),
        esp_alloc::HEAP.used(),
        psram_size / 1024,
        psram_start as usize,
    );
    // SAFETY: the external RAM is mapped for as long as `psram` lives, which is forever here,
    // and nothing else is handed any of it.
    let page: &'static mut [u8] = unsafe { core::slice::from_raw_parts_mut(psram_start, PAGE) };

    for (name, wasm, page) in [
        ("hid-own", HID_OWN, None),
        ("hid-import", HID_IMPORT, Some(page)),
        ("hid-default", HID_DEFAULT, None),
    ] {
        let before = esp_alloc::HEAP.used();
        info!("== {name}: {} bytes of wasm", wasm.len());
        if let Err(e) = run(wasm, page) {
            // For hid-default this is the expected outcome, and the message says which limit.
            warn!("{name}: {e}");
        }
        let after = esp_alloc::HEAP.used();
        if after == before {
            info!("{name}: everything dropped, heap back where it was");
        } else {
            error!("{name}: {} bytes still held after dropping", after as isize - before as isize);
        }
    }
    info!("--- wasm: done ---");
    let _psram = psram;
    loop {}
}

/// Loads one module, instantiates it, times its two exports and drops it all again.
fn run(wasm: &[u8], page: Option<&'static mut [u8]>) -> Result<(), Error> {
    let base = esp_alloc::HEAP.used();

    let mut config = Config::default();
    config
        .consume_fuel(true)
        .compilation_mode(CompilationMode::Eager)
        // One memory per module, imported or its own. The store limit below cannot say that
        // on its own; see there.
        .wasm_multi_memory(false);
    let engine = Engine::new(&config);
    let began = Instant::now();
    let module = Module::new(&engine, wasm)?;
    info!(
        "load: {} us to parse, validate and translate; heap +{} bytes",
        began.elapsed().as_micros(),
        esp_alloc::HEAP.used() - base
    );

    // An imported memory is counted twice: once when the firmware makes it, and again when the
    // module that imports it is instantiated. With a limit of one, `hid-import` was refused as
    // "too many linear memories" while holding exactly one.
    let limits = StoreLimitsBuilder::new()
        .memory_size(PAGE)
        .memories(if page.is_some() { 2 } else { 1 })
        .tables(1)
        .instances(1)
        .build();
    let mut store = Store::new(
        &engine,
        Host {
            limits,
            memory: None,
            usages: 0,
            last_usage: 0,
            bytes_read: 0,
        },
    );
    store.limiter(|host| &mut host.limits);
    store.set_fuel(PLENTY)?;

    let mut linker = <Linker<Host>>::new(&engine);
    linker.func_wrap("teetotum", "send_usage", |mut c: Caller<'_, Host>, id: u32| {
        let host = c.data_mut();
        host.usages += 1;
        host.last_usage = id;
    })?;
    linker.func_wrap(
        "teetotum",
        "text",
        |mut c: Caller<'_, Host>, ptr: u32, len: u32, _x: i32, _y: i32, _size: u32| {
            // What a real host must do before it draws: find the bytes, and check they are text.
            let n = read(&c, ptr, len).map_or(0, |b| core::str::from_utf8(b).map_or(0, str::len));
            c.data_mut().bytes_read += n;
        },
    )?;
    linker.func_wrap(
        "teetotum",
        "arc",
        |_c: Caller<'_, Host>, _cx: i32, _cy: i32, _r: u32, _start: i32, _sweep: i32, _rgb: u32| {},
    )?;
    linker.func_wrap(
        "teetotum",
        "icon",
        |mut c: Caller<'_, Host>, ptr: u32, rows: u32, _x: i32, _y: i32, _rgb: u32| {
            let n = read(&c, ptr, rows.saturating_mul(4)).map_or(0, <[u8]>::len);
            c.data_mut().bytes_read += n;
        },
    )?;

    if let Some(buf) = page {
        let memory = Memory::new_static(&mut store, MemoryType::new(1, Some(1)), buf)?;
        linker.define("env", "memory", memory)?;
        store.data_mut().memory = Some(memory);
    }

    let began = Instant::now();
    let instance = linker.instantiate_and_start(&mut store, &module)?;
    let took = began.elapsed().as_micros();
    if store.data().memory.is_none() {
        store.data_mut().memory = instance.get_memory(&store, "memory");
    }
    let Some(memory) = store.data().memory else {
        warn!("instance: no memory at all");
        return Ok(());
    };
    let at = memory.data(&store).as_ptr() as usize;
    info!(
        "instance: {took} us; heap +{} bytes in all; its {} KiB of memory at {at:#010x}, {}",
        esp_alloc::HEAP.used() - base,
        memory.data(&store).len() / 1024,
        region(at)
    );

    let on_event = instance.get_typed_func::<u32, u32>(&store, "on_event")?;
    let draw = instance.get_typed_func::<(), ()>(&store, "draw")?;

    // The wipe: right is the next title.
    let fuel = store.get_fuel()?;
    let count = on_event.call(&mut store, 2)?;
    let per_event = fuel - store.get_fuel()?;
    let sent = store.data().last_usage;
    if sent == NEXT && count == 1 {
        info!("wipe: usage {sent:#04x} sent, {per_event} fuel");
    } else {
        error!("wipe: expected usage {NEXT:#04x} once, got {sent:#04x} and a count of {count}");
    }
    let fuel = store.get_fuel()?;
    draw.call(&mut store, ())?;
    let per_draw = fuel - store.get_fuel()?;
    info!(
        "draw: {per_draw} fuel, {} bytes read out of the plugin",
        store.data().bytes_read
    );

    let began = Instant::now();
    for k in 0..CALLS {
        on_event.call(&mut store, 1 + k % 2)?;
    }
    let events = began.elapsed().as_micros();
    let began = Instant::now();
    for _ in 0..CALLS {
        draw.call(&mut store, ())?;
    }
    let draws = began.elapsed().as_micros();
    info!(
        "latency over {CALLS} calls: on_event {}.{} us, draw {}.{} us",
        events / u64::from(CALLS),
        events * 10 / u64::from(CALLS) % 10,
        draws / u64::from(CALLS),
        draws * 10 / u64::from(CALLS) % 10,
    );
    info!(
        "heap after the calls: +{} bytes in all",
        esp_alloc::HEAP.used() - base
    );

    // Out of fuel: a trap, and then the same instance carries on.
    let scarce = per_draw / 2;
    store.set_fuel(scarce)?;
    match draw.call(&mut store, ()) {
        Err(e) if e.as_trap_code() == Some(TrapCode::OutOfFuel) => {
            info!("fuel: {scarce} is not enough for a draw, and it came back as a trap")
        }
        Err(e) => error!("fuel: a trap, but not the expected one: {e}"),
        Ok(()) => error!("fuel: {scarce} was enough for a draw -- the test proves nothing"),
    }
    store.set_fuel(PLENTY)?;
    let before = store.data().usages;
    on_event.call(&mut store, 0)?;
    if store.data().usages == before + 1 {
        info!("after the trap: the next call is answered");
    } else {
        error!("after the trap: the next call sent nothing");
    }
    Ok(())
}

/// The bytes a plugin pointed at, or `None` if they are not all inside its memory.
fn read<'a>(c: &'a Caller<'_, Host>, ptr: u32, len: u32) -> Option<&'a [u8]> {
    let memory = c.data().memory?;
    let start = usize::try_from(ptr).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    memory.data(c).get(start..end)
}

/// Which RAM an address is in, by the address map in the ESP32-S3 technical reference manual.
fn region(at: usize) -> &'static str {
    match at {
        0x3FC8_8000..0x3FD0_0000 => "internal RAM",
        0x3C00_0000..0x3E00_0000 => "external RAM",
        _ => "neither internal nor external RAM",
    }
}
