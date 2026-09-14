//! The loader: a face, from its WebAssembly bytes to something the firmware can show.
//!
//! wasmi 2.0 runs it, under rules measured on the board (`src/bin/wasm/main.rs`):
//!
//! - **Its one page of memory is imported, and lies in external RAM.** A module that brings its
//!   own has wasmi allocate it through the global allocator, which is internal RAM only: 76.6 KB
//!   of a 136 KiB heap against 11.3 KB, at the same latency. So a module that does not import
//!   its memory is refused, and the page is a [`Page`] cut out of the PSRAM once.
//! - **A face is signed, and checked before anything else is.** Any key is accepted -- there is
//!   no issuer -- but the bytes have to be the ones its holder signed. Key and name are the
//!   face's identity; see [`PluginId`].
//! - **What a face may do is settled before any of it runs.** The manifest comes out of its
//!   custom section, the imports are checked against it, and only then is anything
//!   instantiated. A face that imports `send_usage` without `Rights::HID` never gets that far.
//! - **Every call runs on a budget** of [`abi::FUEL`], and a trap stops the face for good: it is
//!   logged, the glass says so, and the face is not called again until it is loaded anew. The
//!   instance would answer the next call -- measured -- but a face that trapped once holds
//!   whatever state the trap cut it off in.
//! - **A face that would not fit is refused, not loaded.** wasmi allocates through the global
//!   allocator and cannot fail: an allocation that does not fit is a panic, so a face too large
//!   for the heap would take the firmware down. What it will cost is therefore estimated from
//!   its size before anything is compiled; see [`heap_needed`].
//! - **Drawing is collected, not done.** A `draw` makes its calls, each is checked and kept, and
//!   they are drawn once the face has returned; the bytes they point at are still in its page.
//!   So a face never holds the framebuffer, and a trap halfway through a `draw` leaves a picture
//!   that was never begun rather than half of one.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use core::ptr::NonNull;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Arc, PrimitiveStyle};
use esp_hal::rng::{Rng, Trng};
use esp_hal::time::Instant;
use log::{error, info, warn};
use teetotum::menu::{Palette, draw_packed, fonts, text};
use teetotum_face::manifest::{self, Manifest};
use teetotum_face::{Colour, Event, Icon, Paint, Radio, Rights, Role, Size, Usage, abi};
pub use teetotum_pack::{PluginId, verify};
use wasmi::{
    Caller, CompilationMode, Config, Engine, Error, ExternType, Linker, Memory, MemoryType, Module,
    Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

/// One wasm page: all the memory a face gets.
pub const PAGE: usize = 64 * 1024;

/// How many bytes an icon takes, in a face's memory as in its manifest.
const ICON_BYTES: usize = 4 * Icon::SIZE;

/// What a face costs the heap before a byte of its code is translated: engine and module,
/// store and instance, and the stack the first call runs on. Measured at roughly 8 KB
/// (`src/bin/faceheap.rs`).
const HEAP_BASE: usize = 8 * 1024;

/// How far above what it holds the peak of loading runs: measured at 3 to 5 KB, taken as 6.
const HEAP_PEAK: usize = 6 * 1024;

/// What the firmware is left with once the face is loaded. **Chosen, not measured**: the glass,
/// the menus and the faces themselves all allocate while a face runs, and a face that fits by a
/// hundred bytes would only move the panic to the next thing that asks.
const HEAP_SPARE: usize = 8 * 1024;

/// What loading a face of `wasm` bytes is expected to cost the internal heap, at its peak, with
/// what the firmware needs afterwards on top.
///
/// Measured (`src/bin/faceheap.rs`), eager and with `indirect-dispatch`: the three bundled
/// faces held 9 308, 14 112 and 21 324 bytes at 1 137,
/// 2 604 and 7 133 bytes of module. A straight line through the outer two misses the middle by
/// 1.9 KB -- how much of a module is code and how much is data moves the slope -- so this is
/// deliberately the generous reading: [`HEAP_BASE`] plus 2.5 bytes per byte of module covers all
/// three by 1.7 to 4.7 KB.
///
/// **It is an estimate, and it errs towards refusing.** A face may be turned away that would
/// have fitted; the alternative is a panic, and a face that says why on the glass is worth a
/// few kilobytes of headroom.
pub fn heap_needed(wasm: usize) -> usize {
    HEAP_BASE + wasm * 5 / 2 + HEAP_PEAK + HEAP_SPARE
}

/// One page of external RAM, lent to one face at a time.
///
/// wasmi takes a `&'static mut [u8]` for a memory it does not allocate itself, and does not
/// give it back. A face can be removed and installed again without a reboot, so the page is
/// kept as this token instead: whoever holds it may lend the page, and a [`Plugin`] holds it for
/// as long as its store exists and hands it back from [`Plugin::unload`].
pub struct Page {
    at: NonNull<u8>,
}

impl Page {
    /// `None` unless `buf` is exactly one page.
    pub fn new(buf: &'static mut [u8]) -> Option<Self> {
        (buf.len() == PAGE).then(|| Self {
            at: NonNull::from(buf).cast(),
        })
    }

    /// The page as wasmi wants it, **cleared**.
    ///
    /// Every face gets the same page, one after another, and a module only initialises what its
    /// own data covers. Without the clear, a face could read what the one before left behind --
    /// the names Nearby heard, say -- and a plugin could not keep its promise that it sees no
    /// other plugin.
    ///
    /// # Safety
    ///
    /// What this returned last must be gone -- the store it went into dropped -- before it is
    /// called again. [`Plugin`] keeps to that by holding the token and the store together and
    /// dropping the store first.
    unsafe fn lend(&mut self) -> &'static mut [u8] {
        // SAFETY: the pointer came from a `&'static mut [u8]` of `PAGE` bytes that nothing else
        // holds, and the caller keeps to one borrow at a time.
        let page = unsafe { core::slice::from_raw_parts_mut(self.at.as_ptr(), PAGE) };
        page.fill(0);
        page
    }
}

/// What the firmware keeps for a face while it runs.
struct Host {
    limits: StoreLimits,
    /// Its page. Kept here because an imported memory is not an export.
    memory: Option<Memory>,
    /// Whether a `draw` is running: drawing is allowed then and only then.
    drawing: bool,
    draws: Vec<Draw>,
    usages: Vec<Usage>,
    /// Usages past [`abi::USAGES_MAX`] in the event running now.
    dropped: u32,
    /// Random bytes handed out in the event running now, against [`abi::RANDOM_MAX`].
    random: usize,
    /// How often the face wants the motor to pulse, in milliseconds, clamped; `None` for not.
    pulse: Option<u32>,
}

/// One drawing call, checked and kept until the face has returned.
#[derive(Clone, Copy)]
enum Draw {
    Text {
        at: usize,
        len: usize,
        point: Point,
        size: Size,
        paint: Paint,
    },
    Arc {
        centre: Point,
        radius: u32,
        start: i32,
        sweep: i32,
        width: u32,
        paint: Paint,
    },
    Icon {
        at: usize,
        centre: Point,
        paint: Paint,
    },
}

/// What one event came to.
#[derive(Default)]
pub struct Reply {
    /// Whether the face wants to be drawn again. Also set when the event stopped it, so that
    /// the glass says so.
    pub redraw: bool,
    /// What it sent, for the firmware to hand to the other chip. Empty when it trapped: a call
    /// that did not finish did not send anything.
    pub usages: Vec<Usage>,
}

/// A face, loaded and running.
pub struct Plugin {
    manifest: Manifest<'static>,
    size: usize,
    store: Store<Host>,
    on_event: TypedFunc<u32, u32>,
    draw: TypedFunc<(), ()>,
    page: Page,
    fault: Option<String>,
}

impl Plugin {
    /// Checks, compiles and instantiates a face. On failure the page comes back with the reason.
    pub fn load(wasm: &'static [u8], page: Page) -> Result<Self, (LoadError, Page)> {
        Self::load_with(wasm, page, &config())
    }

    /// [`Plugin::load`] under a configuration of wasmi's other than [`config`], for a run that
    /// compares them.
    pub fn load_with(
        wasm: &'static [u8],
        mut page: Page,
        config: &Config,
    ) -> Result<Self, (LoadError, Page)> {
        match instantiate(wasm, &mut page, config) {
            Ok((manifest, store, on_event, draw)) => Ok(Self {
                manifest,
                size: wasm.len(),
                store,
                on_event,
                draw,
                page,
                fault: None,
            }),
            Err(e) => Err((e, page)),
        }
    }

    pub fn manifest(&self) -> &Manifest<'static> {
        &self.manifest
    }

    /// The module's size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Why the face was stopped, if it was.
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    /// How often the face wants the motor to pulse, in milliseconds, if it does and is running.
    /// The firmware keeps the time; see `teetotum_face::pulse`.
    pub fn pulse(&self) -> Option<u32> {
        self.fault
            .is_none()
            .then(|| self.store.data().pulse)
            .flatten()
    }

    /// Hands an event to the face. A turn of the knob reaches it only with `Rights::KNOB`, a new
    /// round of the radio only with `Rights::RADIO`, whether a phone is connected only with
    /// `Rights::HID`.
    pub fn event(&mut self, event: Event) -> Reply {
        let needs = match event {
            Event::Clockwise | Event::Anticlockwise => Rights::KNOB,
            Event::Nearby => Rights::RADIO,
            Event::Linked | Event::Unlinked => Rights::HID,
            _ => Rights::NONE,
        };
        if self.fault.is_some() || !self.manifest.rights().contains(needs) {
            return Reply::default();
        }
        let host = self.store.data_mut();
        host.usages.clear();
        host.dropped = 0;
        host.random = 0;
        let answer = self
            .store
            .set_fuel(abi::FUEL)
            .and_then(|()| self.on_event.call(&mut self.store, event as u32));
        match answer {
            Ok(redraw) => {
                let host = self.store.data_mut();
                if host.dropped > 0 {
                    warn!(
                        "Plugin: {} sent {} usages past the limit, dropped",
                        self.manifest.name(),
                        host.dropped
                    );
                }
                Reply {
                    redraw: redraw != 0,
                    usages: core::mem::take(&mut host.usages),
                }
            }
            Err(e) => {
                self.stop(&e);
                Reply {
                    redraw: true,
                    usages: Vec::new(),
                }
            }
        }
    }

    /// Has the face draw itself into `target`, in `palette` wherever it asks for the theme's
    /// colours. Answers `false` when nothing was drawn because the face is stopped -- which it
    /// may have been by this very call.
    pub fn draw<D>(&mut self, target: &mut D, palette: &Palette) -> bool
    where
        D: DrawTarget<Color = Rgb565>,
    {
        if self.fault.is_some() {
            return false;
        }
        let host = self.store.data_mut();
        host.draws.clear();
        host.drawing = true;
        let answer = self
            .store
            .set_fuel(abi::FUEL)
            .and_then(|()| self.draw.call(&mut self.store, ()));
        self.store.data_mut().drawing = false;
        if let Err(e) = answer {
            self.stop(&e);
            return false;
        }
        let host = self.store.data();
        let Some(memory) = host.memory else {
            return false;
        };
        let bytes = memory.data(&self.store);
        for draw in &host.draws {
            render(target, bytes, *draw, palette);
        }
        true
    }

    /// Drops the face and everything wasmi holds for it, and gives the page back.
    pub fn unload(self) -> Page {
        let Self { store, page, .. } = self;
        // The store first: it holds the page, and the token may be lent again after this.
        drop(store);
        page
    }

    fn stop(&mut self, error: &Error) {
        error!("Plugin: {} stopped -- {error}", self.manifest.name());
        self.fault = Some(error.to_string());
    }
}

/// Why a face was not loaded.
#[derive(Debug)]
pub enum LoadError {
    Manifest(manifest::Error),
    /// Its signature does not hold: the bytes are not what the key's holder signed.
    Signature,
    /// The module brings its own memory, or none: it was built without `--import-memory`.
    OwnMemory,
    /// It asks for more pages than a face gets.
    Pages(u64),
    /// It imports something the firmware does not offer.
    Import(String),
    /// It imports a function its manifest has not asked the right for.
    NotGranted {
        import: &'static str,
        right: Rights,
    },
    /// It is expected to need more heap than is free; see [`heap_needed`].
    Heap {
        need: usize,
        free: usize,
    },
    /// wasmi refused it: not valid, not linkable, or without the two exports.
    Wasm(Error),
}

impl From<Error> for LoadError {
    fn from(e: Error) -> Self {
        Self::Wasm(e)
    }
}

impl From<teetotum_pack::Error> for LoadError {
    fn from(e: teetotum_pack::Error) -> Self {
        match e {
            teetotum_pack::Error::Manifest(e) => Self::Manifest(e),
            _ => Self::Signature,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(e) => write!(f, "{e}"),
            Self::Signature => f.write_str("signature does not match its bytes and key"),
            Self::OwnMemory => f.write_str("does not import its memory"),
            Self::Pages(n) => write!(f, "asks for {n} pages of memory, a face gets 1"),
            Self::Import(name) => write!(f, "imports {name}, which the firmware does not offer"),
            Self::NotGranted { import, right } => {
                write!(
                    f,
                    "imports {import} without the right {right} in its manifest"
                )
            }
            Self::Heap { need, free } => {
                write!(f, "needs about {need} bytes of heap, {free} free")
            }
            Self::Wasm(e) => write!(f, "{e}"),
        }
    }
}

/// How the loader sets up wasmi for a face.
pub fn config() -> Config {
    let mut config = Config::default();
    config
        .consume_fuel(true)
        // Eager because the lazy modes save nothing: a face calls almost every function on its
        // first tap or draw, and lazy translation then holds as much or more, measured with
        // `src/bin/faceheap.rs`. `Lazy` would also validate only at the first call, after face
        // code ran.
        .compilation_mode(CompilationMode::Eager)
        // One memory per module, and it has to be the imported one; see `check_imports`.
        .wasm_multi_memory(false);
    config
}

/// Everything [`Plugin::load`] does, with the page only borrowed: if this fails, the store it
/// went into is dropped on the way out, and the page can go back.
#[allow(clippy::type_complexity)]
fn instantiate(
    wasm: &'static [u8],
    page: &mut Page,
    config: &Config,
) -> Result<
    (
        Manifest<'static>,
        Store<Host>,
        TypedFunc<u32, u32>,
        TypedFunc<(), ()>,
    ),
    LoadError,
> {
    let manifest = Manifest::read(wasm).map_err(LoadError::Manifest)?;
    let started = Instant::now();
    verify(wasm)?;
    info!(
        "Plugin: {} {} signature holds, checked in {} ms",
        manifest.name(),
        manifest.version(),
        started.elapsed().as_millis()
    );

    // Before the engine, because from here on every allocation is wasmi's and none of them may
    // fail. `free` is the sum over the heap's regions rather than its largest block, so a
    // fragmented heap can still pass this and then panic; there is no allocator call that would
    // answer the better question.
    let free = esp_alloc::HEAP.free();
    let need = heap_needed(wasm.len());
    if free < need {
        return Err(LoadError::Heap { need, free });
    }

    let engine = Engine::new(config);
    let module = Module::new(&engine, wasm)?;
    check_imports(&module, manifest.rights())?;

    // wasmi 2.0 counts an imported memory twice against `memories` -- once when the firmware
    // makes it, once when the importing module is instantiated -- so one memory takes a limit
    // of two. With a limit of one, a face holding exactly one memory was refused as having too
    // many.
    let limits = StoreLimitsBuilder::new()
        .memory_size(PAGE)
        .memories(2)
        .tables(1)
        .instances(1)
        .build();
    let mut store = Store::new(
        &engine,
        Host {
            limits,
            memory: None,
            drawing: false,
            draws: Vec::new(),
            usages: Vec::new(),
            dropped: 0,
            random: 0,
            pulse: None,
        },
    );
    store.limiter(|host| &mut host.limits);

    let mut linker = linker(&engine, manifest.rights())?;
    // SAFETY: the store this goes into is dropped before the page is lent again -- on the way
    // out of here if anything below fails, and by `Plugin::unload` otherwise.
    let buf = unsafe { page.lend() };
    let memory = Memory::new_static(&mut store, MemoryType::new(1, Some(1)), buf)?;
    linker
        .define(abi::MEMORY_MODULE, abi::MEMORY, memory)
        .map_err(Error::from)?;
    store.data_mut().memory = Some(memory);

    let instance = linker.instantiate_and_start(&mut store, &module)?;
    let on_event = instance.get_typed_func::<u32, u32>(&store, abi::ON_EVENT)?;
    let draw = instance.get_typed_func::<(), ()>(&store, abi::DRAW)?;
    Ok((manifest, store, on_event, draw))
}

/// Holds the module's imports to what the firmware offers and the manifest grants.
///
/// With `wasm_multi_memory` off, a module that imports a memory cannot also have one of its
/// own, so "imports its memory" is the whole test for "its page will not land in internal RAM".
fn check_imports(module: &Module, rights: Rights) -> Result<(), LoadError> {
    let mut memory = false;
    for import in module.imports() {
        match (import.module(), import.name(), import.ty()) {
            (abi::MEMORY_MODULE, abi::MEMORY, ExternType::Memory(ty)) => {
                if ty.minimum() > abi::PAGES {
                    return Err(LoadError::Pages(ty.minimum()));
                }
                memory = true;
            }
            (abi::MODULE, abi::SEND_USAGE, _) if !rights.contains(Rights::HID) => {
                return Err(LoadError::NotGranted {
                    import: abi::SEND_USAGE,
                    right: Rights::HID,
                });
            }
            (abi::MODULE, abi::RANDOM, _) if !rights.contains(Rights::RANDOM) => {
                return Err(LoadError::NotGranted {
                    import: abi::RANDOM,
                    right: Rights::RANDOM,
                });
            }
            (abi::MODULE, abi::NEARBY, _) if !rights.contains(Rights::RADIO) => {
                return Err(LoadError::NotGranted {
                    import: abi::NEARBY,
                    right: Rights::RADIO,
                });
            }
            (abi::MODULE, abi::PULSE, _) if !rights.contains(Rights::HAPTIC) => {
                return Err(LoadError::NotGranted {
                    import: abi::PULSE,
                    right: Rights::HAPTIC,
                });
            }
            (
                abi::MODULE,
                abi::SEND_USAGE
                | abi::TEXT
                | abi::ARC
                | abi::ICON
                | abi::RANDOM
                | abi::NEARBY
                | abi::PULSE,
                ExternType::Func(_),
            ) => {}
            (module, name, _) => return Err(LoadError::Import(format!("{module}.{name}"))),
        }
    }
    if memory {
        Ok(())
    } else {
        Err(LoadError::OwnMemory)
    }
}

/// The functions a face may call, those behind a right only when its manifest has it.
fn linker(engine: &Engine, rights: Rights) -> Result<Linker<Host>, Error> {
    let mut linker = <Linker<Host>>::new(engine);
    if rights.contains(Rights::HID) {
        linker.func_wrap(
            abi::MODULE,
            abi::SEND_USAGE,
            |mut c: Caller<'_, Host>, usage: u32| -> Result<(), Error> {
                let usage = Usage::from_u32(usage)
                    .ok_or_else(|| trap("not a usage the other chip maps"))?;
                let host = c.data_mut();
                if host.drawing {
                    return Err(trap("send_usage from draw"));
                }
                if host.usages.len() < abi::USAGES_MAX {
                    host.usages.push(usage);
                } else {
                    host.dropped += 1;
                }
                Ok(())
            },
        )?;
    }
    if rights.contains(Rights::RANDOM) {
        linker.func_wrap(
            abi::MODULE,
            abi::RANDOM,
            |mut c: Caller<'_, Host>, at: u32, len: u32| -> Result<u32, Error> {
                let len = len as usize;
                let host = c.data_mut();
                if host.drawing {
                    return Err(trap("random from draw"));
                }
                host.random += len;
                if host.random > abi::RANDOM_MAX {
                    return Err(trap("random past RANDOM_MAX in one event"));
                }
                let memory = host.memory.ok_or_else(|| trap("no memory"))?;
                let at = at as usize;
                let buf = memory
                    .data_mut(&mut c)
                    .get_mut(at..at.saturating_add(len))
                    .ok_or_else(|| trap("random outside the face's memory"))?;
                // The radio registers itself with esp-hal as an entropy source while it runs,
                // so `Trng` exists exactly when these bytes carry physical noise.
                Ok(match Trng::try_new() {
                    Ok(trng) => {
                        trng.read(buf);
                        abi::PHYSICAL
                    }
                    Err(_) => {
                        Rng::new().read(buf);
                        abi::PSEUDO
                    }
                })
            },
        )?;
    }
    if rights.contains(Rights::RADIO) {
        linker.func_wrap(
            abi::MODULE,
            abi::NEARBY,
            |mut c: Caller<'_, Host>, radio: u32, at: u32, max: u32| -> Result<u32, Error> {
                let radio = Radio::from_u32(radio).ok_or_else(|| trap("not a radio"))?;
                let host = c.data();
                if host.drawing {
                    return Err(trap("nearby from draw"));
                }
                let memory = host.memory.ok_or_else(|| trap("no memory"))?;
                let at = at as usize;
                let len = (max as usize)
                    .checked_mul(abi::SIGNAL_BYTES)
                    .ok_or_else(|| trap("span overflows"))?;
                let buf = memory
                    .data_mut(&mut c)
                    .get_mut(at..at.saturating_add(len))
                    .ok_or_else(|| trap("nearby outside the face's memory"))?;
                Ok(crate::nearby::copy(radio, buf) as u32)
            },
        )?;
    }
    if rights.contains(Rights::HAPTIC) {
        linker.func_wrap(
            abi::MODULE,
            abi::PULSE,
            |mut c: Caller<'_, Host>, every: u32| -> Result<(), Error> {
                let host = c.data_mut();
                if host.drawing {
                    return Err(trap("pulse from draw"));
                }
                host.pulse =
                    (every != 0).then(|| every.clamp(abi::PULSE_MIN_MS, abi::PULSE_MAX_MS));
                Ok(())
            },
        )?;
    }
    linker.func_wrap(
        abi::MODULE,
        abi::TEXT,
        |mut c: Caller<'_, Host>,
         at: u32,
         len: u32,
         x: i32,
         y: i32,
         size: u32,
         colour: u32|
         -> Result<(), Error> {
            let len = len as usize;
            if len > abi::TEXT_MAX {
                return Err(trap("text longer than TEXT_MAX"));
            }
            let size = Size::from_u32(size).ok_or_else(|| trap("not a text size"))?;
            let paint = paint(colour)?;
            core::str::from_utf8(bytes(&c, at, len)?).map_err(|_| trap("text is not UTF-8"))?;
            record(
                &mut c,
                Draw::Text {
                    at: at as usize,
                    len,
                    point: Point::new(x, y),
                    size,
                    paint,
                },
            )
        },
    )?;
    linker.func_wrap(
        abi::MODULE,
        abi::ARC,
        |mut c: Caller<'_, Host>,
         cx: i32,
         cy: i32,
         radius: u32,
         start: i32,
         sweep: i32,
         width: u32,
         colour: u32|
         -> Result<(), Error> {
            if radius > abi::RADIUS_MAX || width > abi::WIDTH_MAX {
                return Err(trap("arc larger than RADIUS_MAX or WIDTH_MAX"));
            }
            let paint = paint(colour)?;
            record(
                &mut c,
                Draw::Arc {
                    centre: Point::new(cx, cy),
                    radius,
                    start,
                    sweep,
                    width,
                    paint,
                },
            )
        },
    )?;
    linker.func_wrap(
        abi::MODULE,
        abi::ICON,
        |mut c: Caller<'_, Host>, at: u32, x: i32, y: i32, colour: u32| -> Result<(), Error> {
            let paint = paint(colour)?;
            bytes(&c, at, ICON_BYTES)?;
            record(
                &mut c,
                Draw::Icon {
                    at: at as usize,
                    centre: Point::new(x, y),
                    paint,
                },
            )
        },
    )?;
    Ok(linker)
}

fn trap(message: &str) -> Error {
    Error::new(message)
}

/// The `len` bytes at `at` in the face's memory, or a trap if they do not all lie there.
fn bytes<'a>(c: &'a Caller<'_, Host>, at: u32, len: usize) -> Result<&'a [u8], Error> {
    let memory = c.data().memory.ok_or_else(|| trap("no memory"))?;
    let start = at as usize;
    let end = start
        .checked_add(len)
        .ok_or_else(|| trap("span overflows"))?;
    memory
        .data(c)
        .get(start..end)
        .ok_or_else(|| trap("points outside the face's memory"))
}

fn paint(colour: u32) -> Result<Paint, Error> {
    Colour::from_raw(colour)
        .paint()
        .ok_or_else(|| trap("not a colour"))
}

fn record(c: &mut Caller<'_, Host>, draw: Draw) -> Result<(), Error> {
    let host = c.data_mut();
    if !host.drawing {
        return Err(trap("drawing outside draw"));
    }
    if host.draws.len() >= abi::DRAWS_MAX {
        return Err(trap("more drawing calls than DRAWS_MAX"));
    }
    host.draws.push(draw);
    Ok(())
}

/// Draws one kept call. The spans were checked when the call was made and a face's page cannot
/// shrink, so the lookups below cannot miss; they are `get`s so that nothing here has to be
/// trusted to stay that way.
fn render<D>(target: &mut D, memory: &[u8], draw: Draw, palette: &Palette)
where
    D: DrawTarget<Color = Rgb565>,
{
    let _ = match draw {
        Draw::Text {
            at,
            len,
            point,
            size,
            paint,
        } => {
            let Some(line) = memory
                .get(at..at + len)
                .and_then(|b| core::str::from_utf8(b).ok())
            else {
                return;
            };
            // The Latin-1 cuts where there are any: what a face writes comes from outside.
            let font = match size {
                Size::Small => &fonts::SMALL_LATIN1,
                Size::Body => &fonts::BODY_LATIN1,
                Size::Large => &fonts::LARGE,
            };
            text(target, line, point, font, colour(paint, palette))
        }
        Draw::Arc {
            centre,
            radius,
            start,
            sweep,
            width,
            paint,
        } => Arc::with_center(
            centre,
            2 * radius,
            (start as f32).deg(),
            (sweep as f32).deg(),
        )
        .into_styled(PrimitiveStyle::with_stroke(colour(paint, palette), width))
        .draw(target),
        Draw::Icon { at, centre, paint } => {
            let Some(rows) = memory.get(at..at + ICON_BYTES) else {
                return;
            };
            draw_packed(target, rows, centre, colour(paint, palette))
        }
    };
}

fn colour(paint: Paint, palette: &Palette) -> Rgb565 {
    match paint {
        Paint::Rgb565(raw) => RawU16::new(raw).into(),
        Paint::Theme(role) => match role {
            Role::Ring => palette.ring,
            Role::Selected => palette.selected,
            Role::Empty => palette.empty,
            Role::Icon => palette.icon,
            Role::Name => palette.name,
            Role::Value => palette.value,
            Role::Quiet => palette.quiet,
        },
    }
}
