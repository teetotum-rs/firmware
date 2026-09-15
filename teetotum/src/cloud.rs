//! A cloud of points to stand a screen on, in the colours of the theme.
//!
//! The ground of home, the settings and the player without a cover, chosen in a browser mockup
//! that draws the ring by the same rules as [`crate::menu`] and the cloud behind it. The points
//! are the theme's selected colour, a share of them its icon colour, and they
//! get darker towards the middle, so text there still stands on black. The same scattering
//! serves every theme; only its colours change.
//!
//! # Drawn every time, not kept
//!
//! The plan was to draw the cloud once into the backdrop and restore it on every frame. It is
//! not, for two reasons. The backdrop holds the cover of what is playing, and the menus stand
//! on the cloud whether a cover is up or not, so keeping both would cost a third screen of
//! external RAM. And restoring a screen is the dearer move anyway: the copy costs about 21 ms
//! against 13 ms for the clear that this adds its points to. Measured on the device: 2450
//! points in 6.7 ms.
//!
//! # Still or moving
//!
//! Given a moment, the cloud turns once in [`TURN_MILLIS`] and every point breathes between
//! 30 % and full brightness at a pace of its own, from 3 to 13 s a breath. What only the moving
//! cloud needs comes from a second sequence, so the still cloud is the same picture whether or
//! not motion exists.
//!
//! # Whole numbers only
//!
//! No `sin`, `cos` or `powf` from a library: the points are drawn from a square and the ones
//! outside the screen thrown away, the brightness rises with the distance from the centre as
//! `t * sqrt(t)`, and the one sine the motion needs is a parabola with a correction, good to
//! about 0.1 %. The mockup drew in polar coordinates and with `t^1.6`, so its points lie
//! elsewhere; the number, the share of large and of icon-coloured points and the darkening are
//! the same, and the two ramps differ by at most 2 % of the peak.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;

use crate::framebuffer::{HEIGHT, WIDTH};
use crate::menu::Palette;

/// The radius the brightness is measured against: the screen's.
const RIM: i32 = WIDTH as i32 / 2;

/// How far from the centre a point may lie, one pixel inside the rim so a large one still fits.
const REACH: i32 = RIM - 1;

/// One point in this many is two pixels square rather than one.
const LARGE_EVERY: u32 = 5;

/// One turn of the moving cloud, in milliseconds: two minutes, slow enough to be a ground.
pub const TURN_MILLIS: u32 = 120_000;

/// A breath's lowest brightness and its swing, of 32768: from 30 % to full.
const BREATH_FLOOR: i32 = 21299;
const BREATH_SWING: i32 = 11469;

/// The cloud's shape. The colours come from the palette it is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cloud {
    /// Where the scattering starts. The same seed puts the points in the same places in every
    /// theme and on every boot.
    pub seed: u32,
    /// How many points lie on the screen.
    pub points: usize,
    /// How bright a point at the rim is, of 256.
    pub peak: u16,
    /// How far from the centre, in pixels, the darkening reaches zero.
    pub core: i32,
    /// How many points in a hundred are in the icon colour rather than the selected one.
    pub accent: u8,
}

impl Cloud {
    /// Draws the points over whatever is in `target`, which is black for a ground.
    ///
    /// `moment` is a time in milliseconds for the moving cloud, from any start, or `None` for
    /// the still one. The scattering is drawn from the seed each time, so this holds no state
    /// and costs no RAM.
    pub fn draw<D>(
        &self,
        target: &mut D,
        palette: &Palette,
        moment: Option<u32>,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        self.draw_within(target, palette, moment, RIM)
    }

    /// Draws only the points nearer the centre than `radius`, for a ground that a ring over the
    /// rim covers anyway. They are the same points [`draw`](Self::draw) puts there.
    pub fn draw_within<D>(
        &self,
        target: &mut D,
        palette: &Palette,
        moment: Option<u32>,
        radius: i32,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let mut scatter = self.seed.max(1);
        let mut beat = (self.seed ^ 0x9E37_79B9).max(1);
        // The turn, as a cosine and a sine of 32768. Standing still is exactly the identity.
        let (cos, sin) = match moment {
            Some(millis) => {
                let turn =
                    ((u64::from(millis % TURN_MILLIS) << 16) / u64::from(TURN_MILLIS)) as u16;
                (sine(turn.wrapping_add(0x4000)), sine(turn))
            }
            None => (32768, 0),
        };

        let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
        let across = (2 * REACH + 1) as u32;
        let span = (RIM - self.core).max(1);
        let mut placed = 0;
        while placed < self.points {
            // Every draw is taken whether the point is kept or not, so the sequence does not
            // depend on the palette or on anything drawn before.
            let x = (xorshift(&mut scatter) % across) as i32 - REACH;
            let y = (xorshift(&mut scatter) % across) as i32 - REACH;
            let large = xorshift(&mut scatter).is_multiple_of(LARGE_EVERY);
            let accent = xorshift(&mut scatter) % 100 < u32::from(self.accent);
            let square = x * x + y * y;
            if square > REACH * REACH {
                continue;
            }
            placed += 1;
            // Where in its breath the point starts, and how fast it breathes, in 1/65536 of a
            // breath per millisecond.
            let phase = xorshift(&mut beat) as u16;
            let pace = 5 + (xorshift(&mut beat) >> 16) % 16;

            let r = (square as u32).isqrt() as i32;
            if r <= self.core || r >= radius {
                continue;
            }
            // Distance past the core, of 256, then to the power of 1.5, then times the peak.
            let t = ((r - self.core) * 256 / span) as u32;
            let ramp = (t * (t << 8).isqrt()) >> 8;
            let mut k = (ramp * u32::from(self.peak)) >> 8;
            if let Some(millis) = moment {
                let breath = sine(phase.wrapping_add(millis.wrapping_mul(pace) as u16));
                k = (k * (BREATH_FLOOR + ((BREATH_SWING * breath) >> 15)) as u32) >> 15;
            }
            let base = if accent {
                palette.icon
            } else {
                palette.selected
            };
            let colour = Rgb565::new(scale(base.r(), k), scale(base.g(), k), scale(base.b(), k));
            if colour == Rgb565::BLACK {
                continue;
            }

            let (x, y) = (
                (x * cos - y * sin + 16384) >> 15,
                (x * sin + y * cos + 16384) >> 15,
            );
            let at = centre + Point::new(x, y);
            if large {
                target.fill_solid(&Rectangle::new(at, Size::new(2, 2)), colour)?;
            } else {
                Pixel(at, colour).draw(target)?;
            }
        }
        Ok(())
    }
}

/// Xorshift32: three shifts. It never leaves zero, so it must never start there.
fn xorshift(state: &mut u32) -> u32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    *state
}

/// The sine of `turn` 65536ths of a turn, of 32768.
///
/// A parabola through the zeros and the peaks, `4x(1 - |x|)`, then pulled towards the true
/// curve by 0.225 of its own error against `y|y|`. Good to about 0.1 %, which a point that
/// moves by one pixel in several frames does not show.
fn sine(turn: u16) -> i32 {
    // Half a turn either way, of 32768.
    let x = i32::from(turn as i16);
    let y = (4 * x * (32768 - x.abs())) >> 15;
    let squared = (y * y.abs()) >> 15;
    y + (((squared - y) * 7373) >> 15)
}

/// One channel times `k` of 256.
fn scale(channel: u8, k: u32) -> u8 {
    ((u32::from(channel) * k) >> 8) as u8
}
