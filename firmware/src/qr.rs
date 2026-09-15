//! QR codes for the links in [`LINKS`], black on white in the disc inside the ring.
//!
//! **The codes are made at build time, not on the device.** `build.rs` encodes every URL and
//! leaves the modules as bits: 80 to 180 bytes of flash a code, no RAM, and no encoder in the
//! image. Encoding here would buy nothing while the links are fixed.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::Rectangle;
use qrcodegen_no_heap::{QrCode, QrCodeEcc, Version};
use teetotum::framebuffer::{Framebuffer, HEIGHT, WIDTH};
use teetotum::menu::{INNER, fonts, text, width};

include!("links.rs");

/// A finished code: `size` modules a side, one bit a module, row by row, high bit first.
pub struct Code {
    pub size: u8,
    modules: &'static [u8],
}

impl Code {
    pub fn dark(&self, x: usize, y: usize) -> bool {
        let i = y * self.size as usize + x;
        self.modules[i / 8] & (0x80 >> (i % 8)) != 0
    }
}

include!(concat!(env!("OUT_DIR"), "/qr_codes.rs"));

/// Radius of the white disc, a little inside the ring so the ring keeps its edge.
const DISC: i32 = INNER - 2;
/// White kept round the code inside the disc's square, in modules. The standard asks for four;
/// the disc adds more on every side but the corners, and phones read two.
const QUIET: i32 = 2;
/// Height of a line in the small font, and what a line keeps clear of the disc's edge.
const LINE: i32 = 16;
const MARGIN: i32 = 6;

/// Draws link `n`'s code over the disc, with its base URL below and its caption above.
pub fn draw(frame: &mut Framebuffer, n: usize) {
    let (code, link) = (&CODES[n], &LINKS[n]);
    draw_modules(
        frame,
        i32::from(code.size),
        |x, y| code.dark(x, y),
        link.host,
        Above::WhereItFits(link.caption),
    );
}

/// The largest version [`encode`] makes: 41 modules a side, some 150 characters at level Medium.
const ENCODED_VERSION: Version = Version::new(6);
const ENCODED_BYTES: usize = ENCODED_VERSION.buffer_len();

/// A code made on the device, for text that is only known at run time.
pub struct Encoded {
    size: u8,
    modules: [u8; ENCODED_BYTES],
}

impl Encoded {
    fn dark(&self, x: usize, y: usize) -> bool {
        let i = y * self.size as usize + x;
        self.modules[i / 8] & (0x80 >> (i % 8)) != 0
    }

    /// Draws the code like a link's, with `below` under it and `above` over it. Unlike a link's
    /// caption, `above` always stands: the code shrinks until it fits.
    pub fn draw(&self, frame: &mut Framebuffer, below: &str, above: &str) {
        draw_modules(
            frame,
            i32::from(self.size),
            |x, y| self.dark(x, y),
            below,
            Above::Always(above),
        );
    }
}

/// Encodes `text`, or `None` if it is too long for [`ENCODED_VERSION`].
pub fn encode(text: &str) -> Option<Encoded> {
    let mut scratch = [0u8; ENCODED_BYTES];
    let mut out = [0u8; ENCODED_BYTES];
    let code = QrCode::encode_text(
        text,
        &mut scratch,
        &mut out,
        QrCodeEcc::Medium,
        Version::MIN,
        ENCODED_VERSION,
        None,
        true,
    )
    .ok()?;
    let size = code.size();
    let mut modules = [0u8; ENCODED_BYTES];
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x, y) {
                let i = (y * size + x) as usize;
                modules[i / 8] |= 0x80 >> (i % 8);
            }
        }
    }
    Some(Encoded {
        size: size as u8,
        modules,
    })
}

/// The line over a code.
enum Above<'a> {
    WhereItFits(&'a str),
    Always(&'a str),
}

/// Draws `size` modules over the disc, as large as whole pixels a module allow.
fn draw_modules(
    frame: &mut Framebuffer,
    size: i32,
    dark: impl Fn(usize, usize) -> bool,
    below: &str,
    above: Above<'_>,
) {
    let (width_px, height_px) = (WIDTH as i32, HEIGHT as i32);
    let centre = Point::new(width_px / 2, height_px / 2);

    // A row at a time, because `fill_solid` is word-wide and a circle primitive goes pixel by
    // pixel. In half pixels, since the screen's centre lies between two.
    for y in 0..height_px {
        let dy = 2 * y + 1 - height_px;
        let r = 2 * DISC;
        if dy.abs() >= r {
            continue;
        }
        let half = isqrt(r * r - dy * dy);
        let (left, right) = ((width_px - half + 1) / 2, (width_px + half) / 2);
        fill(frame, left, y, right - left, 1, Rgb565::WHITE);
    }

    // The base URL always stands below the code, so the code gives up a pixel a module until
    // that line fits the chord there. The caption above stands only where it fits as it is.
    let square = DISC * 181 / 128; // the disc's inscribed square, DISC * sqrt(2)
    let host = width(below, &fonts::SMALL);
    let mut scale = (square / (size + 2 * QUIET)).max(1);
    let (above, required) = match above {
        Above::WhereItFits(text) => (text, 0),
        Above::Always(text) => (text, width(text, &fonts::SMALL)),
    };
    let caption = required.max(host);
    while scale > 1 && caption > room(size * scale / 2 + QUIET * scale + LINE) {
        scale -= 1;
    }
    let side = size * scale;
    let (left, top) = (centre.x - side / 2, centre.y - side / 2);
    for y in 0..size {
        let mut x = 0;
        while x < size {
            if !dark(x as usize, y as usize) {
                x += 1;
                continue;
            }
            let start = x;
            while x < size && dark(x as usize, y as usize) {
                x += 1;
            }
            fill(
                frame,
                left + start * scale,
                top + y * scale,
                (x - start) * scale,
                scale,
                Rgb565::BLACK,
            );
        }
    }

    let gap = side / 2 + QUIET * scale;
    let at = |sign: i32| centre + Point::new(0, sign * (gap + LINE / 2));
    let _ = text(frame, below, at(1), &fonts::SMALL, Rgb565::BLACK);
    if width(above, &fonts::SMALL) <= room(gap + LINE) {
        let _ = text(frame, above, at(-1), &fonts::SMALL, Rgb565::BLACK);
    }
}

/// How wide a line may be whose outer edge lies `outer` pixels from the centre.
fn room(outer: i32) -> i32 {
    if outer >= DISC {
        return 0;
    }
    2 * isqrt(DISC * DISC - outer * outer) - 2 * MARGIN
}

fn fill(frame: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, colour: Rgb565) {
    let area = Rectangle::new(Point::new(x, y), Size::new(w as u32, h as u32));
    let _ = frame.fill_solid(&area, colour);
}

fn isqrt(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let (mut x, mut y) = (n, (n + 1) / 2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// The ring's icons, one a link.
pub mod icons {
    use teetotum::menu::Icon;

    /// The repository: angle brackets and a slash.
    pub const CODE: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "............###.........",
        "............###.........",
        "......###...######......",
        ".....####...##.####.....",
        "....####....##..####....",
        "...####....###...####...",
        "...###.....###....###...",
        "..###......###.....###..",
        ".###.......##.......###.",
        ".###.......##.......###.",
        "..###.....###......###..",
        "...###....###.....####..",
        "...####...###....####...",
        "....####..###...####....",
        ".....####.##...####.....",
        "......######...###......",
        ".........###............",
        ".........###............",
        "........................",
        "........................",
        "........................",
    ]);

    /// An AI coding agent: a prompt in a terminal.
    pub const TERMINAL: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "..####################..",
        ".######################.",
        ".##..................##.",
        ".##..................##.",
        ".##..#...............##.",
        ".##.###..............##.",
        ".##..###.............##.",
        ".##...####...........##.",
        ".##....####..........##.",
        ".##....####..........##.",
        ".##...####...........##.",
        ".##..####............##.",
        ".##.###....########..##.",
        ".##..#.....########..##.",
        ".##..................##.",
        ".##..................##.",
        ".######################.",
        "..####################..",
        "........................",
        "........................",
        "........................",
    ]);

    /// The hardware: the knob seen from above.
    pub const KNOB: Icon = Icon::new(&[
        "........................",
        "........................",
        "........########........",
        "......############......",
        ".....####......####.....",
        "....###..........###....",
        "...###............###...",
        "...##.....####.....##...",
        "..###...########...###..",
        "..##....###..###....##..",
        "..##...###....###...##..",
        "..##...##......##...##..",
        "..##...##......##...##..",
        "..##...###....###...##..",
        "..##....###..###....##..",
        "..###...########...###..",
        "...##.....####.....##...",
        "...###............###...",
        "....###..........###....",
        ".....####......####.....",
        "......############......",
        "........########........",
        "........................",
        "........................",
    ]);

    /// The chips: a package with its pins.
    pub const CHIP: Icon = Icon::new(&[
        "........................",
        "........##.##.##........",
        "........##.##.##........",
        "........##.##.##........",
        "........##.##.##........",
        "........##.##.##........",
        "......############......",
        "......############......",
        ".########......########.",
        ".#######........#######.",
        "......##........##......",
        ".#######........#######.",
        ".#######........#######.",
        "......##........##......",
        ".#######........#######.",
        ".########......########.",
        "......############......",
        "......############......",
        "........##.##.##........",
        "........##.##.##........",
        "........##.##.##........",
        "........##.##.##........",
        "........##.##.##........",
        "........................",
    ]);

    /// A blog: a page of text.
    pub const PAGE: Icon = Icon::new(&[
        "........................",
        "....################....",
        "....################....",
        "....################....",
        "....##............##....",
        "....##............##....",
        "....##.##########.##....",
        "....##.##########.##....",
        "....##............##....",
        "....##............##....",
        "....##.##########.##....",
        "....##.##########.##....",
        "....##............##....",
        "....##............##....",
        "....##.##########.##....",
        "....##.##########.##....",
        "....##............##....",
        "....##............##....",
        "....##.######.....##....",
        "....##.######.....##....",
        "....################....",
        "....################....",
        "....################....",
        "........................",
    ]);

    /// The plugin runtime: the WebAssembly mark, a notched square, with a W.
    pub const WASM: Icon = Icon::new(&[
        "........................",
        "........................",
        ".#######........#######.",
        ".#######........#######.",
        ".##....##......##....##.",
        ".##.....########.....##.",
        ".##......######......##.",
        ".##..................##.",
        ".##..................##.",
        ".##.##.....##.....##.##.",
        ".##.##.....##.....##.##.",
        ".##..##....##....##..##.",
        ".##..##...####...##..##.",
        ".##...##..####..##...##.",
        ".##...##.##..##.##...##.",
        ".##....####..####....##.",
        ".##....###....###....##.",
        ".##.....##....##.....##.",
        ".##..................##.",
        ".##..................##.",
        ".######################.",
        ".######################.",
        "........................",
        "........................",
    ]);

    /// A guide: an open book.
    pub const BOOK: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "..#####..........#####..",
        ".########......########.",
        ".##.....###..###.....##.",
        ".##.......####.......##.",
        ".##........##........##.",
        ".##..####..##..####..##.",
        ".##........##........##.",
        ".##..####..##..####..##.",
        ".##........##........##.",
        ".##..####..##..####..##.",
        ".##........##........##.",
        ".##..####..##..####..##.",
        ".##........##........##.",
        ".##........##........##.",
        ".######################.",
        "..........####..........",
        "........................",
        "........................",
        "........................",
        "........................",
        "........................",
    ]);

    /// Rust on Espressif chips: a crab.
    pub const CRAB: Icon = Icon::new(&[
        "........................",
        "........................",
        "..##................##..",
        ".###..##........##..###.",
        ".##...##........##...##.",
        ".##..###........###..##.",
        ".######..........######.",
        "..####............####..",
        "...##..............##...",
        "...##...##....##...##...",
        "....##..##....##..##....",
        ".....##############.....",
        "....################....",
        "...##################...",
        "..####################..",
        "..####################..",
        "...##################...",
        "....################....",
        "..##.##..........##.##..",
        ".##.##............##.##.",
        ".#..#..............#..#.",
        "........................",
        "........................",
        "........................",
    ]);

    /// A problem report: a bug.
    pub const BUG: Icon = Icon::new(&[
        "........................",
        "......#..........#......",
        ".......##......##.......",
        ".........##..##.........",
        "..........####..........",
        "........########........",
        ".......##########.......",
        "..##...##########...##..",
        "...##..............##...",
        "....##.####..####.##....",
        "......#####..#####......",
        ".....######..######.....",
        ".....######..######.....",
        ".#####.####..####.#####.",
        ".#####.####..####.#####.",
        ".....######..######.....",
        ".....######..######.....",
        "......#####..#####......",
        "...##..####..####..##...",
        "..##....###..###....##..",
        ".##..................##.",
        "........................",
        "........................",
        "........................",
    ]);

    /// Code quality: a checklist, every line ticked.
    pub const CHECKLIST: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "......##................",
        ".....##.................",
        "##..##...##############.",
        ".####....##############.",
        "..##....................",
        "........................",
        "........................",
        "......##................",
        ".....##.................",
        "##..##...##############.",
        ".####....##############.",
        "..##....................",
        "........................",
        "........................",
        "......##................",
        ".....##.................",
        "##..##...##############.",
        ".####....##############.",
        "..##....................",
        "........................",
        "........................",
    ]);
}
