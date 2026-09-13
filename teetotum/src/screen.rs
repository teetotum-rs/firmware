//! The glass as one object: the bus, the panel, the picture, and the way out.
//!
//! Everything this module does was measured in `src/bin/render.rs` and judged in
//! `src/bin/turn.rs`, and until now it lived in each of those runs separately -- eighty lines of
//! PSRAM, DMA, SPI and panel bring-up, copied from bin to bin. Copied code is not the problem;
//! **copied constants are**: the 80 MHz that made the panel four times faster was measured
//! and was still nowhere in the firmware, which went on driving the glass at the
//! 10 MHz that was never a decision but the first value that worked. A number that has been
//! measured belongs in one place, and this is that place.
//!
//! What a caller sees is a picture and an angle: draw into [`Screen::frame`] with
//! `embedded-graphics`, say how the device is being held with [`Screen::set_orientation`], and
//! call [`Screen::present`]. How the turn is done -- three bits in the panel controller or
//! 129600 samples through the staging buffer -- is this module's business and nobody else's.
//!
//! # The quarters are free, and that is worth an if
//!
//! Turning the picture costs 47 ms with nearest and 145 with bilinear, against 7 ms for a
//! picture that goes out as it lies. But MADCTL (36h) has three geometry bits -- mirror X,
//! mirror Y, exchange axes -- so a quarter turn is a register write and costs **nothing**: four
//! of the twelve detents are had for one command, and 90 degrees through the arithmetic is the
//! *slowest* nearest case there is (38.5 ms, because consecutive output pixels walk down a
//! source column and every one of them pulls a fresh cache line out of the PSRAM).
//!
//! The composition is on paper below, and paper is exactly what got the direction of
//! [`rotate_rows`] wrong the first time. So it is checkable in front of the glass:
//! [`Screen::set_quarters`] switches the free path off and makes the same angle come out of the
//! arithmetic instead. At a multiple of 90 degrees the two must be **indistinguishable** -- if
//! the picture jumps when the path is switched, the table below is wrong, not the eye.
//!
//! # The finger comes back the same way
//!
//! A picture turned by [`Screen::set_orientation`] needs the turn undone on anything the user
//! points at, and [`Screen::picture_point`] is that: hand it where the finger is in the
//! viewer's coordinates -- [`Contact::in_view`](crate::touch::Contact::in_view) puts it there,
//! since the touch controller reports in the mounting frame -- and it says which pixel of the
//! picture was under it. Two turns, each owned by the module that measured it, and neither of
//! them knows about the other.
//!
//! Judged in `src/bin/finger.rs` and it held: a stroke drawn on a **turned**
//! picture grows under the fingertip, not a quarter turn away from it. Drawing upright would
//! not have said anything -- the ink lies in the framebuffer and turns with it either way.

use core::convert::Infallible;
use core::sync::atomic::{AtomicBool, Ordering};

use esp_hal::delay::Delay;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{AnyPin, Level, Output, OutputConfig};
use esp_hal::peripherals::{DMA_CH0, PSRAM, SPI2};
use esp_hal::psram::{Psram, PsramConfig, PsramMode};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use log::info;
use st77916::{ColorMode, DisplaySize, DriverError, St77916};

use crate::display::{DisplayBus, DisplayReset};
use crate::framebuffer::{BYTES, Framebuffer, HEIGHT, WIDTH};
use crate::panel::{INIT_COMMANDS, PANEL_MOUNT_MADCTL, POST_INIT_COMMANDS};
use crate::rotate::{self, Filter, STEPS, rotate_rows};

/// The clock the panel is driven at.
///
/// Measured with `src/bin/render.rs`: 10, 20, 40 and 80 MHz all arrive cleanly,
/// and each one was looked at on the glass. 80 turns 16 frames a second into 69, which makes
/// this the largest single number in the project.
pub const CLOCK: Rate = Rate::from_mhz(80);

/// Rows turned into the staging buffer before it is pushed.
///
/// The band length is a property of the bus, not of the panel: it is how much is in flight at
/// once, and 30 rows is 21600 bytes, comfortably inside what one DMA transfer carries.
pub const ROWS_PER_BAND: usize = 30;

/// Bytes in one band, which is also the DMA transfer size.
pub const BAND_BYTES: usize = WIDTH * 2 * ROWS_PER_BAND;

/// Bytes of the picture in one transfer of the direct path.
///
/// Larger than a band on purpose: nothing is assembled here, so the only thing that sets the
/// size is what one SPI transfer can carry -- 32768 bytes, the 18-bit payload counter of the
/// peripheral -- and what the cache can be written back in, which is whole lines of at most
/// [`ALIGN`](crate::display::ALIGN) bytes. 40 rows are 28800 bytes, nine of them are the
/// picture exactly, and 28800 is 450 lines of 64, so every piece starts and ends on one.
pub const DIRECT_BYTES: usize = WIDTH * 2 * 40;

const _: () = assert!(DIRECT_BYTES <= crate::display::DIRECT_MAX);
const _: () = assert!(BYTES.is_multiple_of(DIRECT_BYTES));
const _: () = assert!(DIRECT_BYTES.is_multiple_of(crate::display::ALIGN));

/// Bytes of the picture in one transfer of the staged path.
///
/// Smaller than a direct piece because it has to fit in the staging buffer as well as in one
/// transfer: 24 rows are 17280 bytes, 270 lines of 64, and fifteen of them are the picture
/// exactly.
pub const STAGED_BYTES: usize = WIDTH * 2 * 24;

const _: () = assert!(STAGED_BYTES <= BAND_BYTES);
const _: () = assert!(STAGED_BYTES <= crate::display::DIRECT_MAX);
const _: () = assert!(BYTES.is_multiple_of(STAGED_BYTES));
const _: () = assert!(STAGED_BYTES.is_multiple_of(crate::display::ALIGN));

/// MADCTL for the four orientations the controller can do by itself, at 0, 90, 180 and 270
/// degrees clockwise.
///
/// Derived rather than guessed, from how the controller maps the pixel stream onto the panel.
/// Writing `(i, j)` for the pixel's place in the stream and `(px, py)` for where it lands:
///
/// ```text
/// MV clear:  px = MX ? W-1-i : i      py = MY ? H-1-j : j
/// MV set:    px = MX ? W-1-j : j      py = MY ? H-1-i : i
/// ```
///
/// The glass is fitted upside down, so the viewer's coordinates are the panel's turned by 180
/// -- that is [`PANEL_MOUNT_MADCTL`], and it is the entry for zero here. A picture turned a
/// quarter clockwise wants the viewer to see `(W-1-j, i)`, which on the panel is `(j, H-1-i)`:
/// axes exchanged, Y mirrored, X not. Hence `MV|MY`. The other two follow the same way.
///
/// **This is arithmetic on paper, and the last piece of arithmetic on paper here had the
/// rotation going the wrong way round.** [`Screen::set_quarters`] exists so the glass can say.
const QUARTER_MADCTL: [u8; 4] = [
    PANEL_MOUNT_MADCTL, // 0 degrees: the mount, uncorrected further
    0xA0,               // 90 degrees clockwise: MV | MY
    0x00,               // 180 degrees: the mount undone
    0x60,               // 270 degrees clockwise: MV | MX
];

/// Memory access control, the register the four free orientations live in.
const MADCTL: u8 = 0x36;

/// Where a band of turned rows is assembled.
///
/// Internal RAM on purpose: the rotating blit writes it pixel by pixel, and the external RAM is
/// the wrong place for that -- reading a screen out of there costs 7.9 ms, writing one 13.2.
/// Aligned to a cache line, which the rotating blit does not care about and
/// [`Path::Staged`] does: a piece handed to the DMA has to start on one.
#[repr(C, align(64))]
struct Staging([u8; BAND_BYTES]);

static mut STAGING: Staging = Staging([0; BAND_BYTES]);

const _: () = assert!(align_of::<Staging>() >= crate::display::ALIGN);

/// Whether the one screen this board has has already been taken.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// What can go wrong on the way to a picture.
#[derive(Debug)]
pub enum Error {
    /// A second screen was asked for. There is one panel and one staging buffer.
    Twice,
    /// The external RAM is smaller than a frame, so there is nowhere to draw.
    NoRoom {
        /// How much external RAM the chip mapped.
        found: usize,
        /// How much a 360x360 RGB565 picture needs.
        needed: usize,
    },
    /// The SPI peripheral refused the configuration.
    Spi(esp_hal::spi::master::ConfigError),
    /// The panel refused its initialisation sequence.
    Panel(DriverError<esp_hal::spi::Error, Infallible>),
    /// A transfer to the panel failed.
    Bus(esp_hal::spi::Error),
    /// The driver rejected a command as ill-formed.
    Command(&'static str),
}

/// The pins the glass is wired to, as pins rather than as a board.
///
/// They are [`AnyPin`] so that this module does not name GPIO numbers: which pin carries the
/// clock is a property of the board, and the board is described where the peripherals are
/// handed out. On this one it is SCK 13, SIO0 15, SIO1 16, SIO2 17, SIO3 18, CS 14, reset 21,
/// backlight 47.
pub struct ScreenPins<'d> {
    /// QSPI clock.
    pub sck: AnyPin<'d>,
    /// The four data lines. Registers travel on the first, pixels on all four.
    pub sio0: AnyPin<'d>,
    /// See [`sio0`](Self::sio0).
    pub sio1: AnyPin<'d>,
    /// See [`sio0`](Self::sio0).
    pub sio2: AnyPin<'d>,
    /// See [`sio0`](Self::sio0).
    pub sio3: AnyPin<'d>,
    /// Chip select, driven by hand -- see [`DisplayBus`].
    pub cs: AnyPin<'d>,
    /// The panel's reset line.
    pub reset: AnyPin<'d>,
    /// The backlight, held high for as long as the screen exists -- or `None` when the caller
    /// drives it itself, to dim it.
    pub backlight: Option<AnyPin<'d>>,
}

/// Which way a piece of the picture reaches the bus.
///
/// Three, because one of them is faster than the memory it reads from and the other two are
/// not. See [`Screen::set_path`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Path {
    /// `SpiDmaBus` copies every piece into its own DMA buffer first. 14.4 ms a frame, and the
    /// only path that has ever put a clean picture on the glass out of the firmware.
    Copied,
    /// The DMA reads the piece out of the external RAM where it lies. 6.6 ms a frame, and
    /// **stripes at 80 MHz, everywhere, because the bus drains faster than the memory fills
    /// it**: 40 MB/s out against about 32 in. An SPI transfer does not wait for its DMA, so
    /// what a dry FIFO holds goes out as pixels. Clean at 40 MHz, and at 40 MHz it saves
    /// nothing. See [`Screen::set_path`].
    Direct,
    /// The piece is copied into the internal staging buffer and the DMA reads it from there.
    ///
    /// Slower than either of the others -- it pays the read out of the external RAM *and* a
    /// write into internal RAM -- and it was never here to be fast. It is the same descriptor
    /// chain, the same borrowed-buffer window and the same transfer as [`Direct`](Self::Direct)
    /// with only the source moved, so what it shows on the glass says which half of the direct
    /// path is wrong.
    ///
    /// **It showed a picture** (at the glass, the whole firmware: cloud, ring, Home,
    /// a settings dialog opened and taken back; 16.1 frames a second against 17 to 18 copying).
    /// That cleared the mechanism, and what was left turned out to be the rate: internal RAM
    /// feeds the bus and the external RAM does not. Kept because it is the shape every further
    /// cut is measured against.
    Staged,
}

/// The panel, the picture that is sent to it, and the angle in between.
pub struct Screen<'d> {
    display: St77916<DisplayBus<'d>, DisplayReset<'d>>,
    frame: Framebuffer,
    staging: &'static mut [u8],
    /// A second screen in the external RAM, held behind the picture.
    ///
    /// A background off the TF card costs about 130 ms to read, which is eight frames a second
    /// if every frame re-reads it -- so it is read once into here, and a frame that wants it
    /// starts with a copy instead of a clear. The copy is 253 KiB out of external RAM and back
    /// into it, about 21 ms at the measured 32 and 19 MB/s, against 13 ms for the clear it
    /// replaces. `None` only if the chip mapped less than two screens' worth.
    backdrop: Option<&'static mut [u8]>,
    /// The external RAM behind the two screens, handed out once and never used here.
    ///
    /// Eight megabytes came up and half a megabyte of it is pictures. The rest is the largest
    /// block of memory on the board by a wide margin, and the only place a decoded photograph
    /// or a plugin's own data can live -- see [`take_spare`](Self::take_spare).
    spare: Option<&'static mut [u8]>,
    /// Which of the twelve orientations the picture is shown at.
    step: usize,
    /// Which filter the arithmetic path uses.
    filter: Filter,
    /// Whether the quarter turns are taken from the controller instead of computed.
    quarters: bool,
    /// Which way a piece of the picture reaches the bus.
    ///
    /// **[`Path::Copied`] until the striped glass is understood** -- see
    /// [`set_path`](Self::set_path).
    path: Path,
    /// What the controller was last told, so a present that changes nothing writes nothing.
    madctl: u8,
    /// Held so the pin keeps driving; the backlight is off the moment it is dropped.
    _backlight: Option<Output<'d>>,
    /// Held so the external RAM stays mapped for as long as the picture lives in it.
    _psram: Psram,
}

impl Screen<'static> {
    /// Brings up the external RAM, the bus and the panel, and hands back a blank picture.
    ///
    /// The picture is *not* sent: nothing reaches the glass before the first
    /// [`present`](Self::present), so a caller can draw its first screen without a flash of
    /// whatever the panel powered up with.
    ///
    /// # Errors
    ///
    /// If it is called twice, if the external RAM is too small for a frame, or if the SPI
    /// peripheral or the panel refuse their configuration.
    pub fn new(
        psram_peripheral: PSRAM<'static>,
        spi_peripheral: SPI2<'static>,
        dma_channel: DMA_CH0<'static>,
        pins: ScreenPins<'static>,
        delay: Delay,
    ) -> Result<Self, Error> {
        Self::new_skipping(
            psram_peripheral,
            spi_peripheral,
            dma_channel,
            pins,
            delay,
            0,
        )
    }

    /// Like [`new`](Self::new), with the picture pushed `skip` bytes further into the external
    /// RAM.
    ///
    /// This exists for one question and has one caller, `src/bin/psramdma.rs`. Where the
    /// picture lies is a function of how big the application is -- the flash's read-only data
    /// is mapped into the same address space ahead of the external RAM window -- so the
    /// measurement run holds the picture at `0x3c020000` and the firmware at `0x3c1a0000`, and
    /// for a while it looked as though the direct path minded which. `skip` puts the
    /// measurement run's picture at the firmware's address without making the measurement run
    /// as big as the firmware, and the answer it gave was **no**: the direct path
    /// stripes at both addresses, and what it minds is the clock. Kept because a question about
    /// where the picture lies is cheap to ask again with it.
    ///
    /// `skip` is rounded up to a whole cache line, so the picture keeps the alignment every
    /// other caller gets. Everything else -- the backdrop, the spare -- follows the picture as
    /// usual, so a long `skip` is paid out of the spare.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new), and [`Error::NoRoom`] if `skip` leaves no room for a picture.
    pub fn new_skipping(
        psram_peripheral: PSRAM<'static>,
        spi_peripheral: SPI2<'static>,
        dma_channel: DMA_CH0<'static>,
        pins: ScreenPins<'static>,
        delay: Delay,
        skip: usize,
    ) -> Result<Self, Error> {
        if TAKEN.swap(true, Ordering::Relaxed) {
            return Err(Error::Twice);
        }
        let mut delay = delay;

        let psram = Psram::new(
            psram_peripheral,
            PsramConfig {
                mode: PsramMode::OctalSpi,
                ..Default::default()
            },
        );
        let (psram_start, psram_size) = psram.raw_parts();
        // Where the window begins is not a property of the chip: the flash's read-only data is
        // mapped into the same address space ahead of it, so a bigger application pushes the
        // picture further along. It is logged because the direct path works in one program and
        // stripes in another, and this is one of the few things that differ between them by
        // construction.
        info!(
            "Screen: {} KiB of external RAM from {:p}",
            psram_size / 1024,
            psram_start
        );
        let skip = skip.next_multiple_of(64);
        if psram_size < skip + BYTES {
            return Err(Error::NoRoom {
                found: psram_size,
                needed: skip + BYTES,
            });
        }
        // SAFETY: the PSRAM is mapped, this runs once, and nothing else has been handed a
        // pointer into it -- the allocator's heaps are internal RAM.
        let memory: &'static mut [u8] =
            unsafe { core::slice::from_raw_parts_mut(psram_start, psram_size) };
        // Whatever is skipped is never handed out again: it is below the picture, and the only
        // caller that skips anything is asking what the picture's address is worth.
        let (_skipped, memory) = memory.split_at_mut(skip);
        let memory_len = memory.len();
        if skip != 0 {
            info!(
                "Screen: the picture is {skip} bytes into the window, at {:p}",
                memory.as_ptr()
            );
        }
        // The picture takes the first screen's worth; a second one behind it, if the chip
        // mapped enough, is the backdrop. Nothing else is handed any of this memory, so the
        // split is the whole of the bookkeeping.
        let (front, rest) = memory.split_at_mut(BYTES);
        let frame = Framebuffer::new(front).ok_or(Error::NoRoom {
            found: memory_len,
            needed: BYTES,
        })?;
        let (backdrop, spare) = if rest.len() >= BYTES {
            let (second, left) = rest.split_at_mut(BYTES);
            (Some(second), left)
        } else {
            (None, rest)
        };
        let spare = if spare.is_empty() { None } else { Some(spare) };

        let backlight = pins
            .backlight
            .map(|pin| Output::new(pin, Level::High, OutputConfig::default()));

        // The panel is only ever written to; the read side exists because `SpiDmaBus` insists
        // on both halves, and gets the smallest buffer the macro will make.
        let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(1, BAND_BYTES);
        let dma_rx =
            DmaRxBuf::new(rx_descriptors, rx_buffer).expect("the DMA read buffer is malformed");
        let dma_tx =
            DmaTxBuf::new(tx_descriptors, tx_buffer).expect("the DMA write buffer is malformed");

        let spi = Spi::new(spi_peripheral, SpiConfig::default().with_frequency(CLOCK))
            .map_err(Error::Spi)?
            .with_sck(pins.sck)
            .with_sio0(pins.sio0)
            .with_sio1(pins.sio1)
            .with_sio2(pins.sio2)
            .with_sio3(pins.sio3)
            .with_dma(dma_channel)
            .with_buffers(dma_rx, dma_tx);

        let reset = DisplayReset {
            pin: Output::new(pins.reset, Level::High, OutputConfig::default()),
            delay,
        };
        let bus = DisplayBus::new(
            spi,
            Output::new(pins.cs, Level::High, OutputConfig::default()),
        );

        let mut display =
            St77916::builder(bus, reset, DisplaySize::new(WIDTH as u16, HEIGHT as u16))
                .with_init_commands(INIT_COMMANDS)
                .build(ColorMode::Rgb565, &mut delay)
                .map_err(Error::Panel)?;

        // The vendor sequence ends with SLPOUT and DISPON, whose own delays are minima; running
        // at them gave a panel that initialised on some boots and not on others.
        delay.delay_millis(150);
        for &(cmd, data, wait) in POST_INIT_COMMANDS {
            display
                .send_command_with_data(cmd, data)
                .map_err(Error::Panel)?;
            delay.delay_millis(u32::from(wait));
        }
        display
            .set_window(0, 0, WIDTH as u16 - 1, HEIGHT as u16 - 1)
            .map_err(Error::Panel)?;

        // SAFETY: `TAKEN` above makes this the only reference that will ever be handed out.
        let staging: &'static mut [u8] = unsafe { &mut *core::ptr::addr_of_mut!(STAGING.0) };

        Ok(Self {
            display,
            frame,
            staging,
            backdrop,
            spare,
            step: 0,
            filter: Filter::Nearest,
            quarters: true,
            path: Path::Copied,
            madctl: PANEL_MOUNT_MADCTL,
            _backlight: backlight,
            _psram: psram,
        })
    }
}

impl Screen<'_> {
    /// The picture, to draw into. Nothing drawn here shows before [`present`](Self::present).
    pub fn frame(&mut self) -> &mut Framebuffer {
        &mut self.frame
    }

    /// The backdrop as bytes to be **filled**, in the panel's own order.
    ///
    /// Same door as [`Framebuffer::bytes_mut`] and for the same kind of content: a picture that
    /// already exists in exactly this form, streamed in from somewhere. What is put here is not
    /// shown by putting it here -- [`restore`](Self::restore) is what brings it into the
    /// picture.
    ///
    /// `None` if the external RAM has room for one screen but not two.
    pub fn backdrop_mut(&mut self) -> Option<&mut [u8]> {
        self.backdrop.as_deref_mut()
    }

    /// Keeps the picture as the backdrop, and says whether there was room for one.
    ///
    /// The other half of [`restore`](Self::restore), and the door for a background that was
    /// *computed* rather than streamed in: a decoded photograph costs up to 587 ms to scale
    /// down (see [`crate::image`]) and 21 ms to copy back, so anything drawn over it wants the
    /// copy and not the scaler. What is here is not shown by being here; the next `restore` is
    /// what brings it back.
    pub fn stash(&mut self) -> bool {
        let Some(backdrop) = self.backdrop.as_deref_mut() else {
            return false;
        };
        backdrop.copy_from_slice(self.frame.bytes());
        true
    }

    /// Lays the backdrop over the picture, and says whether there was one.
    ///
    /// This is the opening move of a frame that has a background: it replaces the clear, and
    /// everything drawn afterwards lands on top of it.
    pub fn restore(&mut self) -> bool {
        let Some(backdrop) = self.backdrop.as_deref() else {
            return false;
        };
        self.frame.bytes_mut().copy_from_slice(backdrop);
        true
    }

    /// The external RAM that is not a picture, handed over once.
    ///
    /// The screen brought the PSRAM up, so the screen is what owns it, and everything else on
    /// the board has to ask. It gives the whole of the remainder to the first caller and
    /// `None` to every one after that: a second owner of the same megabytes is a fault that
    /// shows up as a photograph with a font in the middle of it.
    ///
    /// What it is for is anything too big for the internal RAM. A decoded JPEG is the first
    /// case (see [`crate::image`]); handing it to `esp_alloc` as a third heap region, after
    /// the internal ones so that small allocations stay off the slow bus, is the other.
    pub fn take_spare(&mut self) -> Option<&'static mut [u8]> {
        self.spare.take()
    }

    /// Which of the twelve orientations the picture is shown at.
    pub fn orientation(&self) -> usize {
        self.step
    }

    /// Shows the picture turned by `step` * 30 degrees clockwise from here on.
    ///
    /// # Panics
    ///
    /// If `step` is not below [`STEPS`].
    pub fn set_orientation(&mut self, step: usize) {
        assert!(step < STEPS, "there are only twelve orientations");
        self.step = step;
    }

    /// Sets the clock the panel is driven at.
    ///
    /// [`CLOCK`] unless told otherwise. The one caller is `src/bin/psramdma.rs`, asking whether
    /// the direct path's stripes are a bus that drains faster than the external RAM fills it:
    /// 253 KiB in 6.5 ms is 39 MB/s out, and reading the external RAM is about 32 MB/s. If that
    /// is what it is, the same picture is clean at half the clock and striped at the full one.
    ///
    /// # Errors
    ///
    /// If the SPI peripheral refuses the rate.
    pub fn set_clock(&mut self, rate: Rate) -> Result<(), Error> {
        self.display
            .interface_mut()
            .apply_config(&SpiConfig::default().with_frequency(rate))
            .map_err(Error::Spi)
    }

    /// Which filter the turned path samples with. [`Filter::Nearest`] unless told otherwise,
    /// and that is a judgement about interfaces -- see [`Filter::Bilinear`].
    pub fn filter(&self) -> Filter {
        self.filter
    }

    /// Sets the filter for the turned path.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
    }

    /// Whether the quarter turns are taken from the controller (the default) or computed like
    /// every other angle.
    pub fn quarters(&self) -> bool {
        self.quarters
    }

    /// Turns the free quarter turns off, or back on.
    ///
    /// Off, a quarter turn goes through [`rotate_rows`] like the eight angles in between: three
    /// times slower and pixel for pixel the same picture. That equality is the point -- it is
    /// how [`QUARTER_MADCTL`] gets checked against something other than the paper it was
    /// derived on.
    pub fn set_quarters(&mut self, quarters: bool) {
        self.quarters = quarters;
    }

    /// Which way a piece of the picture reaches the bus.
    pub fn path(&self) -> Path {
        self.path
    }

    /// Chooses the way a piece of the picture reaches the bus.
    ///
    /// [`Path::Copied`] is the default and the only one worth having. Every piece goes into the
    /// bus's own DMA buffer first, which is what `SpiDmaBus` does for any slice handed to it and
    /// costs the 7.9 ms a screen takes to read out of the external RAM, so a `present` is
    /// 14.4 ms.
    ///
    /// [`Path::Direct`] is 6.6 ms and **wrong**. It stripes, and the reason came
    /// out: 259200 bytes over four lines at 80 MHz leave the bus at 40 MB/s, and the external
    /// RAM is read at about 32. An SPI transfer does not wait for its DMA -- once the
    /// transaction is running the clock runs, and a dry transmit FIFO sends whatever stood in it
    /// last. That is the thick bands of one colour on the glass, and it is why a nearly black
    /// scene hid it for days: a repeated band of black looks like black.
    ///
    /// It was found by holding the path at `Direct` and swapping the *clock* every two seconds:
    /// clean at 40 MHz, striped at 80 (at the glass). The address had nothing to do with it --
    /// the same picture stripes at `0x3c020000` and at the firmware's `0x3c1a0000` -- and
    /// neither did the alignment, the burst size or the cache writeback, all three of which were
    /// tried at 80 MHz where it fails whatever they say.
    ///
    /// At 40 MHz the direct path is 13.0 ms of bus with no copy against 14.4 ms copying at
    /// 80 MHz: eight per cent, for half the clock. So this is not a lever, and the 7.9 ms is the
    /// price of a memory that cannot feed this bus.
    ///
    /// [`Path::Staged`] was the cut that cleared the mechanics.
    pub fn set_path(&mut self, path: Path) {
        self.path = path;
    }

    /// Which pixel of the picture is under a point on the turned glass.
    ///
    /// The point goes in as the viewer's coordinates -- the frame the picture is drawn in while
    /// it stands upright, which is where [`Contact::in_view`](crate::touch::Contact::in_view)
    /// leaves a finger -- and comes back in the picture's own, whatever the orientation.
    ///
    /// It can land **outside** the picture: the corners of a turned square come from nowhere,
    /// and on round glass a touch near the rim is the ordinary case. What an outside touch means
    /// is the caller's business, so it is handed back as it is rather than clamped.
    pub fn picture_point(&self, x: i32, y: i32) -> (i32, i32) {
        rotate::source_point(self.step, x, y)
    }

    /// How many quarter turns a named direction has to come back, at this orientation.
    ///
    /// For [`Gesture::in_picture`](crate::touch::Gesture::in_picture), which turns a slide the
    /// way [`picture_point`](Self::picture_point) turns a point -- except that four names
    /// cannot resolve thirty degrees, so it rounds. See
    /// [`rotate::source_quarter`](crate::rotate::source_quarter).
    pub fn picture_quarter(&self) -> usize {
        rotate::source_quarter(self.step)
    }

    /// Whether the next [`present`](Self::present) is one the controller does for free.
    ///
    /// True upright, and at a quarter turn while [`quarters`](Self::quarters) is on. It is here
    /// so a caller can say which path ran without repeating the condition -- and a run that
    /// compares the two paths has to be able to say it.
    pub fn free(&self) -> bool {
        self.step == 0 || (self.quarters && self.step % (STEPS / 4) == 0)
    }

    /// Sends the picture to the glass at the current orientation.
    ///
    /// Where the turn is free -- upright, or a quarter turn with [`quarters`](Self::quarters)
    /// on -- it costs **6.6 ms** with the picture standing still and **7.5 ms** after the whole
    /// of it has been redrawn, the difference being the cache written back ahead of the DMA.
    /// That is the bus and almost nothing else: 253 KiB down four lines at 80 MHz is 6.5 ms.
    /// Measured with `src/bin/psramdma.rs`; before the DMA read the picture where
    /// it lay it was 14.4 ms, the copy into the bus's own buffer being the other half.
    ///
    /// Turned it is 47 ms with nearest, and the direct path saves 0.7 of them: what is timed
    /// there is the arithmetic, not the bus.
    ///
    /// # Errors
    ///
    /// If the bus rejects a transfer.
    pub fn present(&mut self) -> Result<(), Error> {
        let free = self.free();
        let wanted = if free {
            QUARTER_MADCTL[self.step / (STEPS / 4)]
        } else {
            PANEL_MOUNT_MADCTL
        };
        if wanted != self.madctl {
            self.set_madctl(wanted)?;
        }

        if free {
            // The bus and the picture are separate fields, so both can be borrowed at once.
            let Self {
                display,
                frame,
                staging,
                path,
                ..
            } = self;
            let bus = display.interface_mut();
            if *path == Path::Copied {
                return bus
                    .send_frame(frame.bytes(), BAND_BYTES)
                    .map_err(Error::Bus);
            }
            let piece_bytes = if *path == Path::Direct {
                DIRECT_BYTES
            } else {
                STAGED_BYTES
            };
            bus.pixels_begin();
            let mut result = Ok(());
            for piece in frame.bytes().chunks(piece_bytes) {
                result = if *path == Path::Direct {
                    bus.pixels_push_direct(piece)
                } else {
                    // The same transfer with the source moved: the copy out of the external RAM
                    // is the CPU's here rather than the DMA's.
                    staging[..piece.len()].copy_from_slice(piece);
                    bus.pixels_push_direct(&staging[..piece.len()])
                }
                .map_err(Error::Bus);
                if result.is_err() {
                    break;
                }
            }
            bus.pixels_end();
            return result;
        }

        let Self {
            display,
            frame,
            staging,
            step,
            filter,
            path,
            ..
        } = self;
        let bus = display.interface_mut();
        bus.pixels_begin();
        let mut result = Ok(());
        for band in 0..HEIGHT / ROWS_PER_BAND {
            rotate_rows(
                frame,
                *step,
                *filter,
                band * ROWS_PER_BAND,
                ROWS_PER_BAND,
                staging,
            );
            // The staging buffer is internal RAM, so the direct path saves only the copy into
            // the bus's own buffer here -- a smaller saving than on the picture itself, and the
            // same switch turns it off.
            result = if *path == Path::Copied {
                bus.pixels_push(staging)
            } else {
                bus.pixels_push_direct(staging)
            }
            .map_err(Error::Bus);
            if result.is_err() {
                break;
            }
        }
        bus.pixels_end();
        result
    }

    /// Writes MADCTL and re-states the window, which the controller reads through the new
    /// mapping.
    fn set_madctl(&mut self, value: u8) -> Result<(), Error> {
        self.display
            .send_command_with_data(MADCTL, &[value])
            .map_err(driver_error)?;
        self.display
            .set_window(0, 0, WIDTH as u16 - 1, HEIGHT as u16 - 1)
            .map_err(driver_error)?;
        self.madctl = value;
        Ok(())
    }
}

/// A driver error, unpacked. The reset line cannot fail, which is what `Infallible` says.
fn driver_error(err: DriverError<esp_hal::spi::Error, Infallible>) -> Error {
    match err {
        DriverError::InterfaceError(err) => Error::Bus(err),
        DriverError::ResetError(never) => match never {},
        DriverError::InvalidConfiguration(what) => Error::Command(what),
    }
}
