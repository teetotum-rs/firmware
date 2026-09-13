//! The display's QSPI transport as wired on this board.
//!
//! The ST77916 speaks a command-address-data protocol over four data lines: an 8-bit opcode
//! says what kind of transfer follows, a 24-bit address carries the actual register as its
//! middle byte, and only pixel payloads use all four lines. Registers are written one line at a
//! time; only the pixel stream is worth the quad width.
//!
//! # Where the DMA reads from
//!
//! `SpiDmaBus` takes a slice and copies it into its own DMA buffer before the transfer starts.
//! For a register write that is nothing; for a 253 KiB picture that lives in external RAM it is
//! the 7.9 ms it takes to read a screen out of there, paid on every frame, on top of the 6.5 ms
//! the bus itself needs. [`pixels_push_direct`](DisplayBus::pixels_push_direct) is the same
//! transfer with the copy left out: it describes the caller's bytes to the DMA where they lie
//! and lets the controller pull them itself. The GDMA on this chip can read the external RAM,
//! as long as the cache has been written back first -- which is what
//! [`Window::prepare`] does.
//!
//! It does not remove the chunking. The SPI peripheral counts its payload in an 18-bit field,
//! so **no single transfer carries more than 32768 bytes** whichever memory it comes from, and
//! a frame goes out in pieces either way.

use esp_hal::delay::Delay;
use esp_hal::dma::{BurstConfig, DmaDescriptor, DmaTxBuffer, Preparation, TransferDirection};
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Address, Command, DataMode, SpiDmaBus};
use st77916::{ControllerInterface, ResetInterface};

/// What a piece handed to this path has to be a multiple of, in address and in length.
///
/// **This is the whole of what makes the path correct, and it cost a screen full of stripes to
/// find.** Writing the cache back is a line-at-a-time operation: a range that starts or ends
/// inside a line leaves that line where it was, the DMA then reads what the external RAM still
/// held, and the frame arrives with bands of the one before it in it. The data cache of this
/// chip has lines of 16, 32 or 64 bytes depending on how it was brought up, and 64 covers all
/// three. It is also the longest burst the external bus does, so the same number answers both
/// questions.
pub const ALIGN: usize = 64;

/// How much of a buffer one DMA descriptor carries here.
///
/// The size field of a descriptor is twelve bits, so 4095 is the ceiling; 4032 is the largest
/// multiple of [`ALIGN`] below it, which keeps every descriptor's own buffer aligned as well as
/// the piece as a whole.
const DESCRIPTOR_SPAN: usize = 4032;

/// How many descriptors the direct path has, and with it the largest piece it can send.
///
/// Eight spans are 32256 bytes, just under the 32768 one SPI transfer can carry. A piece bigger
/// than this cannot be sent at all; a caller that asks for one is handed back to the copying
/// path rather than a broken picture.
const DESCRIPTORS: usize = 8;

/// The largest piece [`DisplayBus::pixels_push_direct`] will take.
pub const DIRECT_MAX: usize = DESCRIPTORS * DESCRIPTOR_SPAN;

const _: () = assert!(DESCRIPTOR_SPAN.is_multiple_of(ALIGN));

/// Where the internal SRAM starts, seen as data.
///
/// The external RAM is mapped below it on this chip, so this is the whole of the test for
/// "does the DMA have to wait for the cache". Comparing against a constant rather than asking
/// `esp-hal` because the range it keeps for the purpose is not public.
const INTERNAL_RAM: usize = 0x3FC0_0000;

/// The descriptors the direct path lends to the DMA.
///
/// One set, because there is one panel, one SPI peripheral and one transfer in flight: every
/// push waits for its own transfer before it returns, so the descriptors are never wanted by
/// two transfers at once.
static mut PIECE_DESCRIPTORS: [DmaDescriptor; DESCRIPTORS] = [DmaDescriptor::EMPTY; DESCRIPTORS];

/// A piece of memory handed to the DMA where it lies.
///
/// [`DmaTxBuf`](esp_hal::dma::DmaTxBuf) is the same thing for memory it owns, and owning is
/// exactly what will not do here: the bytes are the picture, and the picture is being drawn
/// into between frames. This borrows them for the length of one transfer instead, which is
/// also long enough for the borrow checker to prove that nothing writes to them meanwhile.
struct Window<'a> {
    /// The descriptor chain, laid out afresh for every transfer.
    descriptors: &'a mut [DmaDescriptor],
    /// What goes out. The DMA only ever reads it.
    bytes: &'a [u8],
}

// SAFETY: the descriptors and the bytes they point at are borrowed for as long as the window
// lives, and the window outlives the transfer -- `pixels_push_direct` waits for it before
// dropping either.
unsafe impl DmaTxBuffer for Window<'_> {
    type View = Self;
    type Final = Self;

    fn prepare(&mut self) -> Preparation {
        let spans = self.bytes.len().div_ceil(DESCRIPTOR_SPAN);
        for i in 0..spans {
            let offset = i * DESCRIPTOR_SPAN;
            let length = DESCRIPTOR_SPAN.min(self.bytes.len() - offset);
            let last = i + 1 == spans;
            // Taken before the descriptor itself is borrowed, which is the whole reason this
            // walks indices rather than the slice.
            let next = if last {
                core::ptr::null_mut()
            } else {
                &raw mut self.descriptors[i + 1]
            };
            let descriptor = &mut self.descriptors[i];
            // Cast away const: a transmit descriptor is read by the DMA and never written.
            descriptor.buffer = self.bytes.as_ptr().wrapping_add(offset).cast_mut();
            descriptor.next = next;
            descriptor.set_size(length);
            descriptor.set_length(length);
            descriptor.reset_for_tx(last);
        }

        // The picture is written by the CPU through the cache and read by the DMA off the
        // external bus, so what the cache still holds has to go out first. Internal RAM needs
        // none of this, and the staging buffer of the turned path is internal.
        let external = (self.bytes.as_ptr() as usize) < INTERNAL_RAM;
        if external {
            unsafe { cache_writeback(self.bytes.as_ptr() as u32, self.bytes.len() as u32) };
        }

        Preparation {
            start: self.descriptors.as_mut_ptr(),
            direction: TransferDirection::Out,
            accesses_psram: external,
            // Both halves are `esp-hal`'s own answer, deliberately. What this field sets is
            // not descriptor alignment but the block the DMA reaches the external memory in,
            // and a block that does not match how the cache was brought up is a way to read
            // the wrong bytes, not a way to read them faster.
            burst_transfer: BurstConfig::DEFAULT,
            check_owner: None,
            auto_write_back: false,
        }
    }

    fn into_view(self) -> Self::View {
        self
    }

    fn from_view(view: Self::View) -> Self::Final {
        view
    }
}

/// Writes a range of the data cache back to the external RAM.
///
/// The ROM routine, called the way `esp-hal` calls it for the buffers it owns: autoload is
/// suspended around it so that lines the cache is fetching on its own account do not get
/// written back with it. The symbols come from the ROM linker script.
///
/// # Safety
///
/// The range must be mapped.
///
/// Placed in RAM, the way `esp-hal` places its own copy: the cache is being worked on, and code
/// that has to be fetched through it meanwhile is the one thing that must not happen here.
#[unsafe(link_section = ".rwtext")]
unsafe fn cache_writeback(addr: u32, size: u32) {
    unsafe extern "C" {
        fn rom_Cache_WriteBack_Addr(addr: u32, size: u32);
        fn Cache_Suspend_DCache_Autoload() -> u32;
        fn Cache_Resume_DCache_Autoload(value: u32);
    }
    unsafe {
        let autoload = Cache_Suspend_DCache_Autoload();
        rom_Cache_WriteBack_Addr(addr, size);
        Cache_Resume_DCache_Autoload(autoload);
    }
}

/// Invalidates a range of the data cache, so the next read of it comes off the external bus.
///
/// The companion of [`cache_writeback`], and the other half of [`external_probe`]. Clean lines
/// are simply dropped; a **dirty** line is dropped with its contents, which is the point -- what
/// the CPU sees afterwards is what the external RAM actually holds.
///
/// # Safety
///
/// The range must be mapped, and anything in it that has not been written back is lost.
#[unsafe(link_section = ".rwtext")]
unsafe fn cache_invalidate(addr: u32, size: u32) {
    unsafe extern "C" {
        fn Cache_Invalidate_Addr(addr: u32, size: u32);
        fn Cache_Suspend_DCache_Autoload() -> u32;
        fn Cache_Resume_DCache_Autoload(value: u32);
    }
    unsafe {
        let autoload = Cache_Suspend_DCache_Autoload();
        Cache_Invalidate_Addr(addr, size);
        Cache_Resume_DCache_Autoload(autoload);
    }
}

/// Writes a buffer in external RAM back out of the data cache, exactly as a transfer does.
///
/// The picture is written by the CPU through the cache and read by the DMA off the external
/// bus, so anything that wants to read it the way the DMA does has to push it out first. This
/// is that, for a caller outside this module -- `src/bin/psramreach.rs` asks how far into the
/// external RAM the DMA can read, and it has to write its patterns out before it asks.
///
/// A buffer in internal RAM is left alone: the question does not arise there.
pub fn write_back(bytes: &[u8]) {
    let addr = bytes.as_ptr() as usize;
    if addr >= INTERNAL_RAM {
        return;
    }
    // SAFETY: the range is a live slice, so it is mapped.
    unsafe { cache_writeback(addr as u32, bytes.len() as u32) };
}

/// Asks whether the external RAM holds what the CPU thinks it wrote there.
///
/// The direct path fails in the firmware and works in `src/bin/psramdma.rs`, and after
/// [`Path::Staged`](crate::screen::Path::Staged) put a clean picture on the glass with the same
/// descriptors and the same transfer, two halves were left: either the DMA reads the external
/// RAM wrongly here, or the external RAM does not hold the picture at all and every reader of it
/// would fail -- the DMA is just the only one that does not go through the cache.
///
/// This asks the second half directly, and it needs no eyes. The bytes are summed as the CPU
/// sees them, then written back exactly as a transfer would write them back, then the range is
/// **invalidated** so the cache holds none of it, then summed again off the external bus.
///
/// * The two sums equal: the external RAM holds the picture, and the fault is in the read.
/// * The two sums differ: the write-back does not reach the external RAM here, the DMA was
///   reading whatever was there before, and that is the whole of it.
///
/// Returns `None` for a buffer in internal RAM, where the question does not arise. Costs two
/// passes over the buffer -- about 16 ms for a picture -- so it belongs in a few frames of a
/// measurement, not in every one.
pub fn external_probe(bytes: &[u8]) -> Option<(u32, u32)> {
    let addr = bytes.as_ptr() as usize;
    if addr >= INTERNAL_RAM {
        return None;
    }
    let before = sum32(bytes);
    unsafe {
        cache_writeback(addr as u32, bytes.len() as u32);
        cache_invalidate(addr as u32, bytes.len() as u32);
    }
    let after = sum32(bytes);
    Some((before, after))
}

/// A cheap fingerprint of a buffer: the wrapping sum of its words, position weighted in.
///
/// Weighted because a picture is mostly one colour and a plain sum would call two different
/// arrangements of the same bytes equal.
fn sum32(bytes: &[u8]) -> u32 {
    let mut sum: u32 = 0x811c_9dc5;
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        sum = sum.wrapping_add(word.rotate_left((i % 32) as u32));
    }
    sum
}

/// The display's QSPI transport.
pub struct DisplayBus<'d> {
    /// The bus. Away only for the length of one direct transfer, which needs the halves of it
    /// separately and hands them back before it returns -- see
    /// [`pixels_push_direct`](Self::pixels_push_direct).
    spi: Option<SpiDmaBus<'d, esp_hal::Blocking>>,
    /// Chip select, driven by hand rather than by the SPI peripheral.
    ///
    /// The peripheral would release it after every transfer, and the controller treats each
    /// release as the end of a transaction, so a screen sent that way arrives as thousands of
    /// truncated writes -- which looks like stripes. ESP-IDF sends a frame as a single
    /// transfer with CS held down; holding it here by hand is the same thing.
    cs: Output<'d>,
    /// Whether the pixel write currently open has already carried its RAMWR command.
    opened: bool,
}

impl<'d> DisplayBus<'d> {
    /// The bus as wired: a QSPI peripheral and the chip select we drive ourselves.
    pub fn new(spi: SpiDmaBus<'d, esp_hal::Blocking>, cs: Output<'d>) -> Self {
        Self {
            spi: Some(spi),
            cs,
            opened: false,
        }
    }
}

impl<'d> DisplayBus<'d> {
    /// The bus, which is only ever away from here inside
    /// [`pixels_push_direct`](Self::pixels_push_direct).
    fn bus(&mut self) -> &mut SpiDmaBus<'d, esp_hal::Blocking> {
        self.spi
            .as_mut()
            .expect("the bus is only away during a transfer it waits for")
    }
}

impl DisplayBus<'_> {
    /// Opcode for writing a register, its parameters travelling on one line.
    const WRITE_REGISTER: u16 = 0x02;
    /// Opcode for writing pixels, the payload travelling on all four lines.
    const WRITE_PIXELS: u16 = 0x32;

    /// Changes the bus configuration, which on this board means the clock.
    ///
    /// The door `src/bin/render.rs` needs to send the same frame at four clocks in one run. The
    /// bus itself is not handed out: it is away for the length of a direct transfer, and a
    /// caller holding it then would be holding nothing.
    pub fn apply_config(
        &mut self,
        config: &esp_hal::spi::master::Config,
    ) -> Result<(), esp_hal::spi::master::ConfigError> {
        self.bus().apply_config(config)
    }

    pub fn register(&mut self, cmd: u8, data: &[u8]) -> Result<(), esp_hal::spi::Error> {
        self.cs.set_low();
        let result = self.bus().half_duplex_write(
            DataMode::Single,
            Command::_8Bit(Self::WRITE_REGISTER, DataMode::Single),
            Address::_24Bit(u32::from(cmd) << 8, DataMode::Single),
            0,
            data,
        );
        self.cs.set_high();
        result
    }

    /// Fills the current window by sending `chunk` `repeats` times, CS held down throughout.
    ///
    /// The window must already be set. Only the opening transfer carries the RAMWR command; the
    /// rest are bare data continuing the same write. The caller chooses how much goes into one
    /// transfer, which is deliberately independent of the row length: the transfer size is a
    /// property of the bus, the row length one of the panel, and conflating them made an
    /// earlier measurement unreadable.
    pub fn fill_repeating(
        &mut self,
        chunk: &[u8],
        repeats: usize,
    ) -> Result<(), esp_hal::spi::Error> {
        self.fill_bytes(chunk, chunk.len() * repeats)
    }

    /// Fills the current window with `total` bytes taken from `chunk`, repeated as often as
    /// needed and cut short in the last transfer.
    ///
    /// A rectangle of arbitrary size rarely comes out as a whole number of buffers, and the
    /// remainder cannot be sent as a write of its own: the controller ends the pixel write when
    /// CS goes up, so a fill split across two transactions loses everything after the first.
    /// This keeps CS down for the whole rectangle and lets the last transfer be short.
    pub fn fill_bytes(&mut self, chunk: &[u8], total: usize) -> Result<(), esp_hal::spi::Error> {
        self.cs.set_low();
        let result = self.stream(chunk, total);
        self.cs.set_high();
        result
    }

    /// Sends `data` as the contents of the current window, in transfers of at most `chunk`
    /// bytes, CS held down throughout.
    ///
    /// This is [`fill_bytes`](Self::fill_bytes)'s sibling for a picture that is already
    /// assembled: that one repeats one buffer to cover a rectangle, this one walks a buffer as
    /// long as the rectangle.
    ///
    /// `chunk` cannot exceed the SPI bus's own DMA buffer.
    pub fn send_frame(&mut self, data: &[u8], chunk: usize) -> Result<(), esp_hal::spi::Error> {
        self.pixels_begin();
        let mut result = Ok(());
        for piece in data.chunks(chunk) {
            result = self.pixels_push(piece);
            if result.is_err() {
                break;
            }
        }
        self.pixels_end();
        result
    }

    /// Opens a pixel write into the current window.
    ///
    /// A frame is 253 KiB and no DMA transfer is that large, so it goes out in pieces -- and
    /// the pieces have to stay inside one transaction, because the controller ends the pixel
    /// write when CS goes up. The three calls exist separately from [`send_frame`](Self::send_frame)
    /// because a picture that is turned on its way out is not in memory as a whole: each band
    /// is computed into a staging buffer just before it is pushed, and there is no slice to
    /// hand over.
    ///
    /// Every [`pixels_begin`](Self::pixels_begin) must be followed by a
    /// [`pixels_end`](Self::pixels_end), including when a push has failed.
    pub fn pixels_begin(&mut self) {
        self.cs.set_low();
        self.opened = false;
    }

    /// Sends one more piece of the current pixel write.
    pub fn pixels_push(&mut self, piece: &[u8]) -> Result<(), esp_hal::spi::Error> {
        // Only the opening transfer carries the RAMWR command; the rest are bare data
        // continuing the same write.
        let result = if self.opened {
            self.bus()
                .half_duplex_write(DataMode::Quad, Command::None, Address::None, 0, piece)
        } else {
            self.bus().half_duplex_write(
                DataMode::Quad,
                Command::_8Bit(Self::WRITE_PIXELS, DataMode::Single),
                Address::_24Bit(0x2C << 8, DataMode::Single),
                0,
                piece,
            )
        };
        self.opened = true;
        result
    }

    /// Sends one more piece of the current pixel write, with the DMA reading it where it lies.
    ///
    /// [`pixels_push`](Self::pixels_push) copies the piece into the bus's own DMA buffer
    /// first, which for a picture in external RAM is the larger half of the frame time. This
    /// describes the caller's bytes to the DMA instead. The bytes stay borrowed until the
    /// transfer is over, because the transfer is waited for here.
    ///
    /// Falls back to the copying path for a piece larger than [`DIRECT_MAX`], which is as much
    /// as one SPI transfer carries anyway.
    ///
    /// # It is not usable at 80 MHz, and that is not a bug in here
    ///
    /// An SPI transfer does not wait for its DMA. At 80 MHz over four lines the bus takes
    /// 40 MB/s and the external RAM gives about 32, so the transmit FIFO runs dry and the panel
    /// is handed whatever stood in it -- long runs of one colour. Measured at the glass
    /// by holding the path here and swapping the clock: clean at 40 MHz, striped at
    /// 80. Everything in this function was checked against that and is sound; the source simply
    /// cannot keep up. See [`Path::Direct`](crate::screen::Path::Direct).
    pub fn pixels_push_direct(&mut self, piece: &[u8]) -> Result<(), esp_hal::spi::Error> {
        if piece.is_empty() {
            return Ok(());
        }
        // A piece that is not a whole number of cache lines, or does not start on one, cannot
        // have the cache written back for it in full -- so it goes the copying way rather than
        // out with a band of the previous frame in it. Internal memory is not cached this way
        // and would not need it, but one rule is easier to keep than two.
        if piece.len() > DIRECT_MAX
            || !piece.len().is_multiple_of(ALIGN)
            || !(piece.as_ptr() as usize).is_multiple_of(ALIGN)
        {
            return self.pixels_push(piece);
        }

        // The bus lends out its two halves: a direct transfer owns the peripheral and the
        // buffer together for its duration, and `SpiDmaBus` is exactly those two put back.
        let (spi, rx, tx) = self
            .spi
            .take()
            .expect("the bus is only away during a transfer it waits for")
            .split();

        // Only the opening transfer carries the RAMWR command; the rest are bare data
        // continuing the same write.
        let (command, address) = if self.opened {
            (Command::None, Address::None)
        } else {
            (
                Command::_8Bit(Self::WRITE_PIXELS, DataMode::Single),
                Address::_24Bit(0x2C << 8, DataMode::Single),
            )
        };

        // SAFETY: there is one panel, one SPI peripheral and one transfer in flight, and this
        // call waits for its own transfer before it returns, so no second borrow of the
        // descriptors can exist while these are in use.
        let descriptors = unsafe { &mut *core::ptr::addr_of_mut!(PIECE_DESCRIPTORS) };
        let window = Window {
            descriptors,
            bytes: piece,
        };

        match spi.half_duplex_write(DataMode::Quad, command, address, 0, piece.len(), window) {
            Ok(transfer) => {
                let (spi, _window) = transfer.wait();
                self.spi = Some(spi.with_buffers(rx, tx));
                self.opened = true;
                Ok(())
            }
            Err((error, spi, _window)) => {
                self.spi = Some(spi.with_buffers(rx, tx));
                Err(error)
            }
        }
    }

    /// Ends the pixel write, which is what tells the controller the picture is complete.
    pub fn pixels_end(&mut self) {
        self.cs.set_high();
        self.opened = false;
    }

    fn stream(&mut self, chunk: &[u8], total: usize) -> Result<(), esp_hal::spi::Error> {
        if chunk.is_empty() {
            return Ok(());
        }
        let mut sent = 0;
        while sent < total {
            let piece = &chunk[..chunk.len().min(total - sent)];
            if sent == 0 {
                self.bus().half_duplex_write(
                    DataMode::Quad,
                    Command::_8Bit(Self::WRITE_PIXELS, DataMode::Single),
                    Address::_24Bit(0x2C << 8, DataMode::Single),
                    0,
                    piece,
                )?;
            } else {
                self.bus().half_duplex_write(
                    DataMode::Quad,
                    Command::None,
                    Address::None,
                    0,
                    piece,
                )?;
            }
            sent += piece.len();
        }
        Ok(())
    }
}

impl ControllerInterface for DisplayBus<'_> {
    type Error = esp_hal::spi::Error;

    fn send_command(&mut self, cmd: u8) -> Result<(), Self::Error> {
        self.register(cmd, &[])
    }

    fn send_command_with_data(&mut self, cmd: u8, data: &[u8]) -> Result<(), Self::Error> {
        self.register(cmd, data)
    }

    fn send_pixels(&mut self, pixels: &[u8]) -> Result<(), Self::Error> {
        self.fill_repeating(pixels, 1)
    }
}

/// The display's reset line.
pub struct DisplayReset<'d> {
    pub pin: Output<'d>,
    pub delay: Delay,
}

impl ResetInterface for DisplayReset<'_> {
    type Error = core::convert::Infallible;

    fn reset(&mut self) -> Result<(), Self::Error> {
        // Datasheet 7.4.7 asks for at least 10 us low and 120 ms before the panel accepts
        // commands. Both are minima, and running at them produced a panel that initialised on
        // some boots and not on others with identical code. These are deliberately generous.
        self.pin.set_low();
        self.delay.delay_millis(20);
        self.pin.set_high();
        self.delay.delay_millis(200);
        Ok(())
    }
}
