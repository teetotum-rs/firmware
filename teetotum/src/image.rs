//! A picture that is not already a screen: JPEG in, framebuffer out.
//!
//! Everything shown so far arrived in the panel's own shape. The factory demo's backgrounds are
//! 360x360 RGB565 with the high byte first, which is this firmware's framebuffer exactly, so
//! `src/bin/sdshow.rs` reads one straight into it with nothing in between. A JPEG is the other
//! case, and it is the one that matters for anything that comes from outside the box: the cover
//! art the other chip hands over is 27 KiB of JPEG at whatever size the phone felt like, and a
//! plugin's own artwork will be a JPEG for the same reason -- it is what a picture is stored as
//! everywhere that is not this panel.
//!
//! So there are two jobs here, and only the second one is ours: decoding, which
//! [`zune-jpeg`](zune_jpeg) does, and **fitting**, which is arithmetic over a few hundred
//! thousand pixels and therefore worth being careful about.
//!
//! # The big buffer is the caller's
//!
//! A 500x500 photograph is 750 KB of RGB once it is unpacked -- three times a whole screen, and
//! more than this chip's internal RAM has to spare. [`decode`] therefore takes the memory to
//! decode into rather than allocating it: the caller hands over the part of the external RAM
//! that is not a picture ([`Screen::take_spare`](crate::screen::Screen::take_spare)) and knows
//! what it costs. The decoder still uses the heap for its own tables and, for a progressive
//! JPEG, for a frame of coefficients; that is out of our hands, and it is the reason the
//! external RAM belongs in the allocator as well.
//!
//! # Three scalers, because the right one is a matter for the glass
//!
//! Cover art and photographs arrive **larger** than the screen and have to come down, which is
//! the opposite of what the rotating blit in [`crate::rotate`] does. That changes which filter
//! is right, and it changes it twice over:
//!
//! - [`Scaler::Nearest`] throws away every source pixel it does not land on. Coming down by a
//!   factor of two that is half the picture, and what is left aliases: a striped shirt turns
//!   into a moire, a line of small print turns into gravel.
//! - [`Scaler::Bilinear`] mixes the four pixels around the sample. It is the right answer going
//!   **up** and only half an answer coming down, because it still reads four pixels out of the
//!   nine or sixteen that an output pixel covers.
//! - [`Scaler::Box`] averages every source pixel an output pixel actually covers. It is the
//!   only one of the three whose cost grows as the picture gets bigger, and the only one that
//!   uses all of it.
//!
//! **Judged at the glass with `src/bin/jpegshow.rs`, on four synthetic pictures
//! built to separate them: `Scaler::Box` wins clearly, and `Scaler::Nearest` invents rings that
//! are not in the source.** Coming down to 360 pixels, box is the filter. That reverses the
//! judgement the rotating blit got -- and it has to, because the two do opposite
//! things: [`crate::rotate`] resamples at 1:1, where nearest keeps a one-pixel line whole and
//! bilinear greys it, while here every output pixel covers several source pixels and the ones
//! a sampler skips come back as moire.
//!
//! It is not free. 1024 down to 360 costs box 587 ms against 104 for nearest and 237 for
//! bilinear, and box is the one whose cost follows the *source*: it reads every pixel, so it
//! gets dearer the further down the picture comes, while the other two follow the screen.
//!
//! **Going up, bilinear**, and that was judged at the glass too, on the phone's
//! own cover art at 200x200. Box collapses to nearest above 1:1 -- its window falls to a single
//! pixel -- and the two are indeed indistinguishable there, which is this direction's free proof
//! that the sampling grid sits right. Bilinear is better by a little: `tools/upscale-difference.py`
//! puts the mean difference at 3 out of 255 against 41.7 coming down, but the maximum at 116,
//! because going up there is nothing to alias and the whole difference sits in the edges of
//! lettering, where nearest steps and bilinear ramps.
//!
//! Three directions, three different winners. [`Picture::scaler`] is where that lives.

use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use zune_jpeg::JpegDecoder;
use zune_jpeg::errors::DecodeErrors;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

/// What can go wrong between a file and a picture.
#[derive(Debug)]
pub enum Error {
    /// The decoder refused the data.
    Jpeg(DecodeErrors),
    /// The buffer handed in is smaller than the unpacked picture needs.
    NoRoom {
        /// How much room there was.
        found: usize,
        /// How much the picture needs.
        needed: usize,
    },
    /// The headers parsed but named no size, which no real file does.
    NoSize,
}

/// How a source pixel is chosen for an output pixel that falls between several of them.
///
/// The names are the same three the rest of the field uses. What they mean here is spelled out
/// in the module documentation, and which one is right for a given picture is a question the
/// glass has now answered three times, differently each time -- see [`Picture::scaler`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scaler {
    /// The nearest source pixel, and nothing else.
    Nearest,
    /// The four source pixels around the sample, weighted by how close it falls to each.
    Bilinear,
    /// Every source pixel the output pixel covers, averaged.
    Box,
}

impl Scaler {
    /// The three of them in order, for a run that steps through them.
    pub const ALL: [Scaler; 3] = [Scaler::Nearest, Scaler::Bilinear, Scaler::Box];

    /// A name short enough to draw on the picture it made.
    pub fn name(self) -> &'static str {
        match self {
            Scaler::Nearest => "nearest",
            Scaler::Bilinear => "bilinear",
            Scaler::Box => "box",
        }
    }
}

/// An unpacked picture: three bytes a pixel, row by row.
pub struct Picture<'a> {
    /// Its own width, which has nothing to do with the screen's.
    pub width: usize,
    /// Its own height.
    pub height: usize,
    pixels: &'a [u8],
}

/// Where a picture ended up on the screen once it was fitted into it.
///
/// A picture that is not square leaves a band above and below, or left and right; the glass is
/// round and the band is mostly behind the bezel, but a caption still wants to know where the
/// picture stops.
#[derive(Clone, Copy, Debug)]
pub struct Fit {
    /// Leftmost column of the picture on the screen.
    pub left: usize,
    /// Topmost row.
    pub top: usize,
    /// How wide it was drawn.
    pub width: usize,
    /// How tall it was drawn.
    pub height: usize,
}

/// Unpacks a JPEG into `into`, which has to be big enough for three bytes a pixel.
///
/// The headers are read first, so a caller that wants to know the size before committing the
/// memory can call this with an empty slice and read it off the [`Error::NoRoom`].
///
/// # Errors
///
/// If the decoder refuses the data, if `into` is too small, or if the file names no size.
pub fn decode<'a>(jpeg: &[u8], into: &'a mut [u8]) -> Result<Picture<'a>, Error> {
    // RGB out whatever comes in: a grey JPEG is rare and a CMYK one rarer, and neither is worth
    // a second path through the fitting below.
    let options = DecoderOptions::new_fast().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    decoder.decode_headers().map_err(Error::Jpeg)?;

    let info = decoder.info().ok_or(Error::NoSize)?;
    let (width, height) = (usize::from(info.width), usize::from(info.height));
    let needed = decoder.output_buffer_size().ok_or(Error::NoSize)?;
    if into.len() < needed {
        return Err(Error::NoRoom {
            found: into.len(),
            needed,
        });
    }
    let pixels = &mut into[..needed];
    decoder.decode_into(pixels).map_err(Error::Jpeg)?;

    Ok(Picture {
        width,
        height,
        pixels,
    })
}

impl<'a> Picture<'a> {
    /// A picture that is already unpacked, wrapped so it can be fitted to the screen.
    ///
    /// The door for anything that produced RGB some other way -- and, in a run that compares
    /// the scalers, the way to try the second one without decoding the file again.
    ///
    /// Returns `None` if `pixels` is not exactly three bytes per pixel of the given size.
    pub fn new(width: usize, height: usize, pixels: &'a [u8]) -> Option<Self> {
        if pixels.len() < width * height * 3 {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    /// The colour at `(x, y)`, as three channels still eight bits wide.
    ///
    /// The bounds are the caller's business, as they are in
    /// [`Framebuffer::pixel`](crate::framebuffer::Framebuffer::pixel) and for the same reason:
    /// this is an inner loop.
    fn rgb(&self, x: usize, y: usize) -> (u32, u32, u32) {
        let i = (y * self.width + x) * 3;
        (
            u32::from(self.pixels[i]),
            u32::from(self.pixels[i + 1]),
            u32::from(self.pixels[i + 2]),
        )
    }

    /// How big this picture is when it is made to fit the screen without changing its shape.
    ///
    /// Whole pixels, so a 1000x999 picture comes out 360x359 rather than 360x359.64: the last
    /// row of a photograph is not worth carrying a fraction through everything below.
    pub fn fit(&self) -> Fit {
        if self.width == 0 || self.height == 0 {
            return Fit {
                left: 0,
                top: 0,
                width: 0,
                height: 0,
            };
        }
        // Wider than the screen is tall? Then the width is what runs out first.
        let (width, height) = if self.width * HEIGHT >= self.height * WIDTH {
            (WIDTH, (self.height * WIDTH / self.width).max(1).min(HEIGHT))
        } else {
            ((self.width * HEIGHT / self.height).max(1).min(WIDTH), HEIGHT)
        };
        Fit {
            left: (WIDTH - width) / 2,
            top: (HEIGHT - height) / 2,
            width,
            height,
        }
    }

    /// How big this picture is when it is made to fit a square of `side` in the middle of the
    /// screen without changing its shape: [`Picture::fit`] with less room than the whole glass.
    pub fn fit_within(&self, side: usize) -> Fit {
        let side = side.min(WIDTH).min(HEIGHT);
        if self.width == 0 || self.height == 0 || side == 0 {
            return Fit {
                left: 0,
                top: 0,
                width: 0,
                height: 0,
            };
        }
        let (width, height) = if self.width >= self.height {
            (side, (self.height * side / self.width).max(1).min(side))
        } else {
            ((self.width * side / self.height).max(1).min(side), side)
        };
        Fit {
            left: (WIDTH - width) / 2,
            top: (HEIGHT - height) / 2,
            width,
            height,
        }
    }

    /// Where this picture goes at its own size, in the middle of the screen -- or, if it is
    /// bigger than the screen, where [`Picture::fit`] puts it.
    ///
    /// For a small picture that should stay sharp rather than fill the glass: a 200x200 cover
    /// brought up by 1.8 has no more detail than before, only bigger pixels.
    pub fn native(&self) -> Fit {
        if self.width > WIDTH || self.height > HEIGHT {
            return self.fit();
        }
        Fit {
            left: (WIDTH - self.width) / 2,
            top: (HEIGHT - self.height) / 2,
            width: self.width,
            height: self.height,
        }
    }

    /// The filter this picture wants, from the judgement made at the glass.
    ///
    /// **Coming down, box**, because it reads every source pixel and the ones a sampler skips
    /// come back as moire rings -- that is what the glass showed on the zone plate, and it is
    /// the whole reason this is a function and not a caller's guess.
    ///
    /// **At 1:1, nearest**, because all three are then the same arithmetic and this is the
    /// cheapest of them. That is not a preference either: it is what the nineteen 360x360
    /// pictures on the card proved by looking identical under all three.
    ///
    /// **Going up, bilinear**, judged on cover art arriving at 200x200: box and
    /// nearest are indistinguishable there, because `draw_box`'s window collapses to a single
    /// sample once an output pixel covers less than one source pixel, and bilinear is better by
    /// a little -- a mean of 3 out of 255, but a maximum of 116, all of it in the edges of
    /// lettering.
    pub fn scaler(&self) -> Scaler {
        self.scaler_for(self.fit())
    }

    /// The filter for drawing this picture into `fit`, by the same judgement as
    /// [`Picture::scaler`]. At [`Picture::native`] size that is nearest.
    pub fn scaler_for(&self, fit: Fit) -> Scaler {
        match fit.width.cmp(&self.width) {
            core::cmp::Ordering::Less => Scaler::Box,
            core::cmp::Ordering::Equal => Scaler::Nearest,
            core::cmp::Ordering::Greater => Scaler::Bilinear,
        }
    }

    /// Draws the picture into the middle of `frame`, as big as it goes, and says where it went.
    ///
    /// Nothing outside the picture is touched: the bands left over are whatever was in the
    /// framebuffer already, so a caller that wants black behind a portrait clears first.
    pub fn draw(&self, frame: &mut Framebuffer, scaler: Scaler) -> Fit {
        let fit = self.fit();
        self.draw_at(frame, fit, scaler);
        fit
    }

    /// Draws the picture into `fit`, as [`Picture::fit`] or [`Picture::native`] placed it.
    ///
    /// Like [`Picture::draw`], nothing outside `fit` is touched.
    pub fn draw_at(&self, frame: &mut Framebuffer, fit: Fit, scaler: Scaler) {
        if fit.width == 0 || fit.height == 0 {
            return;
        }
        match scaler {
            Scaler::Nearest => self.draw_sampled(frame, fit, false),
            Scaler::Bilinear => self.draw_sampled(frame, fit, true),
            Scaler::Box => self.draw_box(frame, fit),
        }
    }

    /// One sample per output pixel, taken at the middle of the square it stands for.
    ///
    /// The half-pixel shift is what keeps the picture centred: an output pixel covers a whole
    /// square of the source, and its colour belongs to the middle of that square rather than to
    /// its top left corner. Without it a picture halved in size drifts up and left by half a
    /// source pixel, which is invisible on a photograph and obvious on a grid.
    fn draw_sampled(&self, frame: &mut Framebuffer, fit: Fit, blend: bool) {
        // 16.16 fixed point throughout, the same as `crate::rotate`: a picture is never so big
        // that its coordinates need more than sixteen whole bits, and a sixteen-bit fraction is
        // finer than the eye at these sizes.
        let step_x = ((self.width as i64) << 16) / fit.width as i64;
        let step_y = ((self.height as i64) << 16) / fit.height as i64;
        let last_x = (self.width - 1) as i32;
        let last_y = (self.height - 1) as i32;

        for dy in 0..fit.height {
            let sy = (dy as i64 * 2 + 1) * step_y / 2 - 32768;
            for dx in 0..fit.width {
                let sx = (dx as i64 * 2 + 1) * step_x / 2 - 32768;
                let colour = if blend {
                    self.bilinear(sx as i32, sy as i32, last_x, last_y)
                } else {
                    let x = (((sx + 32768) >> 16) as i32).clamp(0, last_x);
                    let y = (((sy + 32768) >> 16) as i32).clamp(0, last_y);
                    let (r, g, b) = self.rgb(x as usize, y as usize);
                    rgb565(r, g, b)
                };
                put(frame, fit.left + dx, fit.top + dy, colour);
            }
        }
    }

    /// The four source pixels around a 16.16 sample, mixed by how close it falls to each.
    ///
    /// Clamped rather than refused at the edges: unlike the rotating blit, every sample here is
    /// inside the picture by construction, and only the outermost half pixel reaches past it.
    fn bilinear(&self, sx: i32, sy: i32, last_x: i32, last_y: i32) -> u16 {
        let x0 = (sx >> 16).clamp(0, last_x);
        let y0 = (sy >> 16).clamp(0, last_y);
        let x1 = (x0 + 1).min(last_x);
        let y1 = (y0 + 1).min(last_y);
        let fx = if sx < 0 { 0 } else { (sx & 0xFFFF) as u32 };
        let fy = if sy < 0 { 0 } else { (sy & 0xFFFF) as u32 };

        let (x0, y0, x1, y1) = (x0 as usize, y0 as usize, x1 as usize, y1 as usize);
        let tl = self.rgb(x0, y0);
        let tr = self.rgb(x1, y0);
        let bl = self.rgb(x0, y1);
        let br = self.rgb(x1, y1);
        rgb565(
            mix(tl.0, tr.0, bl.0, br.0, fx, fy),
            mix(tl.1, tr.1, bl.1, br.1, fx, fy),
            mix(tl.2, tr.2, bl.2, br.2, fx, fy),
        )
    }

    /// Every source pixel an output pixel covers, averaged.
    ///
    /// The spans are worked out from the output grid rather than from a sample point, so they
    /// tile the source exactly: no pixel is read twice and none is skipped. Coming down by a
    /// factor of three that is nine reads per output pixel, which is why this is the expensive
    /// one -- and why it is the only one that cannot alias.
    fn draw_box(&self, frame: &mut Framebuffer, fit: Fit) {
        for dy in 0..fit.height {
            let y0 = dy * self.height / fit.height;
            let y1 = (((dy + 1) * self.height).div_ceil(fit.height)).max(y0 + 1).min(self.height);
            for dx in 0..fit.width {
                let x0 = dx * self.width / fit.width;
                let x1 = (((dx + 1) * self.width).div_ceil(fit.width)).max(x0 + 1).min(self.width);
                let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (pr, pg, pb) = self.rgb(x, y);
                        r += pr;
                        g += pg;
                        b += pb;
                    }
                }
                let n = ((x1 - x0) * (y1 - y0)) as u32;
                put(frame, fit.left + dx, fit.top + dy, rgb565(r / n, g / n, b / n));
            }
        }
    }
}

/// One channel of the four corners, weighted and summed.
fn mix(tl: u32, tr: u32, bl: u32, br: u32, fx: u32, fy: u32) -> u32 {
    let top = tl * (65536 - fx) + tr * fx;
    let bottom = bl * (65536 - fx) + br * fx;
    (((top >> 8) * (65536 - fy) + (bottom >> 8) * fy) >> 24).min(255)
}

/// Three eight-bit channels packed the way the panel wants them.
fn rgb565(r: u32, g: u32, b: u32) -> u16 {
    (((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3)) as u16
}

/// One pixel into the framebuffer, high byte first.
///
/// Straight into the bytes rather than through `embedded-graphics`: this is called 129600 times
/// for one picture, and the whole of it is two stores.
fn put(frame: &mut Framebuffer, x: usize, y: usize, colour: u16) {
    let i = (y * WIDTH + x) * 2;
    let [high, low] = colour.to_be_bytes();
    let bytes = frame.bytes_mut();
    bytes[i] = high;
    bytes[i + 1] = low;
}
