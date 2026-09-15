//! A whole screen held in external RAM, drawn into with `embedded-graphics`.
//!
//! Everything before this module wrote pixels straight at the panel: set a window, stream a
//! colour, done. That is enough to prove a bus works and not enough to draw anything, because
//! a picture is built up out of overlapping pieces and the panel has no memory we can read
//! back. So the picture is assembled here first and sent in one piece.
//!
//! # Where it lives, and why that is not the internal RAM
//!
//! 360x360 pixels at two bytes each is **253 KiB**. The ESP32-S3 has 512 KiB of internal SRAM
//! and this firmware already spends a large part of it on the Wi-Fi and Bluetooth stacks, so
//! the framebuffer goes to the 8 MB of external PSRAM. That is not a compromise: the DMA on
//! this chip reads external RAM directly, so a picture can go out with no copy in
//! front of it -- 6.6 ms, which is the bus and nothing else. What it costs instead is writing
//! the data cache back before each transfer, measured at 0.9 ms with every line of the picture
//! dirty. See [`DisplayBus::pixels_push_direct`](crate::display::DisplayBus::pixels_push_direct).
//!
//! The factory firmware reports about 5.7 MB of PSRAM in use for a demo that does rather more
//! than this, which is the other half of the argument: the space is there.
//!
//! # Why the bytes are stored the wrong way round
//!
//! The ST77916 wants RGB565 with the high byte first, and Xtensa is little-endian. Storing the
//! pixels big-endian means every drawing operation swaps two bytes -- and the blit swaps none,
//! for 129600 pixels. The swap belongs where it happens once per pixel drawn rather than once
//! per pixel shown.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

/// The panel is 360x360.
pub const WIDTH: usize = 360;
/// The panel is 360x360.
pub const HEIGHT: usize = 360;
/// Two bytes per pixel, RGB565.
pub const BYTES: usize = WIDTH * HEIGHT * 2;

/// A 360x360 RGB565 picture, stored ready to send.
pub struct Framebuffer {
    pixels: &'static mut [u8],
}

impl Framebuffer {
    /// Takes the first [`BYTES`] of `memory` as the picture.
    ///
    /// Returns `None` if there is not enough of it, which is the only way this can fail: the
    /// caller hands over a region it owns and stops using it.
    pub fn new(memory: &'static mut [u8]) -> Option<Self> {
        if memory.len() < BYTES {
            return None;
        }
        Some(Self {
            pixels: &mut memory[..BYTES],
        })
    }

    /// The picture as the panel wants it: RGB565, high byte first, row by row.
    pub fn bytes(&self) -> &[u8] {
        self.pixels
    }

    /// The picture as bytes to be **filled**, in the panel's own order.
    ///
    /// This is the door for a picture that already exists somewhere in exactly this form -- the
    /// factory demo keeps its backgrounds on the TF card as 360x360 RGB565, and a file like that
    /// is read straight into here rather than drawn pixel by pixel. Everything a firmware
    /// composes goes through `embedded-graphics` instead, which is why this is the only mutable
    /// view and why it hands out bytes rather than pixels: what fills it is a stream, not a
    /// drawing.
    ///
    /// Whether the bytes of a pixel arrive in the panel's order is the caller's problem, and it
    /// is one that is decided by looking. For the factory demo's backgrounds it was decided
    /// with `src/bin/sdshow.rs`: they are stored **high byte first**, the same way
    /// round as this buffer, so they need no pass over them at all. The other order was on the
    /// screen one tap away and came out **grey** -- swapping the bytes of an RGB565 pixel cuts
    /// across the three channels rather than permuting them, so a photograph turns into noise.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        self.pixels
    }

    /// Halves the brightness of the rows from `top` up to `bottom`, in place.
    ///
    /// This is what makes text readable over a photograph. A band across the whole width rather
    /// than a box around the words, because the screen is round: a full-width band keeps its
    /// edges off the picture, where a box would put two more corners into it.
    ///
    /// The arithmetic is one shift and one mask per pixel. Shifting an RGB565 value down by one
    /// halves all three channels at once and only goes wrong where the low bit of a channel
    /// falls into the top of the one below it, which is exactly what the mask clears.
    pub fn dim_rows(&mut self, top: usize, bottom: usize) {
        /// Every channel's low bit, cleared after the shift.
        const CARRY: u16 = 0x7BEF;

        let bottom = bottom.min(HEIGHT);
        if top >= bottom {
            return;
        }
        let band = &mut self.pixels[top * WIDTH * 2..bottom * WIDTH * 2];
        for pixel in band.chunks_exact_mut(2) {
            let value = u16::from_be_bytes([pixel[0], pixel[1]]);
            let halved = ((value >> 1) & CARRY).to_be_bytes();
            pixel[0] = halved[0];
            pixel[1] = halved[1];
        }
    }

    /// The colour at `(x, y)`, unpacked from the two bytes it is stored as.
    ///
    /// The bounds are the caller's business, so that a loop over every pixel does not pay for
    /// checking each one twice.
    ///
    /// # Panics
    ///
    /// If `x` or `y` is outside the picture, through the slice index.
    pub fn pixel(&self, x: usize, y: usize) -> u16 {
        let i = (y * WIDTH + x) * 2;
        u16::from_be_bytes([self.pixels[i], self.pixels[i + 1]])
    }

    fn put(&mut self, x: usize, y: usize, color: Rgb565) {
        let i = (y * WIDTH + x) * 2;
        let [high, low] = RawU16::from(color).into_inner().to_be_bytes();
        self.pixels[i] = high;
        self.pixels[i + 1] = low;
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(WIDTH as u32, HEIGHT as u32)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb565;
    /// Nothing here can fail: a pixel outside the picture is dropped, as `embedded-graphics`
    /// expects of any target.
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            let (Ok(x), Ok(y)) = (usize::try_from(point.x), usize::try_from(point.y)) else {
                continue;
            };
            if x < WIDTH && y < HEIGHT {
                self.put(x, y, color);
            }
        }
        Ok(())
    }

    /// Fills a rectangle a row at a time instead of a pixel at a time.
    ///
    /// The default implementation walks the area as an iterator of points, which for a full
    /// screen means 129600 rounds of bounds arithmetic. This is the same loop with the
    /// arithmetic hoisted out, and it is worth having because clearing the screen happens once
    /// per frame.
    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let area = area.intersection(&self.bounding_box());
        if area.is_zero_sized() {
            return Ok(());
        }
        let word = word_of(color);
        let left = area.top_left.x as usize;
        let top = area.top_left.y as usize;
        let width = area.size.width as usize;
        for y in top..top + area.size.height as usize {
            let start = (y * WIDTH + left) * 2;
            fill_words(&mut self.pixels[start..start + width * 2], word);
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        fill_words(self.pixels, word_of(color));
        Ok(())
    }
}

/// One colour repeated twice, as the machine word a run of it is written with.
///
/// The bytes have to land in memory as `high, low, high, low`, and this is a little-endian
/// machine, so the word is built from the bytes in the order they are to appear.
fn word_of(color: Rgb565) -> u32 {
    let [high, low] = RawU16::from(color).into_inner().to_be_bytes();
    u32::from_le_bytes([high, low, high, low])
}

/// Fills `bytes` with `word`, four bytes at a time where it can.
///
/// Clearing the screen was **13.5 ms** as a loop of two byte stores per pixel, measured, which
/// by then was more than sending the finished frame at 80 MHz. The stores
/// were the whole cost: PSRAM is reached over a bus that moves a word as cheaply as a byte, so
/// a pass that writes words does a quarter of the work for the same result.
///
/// The head and tail exist because a run may start or end mid-word -- a rectangle at an odd
/// column does. A full-screen clear has neither.
fn fill_words(bytes: &mut [u8], word: u32) {
    // SAFETY: `u32` has no invalid bit patterns and no stricter alignment than the split
    // `align_to_mut` performs; the head and tail keep whatever the middle cannot take.
    let (head, words, tail) = unsafe { bytes.align_to_mut::<u32>() };
    let pattern = word.to_le_bytes();
    for (index, byte) in head.iter_mut().enumerate() {
        *byte = pattern[index % 4];
    }
    // A head of odd length has rotated the pattern for everything after it; a head of even
    // length, the usual case, rotates by nothing.
    words.fill(word.rotate_right(8 * (head.len() as u32 % 4)));
    let offset = head.len() + words.len() * 4;
    for (index, byte) in tail.iter_mut().enumerate() {
        *byte = pattern[(offset + index) % 4];
    }
}
