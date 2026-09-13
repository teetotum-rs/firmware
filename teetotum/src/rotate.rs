//! Turning the picture between the framebuffer and the glass.
//!
//! The panel controller can turn a picture, but only in quarters: MADCTL has three bits for
//! geometry -- mirror X, mirror Y, swap axes -- which is 0, 90, 180, 270 degrees and their
//! mirrors, and nothing in between. The glass here is **round**, and a round picture can be
//! turned to any angle without losing a corner, so the four the controller offers are an
//! arbitrary quarter of what the hardware would allow. This module supplies the other eight.
//!
//! # Why the drawing layer is not turned instead
//!
//! The alternative is to rotate while drawing: hand `embedded-graphics` a target that turns
//! every point on its way in. It is cheaper -- nothing is copied twice -- and it is wrong for
//! this device for two reasons. Text drawn that way is rendered from a *rotated glyph raster*,
//! which at 30 degrees means the font's own hinting fights the rotation and every string has to
//! be re-rendered when the angle changes; and every plugin drawing anything would have to be
//! rotation-aware, when the whole point of the plugin interface is that a plugin draws into a
//! 360x360 picture and does not care how the device is held.
//!
//! So the picture is drawn straight and turned once on its way out, and the cost of that is
//! what this module is for measuring.
//!
//! # The arithmetic
//!
//! Backwards mapping: for each pixel of the *output*, work out where it came from in the
//! source. Forwards mapping would leave holes, because a rotated grid does not land on a grid.
//!
//! Everything is 16.16 fixed point, and the source coordinate is stepped along a row by adding
//! a constant rather than multiplied out per pixel -- the multiplication happens twice per row
//! instead of twice per pixel. There is no trigonometry at run time: twelve angles need twelve
//! pairs of constants, and they are in the table below.

use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};

/// How many orientations there are: one every 30 degrees.
pub const STEPS: usize = 12;

/// Cosine and sine of each step, as 16.16 fixed point.
///
/// Written out rather than computed because twelve angles do not need a sine function, and a
/// table can be read and checked: 56756 is 65536 times the cosine of 30 degrees.
const TURN: [(i32, i32); STEPS] = [
    (65536, 0),
    (56756, 32768),
    (32768, 56756),
    (0, 65536),
    (-32768, 56756),
    (-56756, 32768),
    (-65536, 0),
    (-56756, -32768),
    (-32768, -56756),
    (0, -65536),
    (32768, -56756),
    (56756, -32768),
];

/// How a source pixel is chosen for an output pixel that falls between four of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// Take the nearest source pixel.
    ///
    /// One read per output pixel. At angles that are multiples of 90 degrees it is exact; in
    /// between, a one-pixel line becomes dotted and small text loses stems.
    Nearest,
    /// Mix the four source pixels the output pixel falls between, weighted by how close it
    /// falls to each.
    ///
    /// Four reads and three interpolations per channel per output pixel: 145 ms per frame
    /// against 42, so 7 frames a second against 23.
    ///
    /// **It is the wrong default for a user interface**, judged by switching
    /// between the two under one finger at a fixed angle. A one-pixel white line that falls
    /// between two output pixels lights both at half strength: the line stays continuous, which
    /// is what bilinear is for, but it also goes grey. Against nearest -- which keeps one pixel
    /// at full white and drops the other -- the whole picture reads as darker and softer, and
    /// an interface made of thin strokes and small text is exactly the content that loses by
    /// it. Nearest was better at every angle that is not a multiple of 90 degrees, and at the
    /// multiples of 90 the two are identical by construction.
    ///
    /// It stays here because the judgement was about *this* content. A photograph -- cover art,
    /// say -- has no one-pixel strokes to smear, and resampling it with nearest is the case
    /// bilinear exists for. **The filter belongs to what is being drawn, not to the device.**
    Bilinear,
}

/// The colour rows outside the source turn into.
///
/// A square picture turned by 30 degrees does not cover a square output: the corners come from
/// outside the framebuffer. The glass is round and the corners are behind the bezel, so this is
/// almost never seen -- but "almost" is not "never", and an undefined corner would show
/// whatever was in the staging buffer last time.
const OUTSIDE: [u8; 2] = [0x00, 0x00];

/// Fills `out` with `rows` rows of the picture, starting at `first_row`, turned by
/// `step` * 30 degrees **clockwise**.
///
/// Clockwise is measured on the glass: eleven steps take a mark at twelve o'clock to eleven.
/// The sign follows from the screen's y axis pointing down, which is easy to get backwards on
/// paper.
///
/// `out` is the staging buffer that goes to the panel: `rows * WIDTH * 2` bytes of RGB565, high
/// byte first, exactly as [`Framebuffer::bytes`] would have delivered them unturned. It has to
/// live in internal RAM -- that is the whole reason the picture is turned a band at a time
/// rather than into a second framebuffer.
///
/// # Panics
///
/// If `out` is not exactly `rows * WIDTH * 2` bytes, or `step` is not below [`STEPS`].
pub fn rotate_rows(
    source: &Framebuffer,
    step: usize,
    filter: Filter,
    first_row: usize,
    rows: usize,
    out: &mut [u8],
) {
    assert!(step < STEPS, "there are only twelve orientations");
    assert_eq!(
        out.len(),
        rows * WIDTH * 2,
        "the staging buffer is misshapen"
    );

    let (cos, sin) = TURN[step];

    // The centre of a 360-wide picture is between pixels 179 and 180, so it is 179.5 -- and
    // rounding that to 180 would turn the picture about a point half a pixel off centre, which
    // at the rim is a visible shift.
    let cx = ((WIDTH as i32 - 1) << 16) / 2;
    let cy = ((HEIGHT as i32 - 1) << 16) / 2;

    for row in 0..rows {
        let dy = (((first_row + row) as i32) << 16) - cy;
        // Where the leftmost pixel of this output row comes from. Sixty-four bits only here:
        // the products are up to 180*65536*65536, and the results fit in 32 bits again.
        let mut u = cx + ((((-cx) as i64 * cos as i64) + (dy as i64 * sin as i64)) >> 16) as i32;
        let mut v = cy + (((-((-cx) as i64) * sin as i64) + (dy as i64 * cos as i64)) >> 16) as i32;

        let line = &mut out[row * WIDTH * 2..(row + 1) * WIDTH * 2];
        for pixel in line.chunks_exact_mut(2) {
            let colour = match filter {
                Filter::Nearest => nearest(source, u, v),
                Filter::Bilinear => bilinear(source, u, v),
            };
            match colour {
                Some(rgb) => {
                    let [high, low] = rgb.to_be_bytes();
                    pixel[0] = high;
                    pixel[1] = low;
                }
                None => pixel.copy_from_slice(&OUTSIDE),
            }
            u += cos;
            v -= sin;
        }
    }
}

/// Where the picture pixel is that ended up at `(x, y)` on the turned screen.
///
/// This is the same backwards mapping the blit does per pixel, for one point: a finger touches
/// the *turned* picture, and everything under it -- a button, a list, a plugin's own idea of
/// where it drew something -- lives in the picture's own coordinates. Rounded to whole pixels,
/// because a finger is not a subpixel event.
///
/// The result can fall outside the picture: the corners of a turned square come from nowhere,
/// and on a round glass a touch near the rim is the ordinary case. The caller decides what an
/// outside touch means, so it is returned as it is rather than clamped.
///
/// # Panics
///
/// If `step` is not below [`STEPS`].
pub fn source_point(step: usize, x: i32, y: i32) -> (i32, i32) {
    assert!(step < STEPS, "there are only twelve orientations");
    let (cos, sin) = TURN[step];

    let cx = ((WIDTH as i32 - 1) << 16) / 2;
    let cy = ((HEIGHT as i32 - 1) << 16) / 2;
    let dx = (x << 16) - cx;
    let dy = (y << 16) - cy;

    let u = cx + (((dx as i64 * cos as i64) + (dy as i64 * sin as i64)) >> 16) as i32;
    let v = cy + ((-(dx as i64) * sin as i64 + (dy as i64 * cos as i64)) >> 16) as i32;

    ((u + 0x8000) >> 16, (v + 0x8000) >> 16)
}

/// The source pixel nearest to the 16.16 coordinate, or `None` if that is off the picture.
fn nearest(source: &Framebuffer, u: i32, v: i32) -> Option<u16> {
    let x = (u + 0x8000) >> 16;
    let y = (v + 0x8000) >> 16;
    if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
        return None;
    }
    Some(source.pixel(x as usize, y as usize))
}

/// The four source pixels around the 16.16 coordinate, mixed by how close it falls to each.
///
/// Returns `None` when the sample would need a pixel from outside the picture, so the rim of a
/// turned picture is one pixel narrower than with [`Filter::Nearest`]. That is behind the bezel.
fn bilinear(source: &Framebuffer, u: i32, v: i32) -> Option<u16> {
    let x = u >> 16;
    let y = v >> 16;
    if x < 0 || y < 0 || x + 1 >= WIDTH as i32 || y + 1 >= HEIGHT as i32 {
        return None;
    }
    let (x, y) = (x as usize, y as usize);

    // The fractions, as 0..=256. Eight bits is enough: the source has five bits per channel,
    // and a weight finer than the thing it weighs buys nothing.
    let fx = ((u >> 8) & 0xff) as u32;
    let fy = ((v >> 8) & 0xff) as u32;

    let (r00, g00, b00) = unpack(source.pixel(x, y));
    let (r10, g10, b10) = unpack(source.pixel(x + 1, y));
    let (r01, g01, b01) = unpack(source.pixel(x, y + 1));
    let (r11, g11, b11) = unpack(source.pixel(x + 1, y + 1));

    Some(pack(
        mix(r00, r10, r01, r11, fx, fy),
        mix(g00, g10, g01, g11, fx, fy),
        mix(b00, b10, b01, b11, fx, fy),
    ))
}

/// One channel of the four corners, weighted and summed.
fn mix(c00: u32, c10: u32, c01: u32, c11: u32, fx: u32, fy: u32) -> u32 {
    let top = c00 * (256 - fx) + c10 * fx;
    let bottom = c01 * (256 - fx) + c11 * fx;
    (top * (256 - fy) + bottom * fy) >> 16
}

/// RGB565 into its three channels, each still at its own width.
fn unpack(colour: u16) -> (u32, u32, u32) {
    let colour = u32::from(colour);
    ((colour >> 11) & 0x1f, (colour >> 5) & 0x3f, colour & 0x1f)
}

/// Three channels back into RGB565.
fn pack(r: u32, g: u32, b: u32) -> u16 {
    (((r & 0x1f) << 11) | ((g & 0x3f) << 5) | (b & 0x1f)) as u16
}

/// Which quarter turn a *direction* comes back as, for a picture shown turned `step` steps.
///
/// [`source_point`] answers where a point came from, and that is exact. A direction with a
/// **name** -- up, down, left, right, as the touch controller reports its slides -- has no exact
/// answer at thirty degrees, because there are twelve angles and four names. So it rounds: a
/// picture turned by 30 degrees still calls the viewer's "up" up, one turned by 60 calls it
/// left. The rounding is what a hand does anyway -- nobody swipes to the nearest degree.
///
/// The result is the number of quarter turns, 0 to 3, and what to do with it is
/// [`Gesture::in_picture`](crate::touch::Gesture::in_picture).
///
/// # Panics
///
/// If `step` is not below [`STEPS`].
pub fn source_quarter(step: usize) -> usize {
    assert!(step < STEPS, "there are only twelve orientations");
    // (step + 1) / 3 is round(step / 3) for the twelve steps, and step 11 -- 330 degrees, which
    // is 30 degrees the other way -- wraps back to no turn at all.
    ((step + 1) / 3) % 4
}
