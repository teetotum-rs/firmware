//! The settings menu: a ring of twelve segments around the dialog of the one selected.
//!
//! # The shape
//!
//! **Twelve segments of 30 degrees, and a quarter turn is three of them.** The picture can stand
//! at any quarter turn (see [`Screen::set_orientation`](crate::screen::Screen::set_orientation)),
//! and the menu is drawn into the picture and turned with it. Every one of those orientations
//! leaves the middle of a segment -- never a divider -- pointing wherever a middle pointed before,
//! so the segment that faces the USB socket keeps facing it at every setting.
//!
//! The ring reaches the edge of the screen and is a quarter of its radius thick, 45 of 180 pixels,
//! which leaves a disc 270 pixels across for the dialog. **The ring carries icons and no names**,
//! and that was measured rather than chosen: a 24-pixel icon
//! fits in every segment, but an icon with a name in `FONT_6X10` under it fits only in the four
//! segments on the axes, and only up to five letters. "Orientation" fits in none -- and still in
//! none at a third of the radius, which would have cost the dialog 30 pixels of width for names
//! that fit in some segments and not in others. So the name of the selected entry stands in the
//! middle, large, which is where the eye already is.
//!
//! **The type is Helvetica, sans-serif, at 14, 18 and 24 pixels** -- the X11 bitmaps U8g2
//! carries, under Adobe's and DEC's permission notice (see `LICENSE-FONTS`). The screen has
//! 0.127 mm to the pixel, so the 10x20 font that `embedded-graphics` brings is 2.5 mm tall; it
//! was judged too small on the screen, and it has serifs, which were not wanted.
//! A dialog's body is set in the same [`fonts`] through [`text`], so it matches the frame.
//!
//! # The rules every menu keeps
//!
//! - **About is the top segment**, in the firmware's menu and in every plugin's. On round screen
//!   it is also the one mark that says which way up the menu stands, so the orientation setting
//!   needs no marker of its own. **The home menu is the one exception**: Home stands there, and
//!   marks the top just the same (see [`Menu::home`]).
//! - **A plugin's menu has a way to the firmware's settings**, in the segment left of its About,
//!   and so has the home menu, left of Home. [`Menu::plugin`] and [`Menu::home`] put it there,
//!   and nothing else can take that slot. From the firmware's side a plugin's menu is entered
//!   through a [`Kind::Plugin`] entry, and the link then leads back one level further up, exactly
//!   as it would from inside the plugin.
//! - **OK or Cancel in a dialog goes back to the menu it was opened from.** OK in a menu goes up
//!   one level, and out of the menus altogether from the top one -- **unless the top one is the
//!   home menu, which has no OK.** It is left by choosing a screen.
//! - **A long press on the screen leads home**, from anywhere, the menus included; in an open
//!   dialog it is Cancel first. That rule belongs to whoever reads the screen, which is the
//!   firmware -- see [`Taps::press`](crate::touch::Taps::press) -- so a plugin never sees the
//!   long press and cannot take it away.
//! - **Where OK and the long press would do the same, the menu says "hold for home" instead of
//!   showing OK**: in a menu one level below home, and only there. Anywhere
//!   deeper, and in every dialog, OK goes back one level and holding goes further, so both stay.
//!
//! Menus nest: an entry can be another [`Menu`], and the [`Navigator`] keeps the way back.
//!
//! # Who does what
//!
//! The navigator knows where the user is and what a turn or a tap means there; the owner of a
//! setting knows its value. So the navigator draws the ring, the names and the buttons, the owner
//! draws a dialog's body inside [`BODY`], and what happened comes back as an [`Outcome`] -- a
//! dialog opened, the knob turned in it, OK, Cancel -- rather than the navigator touching a value
//! itself. Cancel means "as if it had never been opened", and the snapshot that takes is the
//! owner's, taken at [`Outcome::Open`].
//!
//! Everything here is in picture coordinates, drawn as if upright and turned on the way to the
//! screen like every other picture. A tap has to be brought into the same frame before it is
//! handed in: [`Contact::in_view`](crate::touch::Contact::in_view), then
//! [`Screen::picture_point`](crate::screen::Screen::picture_point).

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::pixelcolor::raw::RawU16;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle};
use u8g2_fonts::FontRenderer;
use u8g2_fonts::types::{FontColor, HorizontalAlignment, VerticalPosition};

use crate::framebuffer::{Framebuffer, HEIGHT, WIDTH};

/// How many segments the ring has; see the module description for why twelve.
pub const SLOTS: usize = 12;
/// About's segment, at twelve o'clock in every menu -- and Home's in the home menu.
pub const ABOUT_SLOT: usize = 0;
/// The segment left of the top one, which in a plugin's menu and in the home menu leads to the
/// firmware's settings.
pub const FIRMWARE_SLOT: usize = SLOTS - 1;

/// How many pages a menu may have.
///
/// A ring holds twelve segments; a menu with more entries than fit hands the rest on to a second
/// page, and so on. **Five, because that is what the hidden bits allow**: [`Navigator::hide`]
/// addresses a segment of a page as one bit of a `u64`, and five pages of twelve are sixty.
pub const MAX_PAGES: usize = 64 / SLOTS;

/// The ring's outer radius: the edge of the screen.
pub const OUTER: i32 = WIDTH as i32 / 2;
/// The ring's inner radius. A quarter of the screen's radius is the ring; the rest is the dialog.
pub const INNER: i32 = OUTER - OUTER / 4;

/// Where a dialog's owner draws its body: between the name above and the buttons below.
///
/// 200 pixels wide, which the disc has room for from top to bottom of this band.
pub const BODY: Rectangle = Rectangle::new(
    Point::new(CENTRE.x - 100, CENTRE.y - 60),
    Size::new(200, 102),
);

/// The width of the dark line between two segments, in pixels.
///
/// Measured across the line rather than as an angle, so it is equally wide at the rim and at the
/// inner edge -- a wedge of constant angle would be three pixels outside and two inside.
const GAP: i32 = 3;

/// How deep menus may nest. Eight levels on a knob is already more than anyone will walk.
const DEPTH: usize = 8;

const CENTRE: Point = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);

/// The middle of each segment as a direction 1024 long, clockwise from twelve o'clock, in screen
/// coordinates where y grows downwards.
///
/// Twelve directions need two values, sin 30 = 0.5 and sin 60 = 0.866, so they are written out
/// rather than computed: `core` has no trigonometry, and a table cannot disagree with itself.
const DIRECTION: [(i32, i32); SLOTS] = [
    (0, -1024),
    (512, -887),
    (887, -512),
    (1024, 0),
    (887, 512),
    (512, 887),
    (0, 1024),
    (-512, 887),
    (-887, 512),
    (-1024, 0),
    (-887, -512),
    (-512, -887),
];

/// How far apart two neighbouring [`DIRECTION`]s are, at the same scale: 2 sin 15 degrees.
const NEIGHBOUR_DISTANCE: i32 = 530;

/// The size of a button, and how far around it a finger still counts as on it.
const BUTTON: Size = Size::new(92, 40);
const BUTTON_REACH: Size = Size::new(104, 52);

/// The colours of the menu.
///
/// **Two families on purpose**: the ring in teal and the icons in amber, so an icon reads as the
/// thing and the ring as the ground it stands on. Green is left out of both because on this device
/// green means "a value to be read off the screen", and the menu uses it for exactly that.
///
/// Expected to change, which is why it is a value handed to the drawing rather than constants in
/// it -- a plugin can bring its own.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// A segment with an entry in it.
    pub ring: Rgb565,
    /// The segment the knob is on, and the OK button.
    pub selected: Rgb565,
    /// A segment with nothing in it yet.
    pub empty: Rgb565,
    /// Every icon, the buttons' tick and cross included.
    pub icon: Rgb565,
    /// The entry's name.
    pub name: Rgb565,
    /// A setting's current value.
    pub value: Rgb565,
    /// Anything that explains rather than informs.
    pub quiet: Rgb565,
}

/// The firmware's palette.
pub const PALETTE: Palette = Palette {
    ring: rgb(0x0F4D5A),
    selected: rgb(0x1F8FA3),
    empty: rgb(0x0A262D),
    icon: rgb(0xFFB547),
    name: Rgb565::WHITE,
    value: Rgb565::CSS_LIGHT_GREEN,
    quiet: Rgb565::CSS_GRAY,
};

/// The firmware's palette with the teal turned orange, for its second colour theme.
///
/// The ring's three shades change, each to an orange about as bright as the teal it replaces, so
/// the ring keeps the depth it had. **The icons change the other way, to teal**: the two families
/// swap roles, and an icon stays the cool thing on warm
/// ground just as it is the warm thing on cool ground in [`PALETTE`]. The teal is about as bright
/// as the amber it replaces.
pub const PALETTE_ORANGE: Palette = Palette {
    ring: rgb(0x6A300F),
    selected: rgb(0xD2641E),
    empty: rgb(0x3A1A08),
    icon: rgb(0x4FD8E8),
    ..PALETTE
};

// The neon themes, each a ring hue with an icon hue across from it, chosen from a browser mockup
// of this ring that draws it by the same rules. The ring keeps three shades, the selected one
// saturated rather than only lighter.
//
// **Green breaks the rule at [`Palette`]**: its ring is the colour that
// elsewhere on this device means a value. The values stand in the disc, on black, so they still
// read; what is lost is only that green meant one thing.

/// Magenta with cyan icons.
pub const PALETTE_MAGENTA: Palette = Palette {
    ring: rgb(0x5C0F4E),
    selected: rgb(0xE0189E),
    empty: rgb(0x2E0827),
    icon: rgb(0x3DF2FF),
    ..PALETTE
};

/// Violet with lime icons.
pub const PALETTE_VIOLET: Palette = Palette {
    ring: rgb(0x34126B),
    selected: rgb(0x7A2CF5),
    empty: rgb(0x1A0936),
    icon: rgb(0xB8FF3D),
    ..PALETTE
};

/// Blue with hot pink icons.
pub const PALETTE_BLUE: Palette = Palette {
    ring: rgb(0x0E2A6E),
    selected: rgb(0x2B6BFF),
    empty: rgb(0x07153A),
    icon: rgb(0xFF4FAE),
    ..PALETTE
};

/// Pink with yellow icons.
pub const PALETTE_PINK: Palette = Palette {
    ring: rgb(0x6B0E3C),
    selected: rgb(0xFF2D8A),
    empty: rgb(0x36071E),
    icon: rgb(0xFFEE3A),
    ..PALETTE
};

/// Red with cyan icons.
pub const PALETTE_RED: Palette = Palette {
    ring: rgb(0x5E0E14),
    selected: rgb(0xF0283A),
    empty: rgb(0x2F070A),
    icon: rgb(0x55F4FF),
    ..PALETTE
};

/// Green with magenta icons.
pub const PALETTE_GREEN: Palette = Palette {
    ring: rgb(0x0F5418),
    selected: rgb(0x22D63A),
    empty: rgb(0x082A0C),
    icon: rgb(0xFF3DE0),
    ..PALETTE
};

/// Cyan with coral icons.
pub const PALETTE_CYAN: Palette = Palette {
    ring: rgb(0x07505C),
    selected: rgb(0x00D8F0),
    empty: rgb(0x04282E),
    icon: rgb(0xFF6A3D),
    ..PALETTE
};

/// Indigo with gold icons.
pub const PALETTE_INDIGO: Palette = Palette {
    ring: rgb(0x1E1B6B),
    selected: rgb(0x4B45F0),
    empty: rgb(0x0F0D36),
    icon: rgb(0xFFB000),
    ..PALETTE
};

/// No hue at all: the ring in three greys and the icons nearly white. Cut to RGB565 a grey keeps
/// at most a level of green, because green has the sixth bit.
pub const PALETTE_GREY: Palette = Palette {
    ring: rgb(0x3A3A3A),
    selected: rgb(0x808080),
    empty: rgb(0x1C1C1C),
    icon: rgb(0xE8E8E8),
    ..PALETTE
};

/// The type the menu is set in, for a dialog's body to match. Helvetica throughout; see the
/// module description for why.
///
/// **Every one of them skips a character it does not have.** Without that, u8g2-fonts answers
/// such a character with an error, [`text`] swallows the error, and the line does not come out
/// whole -- which a track title from a phone reaches sooner or later.
pub mod fonts {
    /// Re-exported because [`super::text`] takes one: a plugin setting its own text should not
    /// have to guess which version of the font crate to depend on.
    pub use u8g2_fonts::FontRenderer;
    use u8g2_fonts::fonts::{
        u8g2_font_helvB24_tr, u8g2_font_helvR14_tf, u8g2_font_helvR14_tr, u8g2_font_helvR18_tf,
        u8g2_font_helvR18_tr,
    };

    /// An entry's name, and a value to be read off the screen.
    pub const LARGE: FontRenderer =
        FontRenderer::new::<u8g2_font_helvB24_tr>().with_ignore_unknown_chars(true);
    /// A dialog's running text.
    pub const BODY: FontRenderer =
        FontRenderer::new::<u8g2_font_helvR18_tr>().with_ignore_unknown_chars(true);
    /// Anything that explains rather than informs.
    pub const SMALL: FontRenderer =
        FontRenderer::new::<u8g2_font_helvR14_tr>().with_ignore_unknown_chars(true);
    /// [`SMALL`] with Latin-1 as well as ASCII, for text that comes from outside: a phone names
    /// its tracks in whatever the artist wrote, accented letters included. It costs more than
    /// twice the flash of the ASCII cut (3.7 KB against 1.7), so it is only for such text.
    pub const SMALL_LATIN1: FontRenderer =
        FontRenderer::new::<u8g2_font_helvR14_tf>().with_ignore_unknown_chars(true);
    /// [`BODY`] with Latin-1, for a track's title: the one line from outside that is meant to be
    /// read at a glance. 4.9 KB against 2.2 for the ASCII cut.
    pub const BODY_LATIN1: FontRenderer =
        FontRenderer::new::<u8g2_font_helvR18_tf>().with_ignore_unknown_chars(true);
}

/// A colour written the way every colour picker shows it.
const fn rgb(hex: u32) -> Rgb565 {
    Rgb565::new(
        ((hex >> 19) & 0x1F) as u8,
        ((hex >> 10) & 0x3F) as u8,
        ((hex >> 3) & 0x1F) as u8,
    )
}

/// A one-colour icon, drawn as text: `#` is ink, anything else is not.
///
/// **So an icon can be written by someone with nothing but an editor**, which is who writes a
/// plugin. The colour is the palette's and not the icon's, which is what "one colour" buys: the
/// palette can change without a single icon being redrawn. 24x24 is the size the ring was laid out
/// for; the drawing centres whatever it is given.
#[derive(Clone, Copy, Debug)]
pub struct Icon {
    shape: Shape,
}

/// How an icon is held: as the art it was drawn as, or packed the way a face's manifest
/// carries it.
#[derive(Clone, Copy, Debug)]
enum Shape {
    Art(&'static [&'static str]),
    Packed(&'static [u8]),
}

impl Icon {
    pub const fn new(rows: &'static [&'static str]) -> Self {
        Self {
            shape: Shape::Art(rows),
        }
    }

    /// An icon packed the way `teetotum-face` packs it: 24 rows of four bytes, little-endian,
    /// bit 23 leftmost. This is how a face's icon arrives -- out of its manifest, drawn from
    /// where the module lies rather than copied.
    pub const fn packed(rows: &'static [u8]) -> Self {
        Self {
            shape: Shape::Packed(rows),
        }
    }

    /// Draws the icon centred on `centre`.
    pub fn draw<D>(&self, target: &mut D, centre: Point, colour: Rgb565) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let rows = match self.shape {
            Shape::Art(rows) => rows,
            Shape::Packed(rows) => return draw_packed(target, rows, centre, colour),
        };
        let height = rows.len() as i32;
        let width = rows.first().map_or(0, |row| row.len()) as i32;
        let top_left = centre - Point::new(width / 2, height / 2);
        target.draw_iter(rows.iter().enumerate().flat_map(move |(y, row)| {
            row.bytes()
                .enumerate()
                .filter(|&(_, ink)| ink == b'#')
                .map(move |(x, _)| Pixel(top_left + Point::new(x as i32, y as i32), colour))
        }))
    }
}

/// Draws a packed icon centred on `centre`; see [`Icon::packed`]. Public for a firmware that
/// draws a face's icon out of the face's own memory, which does not live for `'static`.
pub fn draw_packed<D>(
    target: &mut D,
    rows: &[u8],
    centre: Point,
    colour: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    const SIZE: i32 = 24;
    let top_left = centre - Point::new(SIZE / 2, SIZE / 2);
    target.draw_iter(
        rows.chunks_exact(4)
            .take(SIZE as usize)
            .enumerate()
            .flat_map(move |(y, row)| {
                let bits = u32::from_le_bytes([row[0], row[1], row[2], row[3]]);
                (0..SIZE)
                    .filter(move |x| bits & (1 << (SIZE - 1 - x)) != 0)
                    .map(move |x| Pixel(top_left + Point::new(x, y as i32), colour))
            }),
    )
}

/// The icons the firmware brings, and any plugin may use.
pub mod icons {
    use super::Icon;

    /// A house, for the top of the home menu.
    pub const HOME: Icon = Icon::new(&[
        "...........##...........",
        "..........####..........",
        ".........######.........",
        "........########........",
        ".......####..####.......",
        "......####....####......",
        ".....####......####.....",
        "....####........####....",
        "...####..........####...",
        "..####............####..",
        ".####..............####.",
        "#####..............#####",
        "#####..............#####",
        "#####..##########..#####",
        "..###..##########..###..",
        "..###..##########..###..",
        "..###..###....###..###..",
        "..###..###....###..###..",
        "..###..###....###..###..",
        "..###..###....###..###..",
        "..###..###....###..###..",
        "..####################..",
        "..####################..",
        "..####################..",
    ]);

    /// A tick, for the button that keeps what the dialog set.
    pub const CHECK: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "........................",
        ".....................##.",
        "....................###.",
        "...................####.",
        "..................####..",
        ".................####...",
        "................####....",
        "...............####.....",
        ".##...........####......",
        ".###.........####.......",
        ".####.......####........",
        "..####.....####.........",
        "...####...####..........",
        "....####.####...........",
        ".....#######............",
        "......#####.............",
        ".......###..............",
        "........................",
        "........................",
        "........................",
        "........................",
    ]);

    /// A cross, for the button that goes back to how the dialog found things.
    pub const CROSS: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "...##..............##...",
        "...###............###...",
        "...####..........####...",
        "....####........####....",
        ".....####......####.....",
        "......####....####......",
        ".......####..####.......",
        "........########........",
        ".........######.........",
        "..........####..........",
        ".........######.........",
        "........########........",
        ".......####..####.......",
        "......####....####......",
        ".....####......####.....",
        "....####........####....",
        "....###..........###....",
        "....##............##....",
        "........................",
        "........................",
        "........................",
    ]);

    /// An "i" in a circle, for the About entry every menu has.
    pub const ABOUT: Icon = Icon::new(&[
        ".........######.........",
        "......############......",
        ".....##############.....",
        "....#####......#####....",
        "...####..........####...",
        "..####.....##.....####..",
        ".####.....####.....####.",
        ".###......####......###.",
        ".###................###.",
        "###..................###",
        "###........##........###",
        "###.......####.......###",
        "###.......####.......###",
        "###.......####.......###",
        "###.......####.......###",
        ".###......####......###.",
        ".###......####......###.",
        ".####.....####.....####.",
        "..####.....##.....####..",
        "...####..........####...",
        "....#####......#####....",
        ".....##############.....",
        "......############......",
        ".........######.........",
    ]);

    /// A clock face without hands, marked at 12, 3, 6 and 9: the picture turns in the steps of
    /// a clock's hours.
    pub const ORIENTATION: Icon = Icon::new(&[
        ".........######.........",
        "......############......",
        ".....##############.....",
        "....#####.####.#####....",
        "...####...####...####...",
        "..####....####....####..",
        ".####......##......####.",
        ".###.......##.......###.",
        ".###................###.",
        "###..................###",
        "######............######",
        "########........########",
        "########........########",
        "######............######",
        "###..................###",
        ".###................###.",
        ".###.......##.......###.",
        ".####......##......####.",
        "..####....####....####..",
        "...####...####...####...",
        "....#####.####.#####....",
        ".....##############.....",
        "......############......",
        ".........######.........",
    ]);

    /// A gear, for the way from a plugin's menu to the firmware's.
    pub const FIRMWARE: Icon = Icon::new(&[
        "........................",
        "..........####..........",
        "..........####..........",
        "....##....####....##....",
        "...####..######..####...",
        "...##################...",
        "....################....",
        ".....##############.....",
        ".....######..######.....",
        "....#####......#####....",
        ".########......########.",
        ".#######........#######.",
        ".#######........#######.",
        ".########......########.",
        "....#####......#####....",
        ".....######..######.....",
        ".....##############.....",
        "....################....",
        "...##################...",
        "...####..######..####...",
        "....##....####....##....",
        "..........####..........",
        "..........####..........",
        "........................",
    ]);

    /// A puzzle piece, for an entry that leads into a plugin's menu.
    pub const PLUGIN: Icon = Icon::new(&[
        "........................",
        "........................",
        ".........###............",
        "........#####...........",
        ".......#######..........",
        ".......#######..........",
        "........#####...........",
        "...###############......",
        "...###############......",
        "...###############......",
        "...###############......",
        "...###############.##...",
        ".....#################..",
        "......#################.",
        "......#################.",
        "......#################.",
        ".....#################..",
        "...###############.##...",
        "...###############......",
        "...###############......",
        "...###############......",
        "...###############......",
        "........................",
        "........................",
    ]);

    /// A sun, for how bright the screen is: a disc with eight rays, so it cannot be taken for the
    /// gear, which has its teeth on the rim.
    pub const BRIGHTNESS: Icon = Icon::new(&[
        "........................",
        "...........##...........",
        "...........##...........",
        "....##.....##.....##....",
        "....###....##....###....",
        ".....###........###.....",
        "......#...####...#......",
        ".........######.........",
        "........########........",
        ".......##########.......",
        ".......##########.......",
        "####...##########...####",
        "####...##########...####",
        ".......##########.......",
        ".......##########.......",
        "........########........",
        ".........######.........",
        "......#...####...#......",
        ".....###........###.....",
        "....###....##....###....",
        "....##.....##.....##....",
        "...........##...........",
        "...........##...........",
        "........................",
    ]);

    /// A phone that shakes, for how hard the motor clicks. The shaking is drawn thinner than
    /// the phone, because at the full stroke two zigzags fill the space either side.
    pub const HAPTICS: Icon = Icon::new(&[
        "........................",
        "........................",
        ".......##########.......",
        ".......##########.......",
        ".......##########.......",
        "....#..###....###..#....",
        "...##..###....###..##...",
        "..###..###....###..###..",
        ".###...###....###...###.",
        ".##....###....###....##.",
        ".###...###....###...###.",
        "..###..###....###..###..",
        "...##..###....###..##...",
        "..###..###....###..###..",
        ".###...###....###...###.",
        ".##....###....###....##.",
        ".###...###....###...###.",
        "..###..###....###..###..",
        "...##..###....###..##...",
        "....#..##########..#....",
        ".......##########.......",
        ".......##########.......",
        "........................",
        "........................",
    ]);

    /// A painter's palette, for a choice of colours. The paint is left out rather than drawn in,
    /// because one colour is all an icon has. A half-filled circle would read as day and night,
    /// and this setting chooses colours, not brightness.
    pub const THEME: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "........########........",
        "......############......",
        "....#######...######....",
        "...###...##...#######...",
        "..####...##...##...###..",
        ".#####...#######...####.",
        ".###############...####.",
        ".######################.",
        ".###...#######..#######.",
        ".###...######....######.",
        ".###...######....######.",
        ".#############..#######.",
        ".######################.",
        "..###############.......",
        "...#############........",
        "....############........",
        "......##########........",
        "........########........",
        "........................",
        "........................",
        "........................",
    ]);

    /// Two beamed eighth notes, for the music player.
    pub const MUSIC: Icon = Icon::new(&[
        "........................",
        "........................",
        "................#####...",
        "............#########...",
        "........#############...",
        "........#############...",
        "........#########..##...",
        "........#####......##...",
        "........##.........##...",
        "........##.........##...",
        "........##.........##...",
        "........##.........##...",
        "........##.........##...",
        "........##.........##...",
        "........##......#####...",
        "........##....#######...",
        "........##...########...",
        ".....#####...########...",
        "...#######...######.....",
        "..########....###.......",
        "..########..............",
        "..######................",
        "...###..................",
        "........................",
    ]);

    /// A memory card with a Wi-Fi mark cut out of it, for the card over Wi-Fi.
    pub const CARD_WIFI: Icon = Icon::new(&[
        "........................",
        "........................",
        "....############........",
        "....#############.......",
        "....##############......",
        "....###############.....",
        "....################....",
        "....################....",
        "....####........####....",
        "....##..########..##....",
        "....##.##########.##....",
        "....#####......#####....",
        "....####.######.####....",
        "....################....",
        "....################....",
        "....#######..#######....",
        "....#######..#######....",
        "....################....",
        "....################....",
        "....################....",
        "....################....",
        "....################....",
        "........................",
        "........................",
    ]);

    /// A framed picture, for how the cover stands behind the player.
    pub const COVER: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "..####################..",
        "..####################..",
        "..##................##..",
        "..##...........###..##..",
        "..##..........#####.##..",
        "..##..........#####.##..",
        "..##...........###..##..",
        "..##................##..",
        "..##......#.........##..",
        "..##.....###........##..",
        "..##....#####...#...##..",
        "..##...#######.###..##..",
        "..##..#############.##..",
        "..##.#################..",
        "..####################..",
        "..####################..",
        "..####################..",
        "..####################..",
        "........................",
        "........................",
        "........................",
    ]);

    /// Points of two sizes, scattered: the cloud behind the menus.
    pub const CLOUD: Icon = Icon::new(&[
        "........................",
        "..........###...........",
        "..........###.....##....",
        "..##......###.....##....",
        "..##....................",
        "....................###.",
        "......##............###.",
        "......##............###.",
        "........................",
        "###.........##..........",
        "###.........##..........",
        "###.................##..",
        "...........###......##..",
        ".....##....###..........",
        ".....##....###..........",
        "....................###.",
        "..###...............###.",
        "..###.......##......###.",
        "..###.......##..........",
        "........................",
        ".........###.......##...",
        "...##....###.......##...",
        "...##....###............",
        "........................",
    ]);

    /// A ring that turns: whether the cloud moves.
    pub const MOTION: Icon = Icon::new(&[
        "............##..........",
        "............###.........",
        ".........########.......",
        ".......###########......",
        ".....############.......",
        "....######..####........",
        "....####....##..........",
        "...####.....#...........",
        "...###..................",
        "..####.............###..",
        "..###..............###..",
        "..###..............###..",
        "..###..............###..",
        "..###..............###..",
        "..####............####..",
        "...###............###...",
        "...####..........####...",
        "....####........####....",
        "....######....######....",
        ".....##############.....",
        ".......##########.......",
        ".........######.........",
        "........................",
        "........................",
    ]);

    /// A grid of points: how many the cloud has.
    pub const POINTS: Icon = Icon::new(&[
        "........................",
        "........................",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "........................",
        "........................",
        "........................",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "........................",
        "........................",
        "........................",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "........................",
        "........................",
        "........................",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "..###...###...###...###.",
        "........................",
    ]);

    /// A four-pointed star: how bright the cloud is at its brightest.
    pub const BRIGHTEST: Icon = Icon::new(&[
        "........................",
        "........................",
        "........................",
        "........................",
        "........................",
        "...........##...........",
        "...........##...........",
        "..........####..........",
        "..........####..........",
        ".........######.........",
        ".......##########.......",
        ".....##############.....",
        ".....##############.....",
        ".......##########.......",
        ".........######.........",
        "..........####..........",
        "..........####..........",
        "...........##...........",
        "...........##...........",
        "........................",
        "........................",
        "........................",
        "........................",
        "........................",
    ]);

    /// Points round an empty middle: how far the cloud keeps out of it.
    pub const CENTRE: Icon = Icon::new(&[
        "........................",
        "...........##...........",
        "..........####..........",
        ".....###..####..###.....",
        ".....###........###.....",
        ".....###........###.....",
        "........................",
        "..##................##..",
        ".####..............####.",
        ".####..............####.",
        "..##................##..",
        "........................",
        "........................",
        "..##................##..",
        ".####..............####.",
        ".####..............####.",
        "..##................##..",
        "........................",
        ".....###........###.....",
        ".....###........###.....",
        ".....###..####..###.....",
        "..........####..........",
        "...........##...........",
        "........................",
    ]);

    /// A share of a circle: how many points are in the icon colour.
    pub const ACCENT: Icon = Icon::new(&[
        "..........####..........",
        ".......##########.......",
        ".....##############.....",
        "....####........####....",
        "...####..........####...",
        "..###.......####...###..",
        "..###.......#####..###..",
        ".###........######..###.",
        ".##.........#######..##.",
        ".##.........#######..##.",
        "###.........#######..###",
        "###.........#######..###",
        "###.........#######..###",
        "###..........######..###",
        ".##...........#####..##.",
        ".##............####..##.",
        ".###...........###..###.",
        "..###...........#..###..",
        "..###..............###..",
        "...####..........####...",
        "....####........####....",
        ".....##############.....",
        ".......##########.......",
        "..........####..........",
    ]);
}

/// Names a setting to its owner. The numbers are the owner's to choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Id(pub u16);

/// Whose setting an [`Outcome`] is about.
///
/// A plugin's menu leads into the firmware's, so one navigator can hand out ids from two owners
/// who chose their numbers without asking each other. This is what keeps them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Firmware,
    Plugin,
}

/// Which buttons a dialog has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Buttons {
    /// Something that can be changed, and so can be taken back.
    OkCancel,
    /// Something that is only shown, like About. Cancel would have nothing to undo.
    Ok,
    /// Something that fills the disc and is only looked at, like a QR code. A tap on it is the
    /// owner's to answer, usually with [`Navigator::dismiss`].
    None,
}

/// What selecting an entry does.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// Opens a dialog, which its owner draws and answers.
    Setting { id: Id, buttons: Buttons },
    /// Goes one level deeper.
    Menu(&'static Menu),
    /// Goes into a plugin's menu, whose entries are then the plugin's to answer. This is how the
    /// firmware's settings reach a plugin's; the plugin's link left of About leads back.
    Plugin(&'static Menu),
    /// Goes to the firmware's settings. Only a plugin's menu and the home menu have this, in
    /// [`FIRMWARE_SLOT`].
    Firmware,
    /// Leaves the menus for one of the owner's screens -- in the firmware's home menu, a face.
    /// Which screen the id names is the owner's to choose, as with a setting.
    Screen(Id),
    /// The top of the home menu. The user is already there, so it opens nothing.
    Home,
}

/// One segment's worth of menu.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub name: &'static str,
    pub icon: &'static Icon,
    pub kind: Kind,
}

impl Entry {
    /// An entry that opens a dialog.
    pub const fn setting(
        name: &'static str,
        icon: &'static Icon,
        id: Id,
        buttons: Buttons,
    ) -> Self {
        Self {
            name,
            icon,
            kind: Kind::Setting { id, buttons },
        }
    }

    /// An entry that opens another menu.
    pub const fn menu(name: &'static str, icon: &'static Icon, menu: &'static Menu) -> Self {
        Self {
            name,
            icon,
            kind: Kind::Menu(menu),
        }
    }

    /// An entry that opens a plugin's menu.
    pub const fn plugin(name: &'static str, icon: &'static Icon, menu: &'static Menu) -> Self {
        Self {
            name,
            icon,
            kind: Kind::Plugin(menu),
        }
    }

    /// An entry that leaves the menus for one of the owner's screens.
    pub const fn screen(name: &'static str, icon: &'static Icon, id: Id) -> Self {
        Self {
            name,
            icon,
            kind: Kind::Screen(id),
        }
    }

    /// The setting this entry opens, if it opens one.
    pub fn id(&self) -> Option<Id> {
        match self.kind {
            Kind::Setting { id, .. } => Some(id),
            _ => None,
        }
    }
}

/// The entry [`Menu::plugin`] puts left of About. Inside a plugin's menu "Settings" alone would
/// name the menu the user is already in, so there it says which they are ("Device Settings" ran
/// past the disc).
pub const FIRMWARE_LINK: Entry = Entry {
    name: "Main Settings",
    icon: &icons::FIRMWARE,
    kind: Kind::Firmware,
};

/// The same way to the firmware's settings, as [`Menu::home`] names it: home is nobody's menu,
/// so there is nothing to tell it apart from.
const SETTINGS_LINK: Entry = Entry {
    name: "Settings",
    ..FIRMWARE_LINK
};

/// The entry [`Menu::home`] puts at the top.
const HOME_ENTRY: Entry = Entry {
    name: "Home",
    icon: &icons::HOME,
    kind: Kind::Home,
};

/// Twelve segments, of which About -- or Home -- is always the top one.
///
/// Built in a `const`, so that a menu which breaks a rule -- a second entry in About's slot, or
/// anything in the one a plugin keeps for the firmware -- fails the build rather than the screen.
#[derive(Debug)]
pub struct Menu {
    /// Shown small above the entry's name, so the user knows which level they are on.
    pub title: &'static str,
    slots: [Option<Entry>; SLOTS],
    /// Whether this is the home menu, which has no OK.
    home: bool,
    /// The page after this one, in a menu whose entries do not fit a single ring.
    next: Option<&'static Menu>,
    /// What a long press does here, said where OK would stand. See [`holding`](Self::holding).
    hold: Option<&'static str>,
}

impl Menu {
    /// A menu with About at the top and nothing else yet.
    pub const fn new(title: &'static str, about: Entry) -> Self {
        let mut slots = [None; SLOTS];
        slots[ABOUT_SLOT] = Some(about);
        Self {
            title,
            slots,
            home: false,
            next: None,
            hold: None,
        }
    }

    /// A plugin's menu: About at the top, and the way to the firmware's settings left of it.
    pub const fn plugin(title: &'static str, about: Entry) -> Self {
        Self::new(title, about).with(FIRMWARE_SLOT, FIRMWARE_LINK)
    }

    /// The menu the device starts in and comes back to: Home at the top, and the way to the
    /// firmware's settings left of it, where a plugin's menu has it too.
    ///
    /// **It has no OK**, because there is nowhere above it to go back to. It is left by an entry
    /// of [`Kind::Screen`], and the rest of its segments are for those.
    pub const fn home(title: &'static str) -> Self {
        let mut menu = Self::new(title, HOME_ENTRY).with(FIRMWARE_SLOT, SETTINGS_LINK);
        menu.home = true;
        menu
    }

    /// A page after the first of the home menu: Home at the top and nothing else.
    ///
    /// **The way to the firmware's settings is not repeated**: a page after
    /// the first carries only what no page before it carries. Home is the one exception, because
    /// it is the anchor the pages turn under and where the dots that count them stand.
    pub const fn home_page(title: &'static str) -> Self {
        let mut menu = Self::new(title, HOME_ENTRY);
        menu.home = true;
        menu
    }

    /// Says where OK would stand what a long press does in this menu.
    ///
    /// Only for a menu without OK, which is the home menu. The menu only says it; what a long
    /// press does is the owner's to answer.
    pub const fn holding(mut self, hint: &'static str) -> Self {
        self.hold = Some(hint);
        self
    }

    /// Whether this is the home menu.
    pub fn is_home(&self) -> bool {
        self.home
    }

    /// Puts `entry` into segment `slot`, counted clockwise from About.
    ///
    /// # Panics
    ///
    /// If the slot is past the twelfth or already taken -- at compile time, where menus are built.
    pub const fn with(mut self, slot: usize, entry: Entry) -> Self {
        assert!(slot < SLOTS, "a menu has twelve slots");
        assert!(
            self.slots[slot].is_none(),
            "that slot is taken; About or Home is always 0, and in a plugin's menu and the home menu 11 leads to the firmware"
        );
        self.slots[slot] = Some(entry);
        self
    }

    /// The entry in segment `slot`, if there is one.
    pub fn entry(&self, slot: usize) -> Option<&Entry> {
        self.slots.get(slot)?.as_ref()
    }

    /// Hands what did not fit on to `next`, the page after this one.
    ///
    /// **Only the top entry stands on every page** -- About, or Home in the home menu.
    /// Everything else has one place in the whole menu, so a page after the first
    /// is built from [`new`](Self::new) or [`home_page`](Self::home_page) and carries nothing
    /// but the entries that did not fit before it. The top segment is the anchor the pages turn
    /// under, which is why the row of dots that counts them sits right beneath it.
    ///
    /// **Pages are built backwards**, the last one first, so that every page can be handed the
    /// one it turns on to. A menu without this call is one page, which is every menu written as
    /// a `const`.
    pub const fn then(mut self, next: &'static Menu) -> Self {
        self.next = Some(next);
        self
    }

    /// How many pages there are from this one on; 1 for a menu that fits its ring.
    pub fn pages(&self) -> usize {
        let mut pages = 1;
        let mut page = self;
        while let Some(next) = page.next {
            pages += 1;
            page = next;
        }
        pages
    }

    /// Page `n` counted from this one, or the last there is.
    pub fn page(&'static self, n: usize) -> &'static Menu {
        let mut page = self;
        for _ in 0..n {
            match page.next {
                Some(next) => page = next,
                None => break,
            }
        }
        page
    }
}

/// What a turn or a tap came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing is there to act on.
    Nothing,
    /// The selection or the level changed, and nothing else did.
    Moved,
    /// A dialog opened. Its owner takes a snapshot now, for Cancel to return to.
    Open { id: Id, owner: Owner },
    /// The knob turned while a dialog was open. What that changes is the owner's business.
    Adjust { id: Id, owner: Owner, detents: i32 },
    /// A tap inside an open dialog that was not on a button, in picture coordinates.
    Touch { id: Id, owner: Owner, point: Point },
    /// OK in a dialog: keep what it shows. The menu above is back.
    Ok { id: Id, owner: Owner },
    /// Cancel in a dialog: go back to the snapshot. The menu above is back.
    Cancel { id: Id, owner: Owner },
    /// OK in the top menu: the menus are done with. The home menu never says this; it has no OK.
    Close,
    /// An entry of [`Kind::Screen`]: the menus are done with, for the screen `id` names. The
    /// navigator stays where it was, for the owner to drop.
    Screen { id: Id, owner: Owner },
}

#[derive(Clone, Copy, Debug)]
struct Level {
    /// The menu's first page. What is on the screen is [`page`](Self::page) steps along from it,
    /// so that the pages can be counted and walked from wherever the knob stands.
    menu: &'static Menu,
    /// Which page of it is on the screen.
    page: usize,
    selected: usize,
    owner: Owner,
    /// Entries left out, one bit per segment and page: bit `page * SLOTS + slot`. See
    /// [`Navigator::hide`].
    hidden: u64,
}

/// A menu is the same menu by address. Comparing its contents would walk every submenu, and two
/// menus with the same entries are still two places.
impl PartialEq for Level {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.menu, other.menu)
            && self.page == other.page
            && self.selected == other.selected
            && self.owner == other.owner
            && self.hidden == other.hidden
    }
}

impl Eq for Level {}

impl Level {
    /// How many pages the menu has.
    fn pages(&self) -> usize {
        self.menu.pages()
    }

    /// The entry in segment `slot` of page `page`, unless there is none or it is hidden.
    fn entry_on(&self, page: usize, slot: usize) -> Option<&'static Entry> {
        let bit = page * SLOTS + slot;
        if bit < u64::BITS as usize && self.hidden & (1 << bit) != 0 {
            return None;
        }
        let menu: &'static Menu = self.menu;
        menu.page(page).slots[slot].as_ref()
    }

    /// The entry in segment `slot` of the page on the screen, unless there is none or it is
    /// hidden.
    fn entry(&self, slot: usize) -> Option<&'static Entry> {
        self.entry_on(self.page, slot)
    }

    /// The next shown entry after `(page, slot)`, clockwise or back, over empty and hidden
    /// segments **and on across the pages, endlessly**: past the last entry of the last page the
    /// knob comes back to the first page rather than stopping.
    ///
    /// The top entry is never empty and never hidden, so there always is one; in a menu of that
    /// one alone the knob stays on it.
    fn step(&self, page: usize, slot: usize, clockwise: bool) -> (usize, usize) {
        let pages = self.pages();
        let (mut at, mut on) = (page, slot);
        for _ in 0..SLOTS * pages {
            if clockwise {
                on += 1;
                if on == SLOTS {
                    on = 0;
                    at = (at + 1) % pages;
                }
            } else if on == 0 {
                on = SLOTS - 1;
                at = (at + pages - 1) % pages;
            } else {
                on -= 1;
            }
            if self.entry_on(at, on).is_some() {
                return (at, on);
            }
        }
        (page, slot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Open {
    id: Id,
    buttons: Buttons,
    owner: Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Ok,
    Cancel,
}

/// The bits [`Navigator::hide`] never takes: the top segment of every page, which carries About
/// or Home and so is what the pages turn under.
const TOP_BITS: u64 = {
    let mut bits = 0;
    let mut page = 0;
    while page < MAX_PAGES {
        bits |= 1 << (page * SLOTS + ABOUT_SLOT);
        page += 1;
    }
    bits
};

/// What the screen says about the long press, in the menus and on every face. One string, so the
/// two cannot drift apart.
pub const HOLD_FOR_HOME: &str = "hold for home";

static NO_BUTTONS: [(Button, Point); 0] = [];
/// Where a menu's OK stands, and where "hold for home" stands instead of it: the box
/// `teetotum_face::HINT` uses on a face lies under the ring here.
const MENU_OK: Point = Point::new(0, 62);
static MENU_BUTTONS: [(Button, Point); 1] = [(Button::Ok, MENU_OK)];
static OK_BUTTONS: [(Button, Point); 1] = [(Button::Ok, Point::new(0, 68))];
static OK_CANCEL_BUTTONS: [(Button, Point); 2] = [
    (Button::Cancel, Point::new(-50, 68)),
    (Button::Ok, Point::new(50, 68)),
];

/// Where the user is in the menus, and what a turn or a tap means there.
///
/// Small and `Copy`, so a firmware can keep it in the state it redraws from and compare it: the
/// screen then changes exactly when the user got somewhere.
#[derive(Clone, Copy, Debug)]
pub struct Navigator {
    levels: [Option<Level>; DEPTH],
    /// How many of `levels` are in use; at least one.
    depth: usize,
    open: Option<Open>,
    /// Where [`Kind::Firmware`] leads, for a navigator that started in a plugin's menu.
    firmware: Option<&'static Menu>,
}

impl PartialEq for Navigator {
    fn eq(&self, other: &Self) -> bool {
        let same_firmware = match (self.firmware, other.firmware) {
            (Some(ours), Some(theirs)) => core::ptr::eq(ours, theirs),
            (None, None) => true,
            _ => false,
        };
        self.depth == other.depth
            && self.levels == other.levels
            && self.open == other.open
            && same_firmware
    }
}

impl Eq for Navigator {}

impl Navigator {
    /// The firmware's own settings, on About. A plugin's menu entered from here links back to
    /// `root`.
    pub fn firmware(root: &'static Menu) -> Self {
        Self::start(root, Owner::Firmware, Some(root))
    }

    /// A plugin's settings, on About, with the way into `firmware` left of it.
    pub fn plugin(root: &'static Menu, firmware: &'static Menu) -> Self {
        Self::start(root, Owner::Plugin, Some(firmware))
    }

    /// The firmware's home menu, built with [`Menu::home`], on Home, with the way into
    /// `firmware` left of it.
    pub fn home(root: &'static Menu, firmware: &'static Menu) -> Self {
        Self::start(root, Owner::Firmware, Some(firmware))
    }

    /// Leaves the entries in `slots` out of the top menu, **one bit per segment and page**: bit
    /// `page * SLOTS + slot`. They are drawn as empty, the knob passes over them and a tap finds
    /// nothing.
    ///
    /// For entries that are there only sometimes -- in the firmware's home menu, the face of a
    /// plugin that can be removed. The top segment of a page cannot be hidden, and a knob that
    /// stood on an entry now hidden goes back to the top of the first page.
    pub fn hide(&mut self, slots: u64) {
        let top = self.levels[0]
            .as_mut()
            .expect("a navigator always has its top menu");
        top.hidden = slots & !TOP_BITS;
        if top.entry(top.selected).is_none() {
            top.page = 0;
            top.selected = ABOUT_SLOT;
        }
    }

    fn start(root: &'static Menu, owner: Owner, firmware: Option<&'static Menu>) -> Self {
        let mut levels = [None; DEPTH];
        levels[0] = Some(Level {
            menu: root,
            page: 0,
            selected: ABOUT_SLOT,
            owner,
            hidden: 0,
        });
        Self {
            levels,
            depth: 1,
            open: None,
            firmware,
        }
    }

    fn level(&self) -> Level {
        self.levels[self.depth - 1].expect("a navigator always has its top menu")
    }

    fn level_mut(&mut self) -> &mut Level {
        self.levels[self.depth - 1]
            .as_mut()
            .expect("a navigator always has its top menu")
    }

    /// The menu on the screen.
    pub fn menu(&self) -> &'static Menu {
        self.level().menu
    }

    /// Whose menu is on the screen, and so whose ids [`selected`](Self::selected) names.
    pub fn owner(&self) -> Owner {
        self.level().owner
    }

    /// How deep the user is: 1 in the top menu.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// The entry the knob is on.
    pub fn selected(&self) -> Option<&'static Entry> {
        let level = self.level();
        level.entry(level.selected)
    }

    /// The dialog that is open, if one is.
    pub fn opened(&self) -> Option<(Id, Owner)> {
        self.open.map(|open| (open.id, open.owner))
    }

    /// Goes one level deeper into `menu`, which no entry has to lead to -- for a menu the owner
    /// opens itself, as the firmware does on a long press at home. OK or a long press lead back.
    pub fn enter(&mut self, menu: &'static Menu) -> Outcome {
        let owner = self.level().owner;
        self.push(menu, owner)
    }

    /// Closes the open dialog without OK or Cancel and says which it was: the way out of a
    /// dialog with [`Buttons::None`].
    pub fn dismiss(&mut self) -> Option<(Id, Owner)> {
        self.open.take().map(|open| (open.id, open.owner))
    }

    /// Opens the selected entry, as a tap in the middle would.
    pub fn open_selected(&mut self) -> Outcome {
        self.activate()
    }

    /// The knob turned by `detents`, clockwise positive.
    ///
    /// In a menu **every detent is one entry**: clockwise to the next shown one, over empty and
    /// hidden segments. A detent is what every other user of the knob counts -- a dialog, a face,
    /// the volume.
    ///
    /// In a dialog the knob belongs to the dialog, a detent at a time.
    pub fn turn(&mut self, detents: i32) -> Outcome {
        if detents == 0 {
            return Outcome::Nothing;
        }
        if let Some(open) = self.open {
            return Outcome::Adjust {
                id: open.id,
                owner: open.owner,
                detents,
            };
        }
        let level = self.level_mut();
        let before = (level.page, level.selected);
        let mut at = before;
        for _ in 0..detents.unsigned_abs() {
            at = level.step(at.0, at.1, detents > 0);
        }
        if at == before {
            return Outcome::Nothing;
        }
        (level.page, level.selected) = at;
        Outcome::Moved
    }

    /// A tap at `point`, in picture coordinates.
    ///
    /// In a menu, **a tap on a segment selects it and a tap on the selected one opens it**; the
    /// middle opens what the knob is on, and the buttons do what they say. **A finger on or past
    /// the rim counts as the ring**: on a round screen the edge is where a finger aiming at the
    /// ring lands. While a dialog is open the ring does
    /// nothing -- the way out of a dialog is its buttons, so that Cancel means something.
    pub fn tap(&mut self, point: Point) -> Outcome {
        let (dx, dy) = half_pixels(point);
        if dx * dx + dy * dy >= (2 * INNER) * (2 * INNER) {
            if self.open.is_some() {
                return Outcome::Nothing;
            }
            let (slot, _) = slot_at(dx, dy);
            let level = self.level_mut();
            if level.entry(slot).is_none() {
                return Outcome::Nothing;
            }
            if level.selected != slot {
                level.selected = slot;
                return Outcome::Moved;
            }
            return self.activate();
        }
        for &(button, at) in self.buttons() {
            if Rectangle::with_center(CENTRE + at, BUTTON_REACH).contains(point) {
                return self.press(button);
            }
        }
        match self.open {
            Some(open) => Outcome::Touch {
                id: open.id,
                owner: open.owner,
                point,
            },
            None => self.activate(),
        }
    }

    fn activate(&mut self) -> Outcome {
        let level = self.level();
        let Some(entry) = level.entry(level.selected) else {
            return Outcome::Nothing;
        };
        match entry.kind {
            Kind::Setting { id, buttons } => {
                self.open = Some(Open {
                    id,
                    buttons,
                    owner: level.owner,
                });
                Outcome::Open {
                    id,
                    owner: level.owner,
                }
            }
            Kind::Menu(menu) => self.push(menu, level.owner),
            Kind::Plugin(menu) => self.push(menu, Owner::Plugin),
            Kind::Firmware => match self.firmware {
                Some(menu) => self.push(menu, Owner::Firmware),
                None => Outcome::Nothing,
            },
            Kind::Screen(id) => Outcome::Screen {
                id,
                owner: level.owner,
            },
            Kind::Home => Outcome::Nothing,
        }
    }

    fn push(&mut self, menu: &'static Menu, owner: Owner) -> Outcome {
        if self.depth == DEPTH {
            log::warn!("Menu: {DEPTH} levels deep already, not going further");
            return Outcome::Nothing;
        }
        self.levels[self.depth] = Some(Level {
            menu,
            page: 0,
            selected: ABOUT_SLOT,
            owner,
            hidden: 0,
        });
        self.depth += 1;
        Outcome::Moved
    }

    fn press(&mut self, button: Button) -> Outcome {
        match (self.open.take(), button) {
            (Some(open), Button::Ok) => Outcome::Ok {
                id: open.id,
                owner: open.owner,
            },
            (Some(open), Button::Cancel) => Outcome::Cancel {
                id: open.id,
                owner: open.owner,
            },
            (None, _) if self.depth > 1 => {
                self.depth -= 1;
                self.levels[self.depth] = None;
                Outcome::Moved
            }
            (None, _) => Outcome::Close,
        }
    }

    /// Whether the menu on the screen lies one level below home, so that its OK would do exactly
    /// what the long press does. There the hint stands instead of OK.
    fn up_is_home(&self) -> bool {
        self.open.is_none() && self.depth == 2 && self.levels[0].is_some_and(|top| top.menu.home)
    }

    /// The buttons on the screen right now, and where their middles are from the centre.
    fn buttons(&self) -> &'static [(Button, Point)] {
        match self.open {
            None if self.level().menu.home || self.up_is_home() => &NO_BUTTONS,
            None => &MENU_BUTTONS,
            Some(Open {
                buttons: Buttons::None,
                ..
            }) => &NO_BUTTONS,
            Some(Open {
                buttons: Buttons::Ok,
                ..
            }) => &OK_BUTTONS,
            Some(Open {
                buttons: Buttons::OkCancel,
                ..
            }) => &OK_CANCEL_BUTTONS,
        }
    }

    /// Draws the ring, the names and the buttons.
    ///
    /// `value` is what the selected setting currently stands at, shown under its name while the
    /// menu is up -- the owner knows it and the navigator does not. While a dialog is open its
    /// body is the owner's to draw, inside [`BODY`], after this.
    ///
    /// **At home the value is a state, and set smaller**: a title from the
    /// phone or a line from a plugin's manifest, neither of which the firmware wrote or measured.
    /// So it stands in a Latin-1 cut, and is cut with "..." where it would run into the ring.
    ///
    /// Draws onto whatever is there, so the caller clears first.
    ///
    /// Works out the ring pixel by pixel, 98 ms with everything over it; [`draw_on`](Self::draw_on)
    /// paints the same pixels from a [`Ring`] in 13.5.
    pub fn draw<D>(
        &self,
        target: &mut D,
        palette: &Palette,
        value: Option<&str>,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        let level = self.level();
        draw_segments(target, &level, palette)?;
        self.draw_over(target, &level, palette, value)
    }

    /// Draws what [`draw`](Self::draw) does, with the ring's segments painted from `ring`.
    ///
    /// The pixels come out the same; the difference is the time, see [`Ring`].
    pub fn draw_on(
        &self,
        frame: &mut Framebuffer,
        ring: &Ring,
        palette: &Palette,
        value: Option<&str>,
    ) {
        let level = self.level();
        ring.paint(frame, &level, palette);
        let Ok(()) = self.draw_over(frame, &level, palette, value);
    }

    /// Everything over the ring's segments: their icons, the names, the buttons.
    fn draw_over<D>(
        &self,
        target: &mut D,
        level: &Level,
        palette: &Palette,
        value: Option<&str>,
    ) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = Rgb565>,
    {
        draw_icons(target, level, palette)?;
        draw_pages(target, level, palette)?;
        let selected = level.entry(level.selected);
        let name = selected.map_or("", |entry| entry.name);

        match self.open {
            None => {
                let title = level.menu.title;
                text(
                    target,
                    title,
                    CENTRE + Point::new(0, -80),
                    &fonts::SMALL,
                    palette.quiet,
                )?;
                text(
                    target,
                    name,
                    CENTRE + Point::new(0, -48),
                    largest_fitting(name, NAME_ROOM),
                    palette.name,
                )?;
                let at = CENTRE + Point::new(0, -14);
                match value {
                    Some(state) if level.menu.home => {
                        // Two lines if the state carries a newline: home says where to look
                        // before it says what to look for, and the pair centres on the line a
                        // single one would have used. The lead-in is set small and quiet, the
                        // line that matters in the state colour.
                        match state.split_once('\n') {
                            Some((lead, line)) => {
                                text(
                                    target,
                                    lead,
                                    at - Point::new(0, 11),
                                    &fonts::SMALL,
                                    palette.quiet,
                                )?;
                                fitted(
                                    target,
                                    line,
                                    at + Point::new(0, 11),
                                    state_font(line),
                                    STATE_ROOM,
                                    palette.value,
                                )?;
                            }
                            // A line that will not fit drops to the small face rather than
                            // losing its end: half a URL is worth nothing on the screen.
                            None => fitted(
                                target,
                                state,
                                at,
                                state_font(state),
                                STATE_ROOM,
                                palette.value,
                            )?,
                        }
                    }
                    Some(value) => text(target, value, at, &fonts::LARGE, palette.value)?,
                    None => {}
                }
                // Home is where the user already is, so it asks for the first tap, the one that
                // chooses; any other entry is chosen already and asks for the tap that opens it.
                let hint = match selected.map(|entry| entry.kind) {
                    Some(Kind::Home) => "tap menu entry to choose",
                    _ => "tap menu entry to open",
                };
                text(
                    target,
                    hint,
                    CENTRE + Point::new(0, 18),
                    &fonts::SMALL,
                    palette.quiet,
                )?;
            }
            Some(_) => fitted(
                target,
                name,
                CENTRE + Point::new(0, -82),
                largest_fitting(name, TITLE_ROOM),
                TITLE_ROOM,
                palette.name,
            )?,
        }

        // The buttons carry icons in the icons' colour, as the ring does: a tick and a cross say
        // OK and Cancel in any language, and a word had to fit the key.
        for &(button, at) in self.buttons() {
            let (icon, fill) = match button {
                Button::Ok => (&icons::CHECK, palette.selected),
                Button::Cancel => (&icons::CROSS, palette.ring),
            };
            draw_key(
                target,
                Rectangle::with_center(CENTRE + at, BUTTON),
                10,
                fill,
            )?;
            icon.draw(target, CENTRE + at, palette.icon)?;
        }
        // In the colour the faces write it in, so it reads as the same sentence.
        let hold = match (self.open, level.menu.hold) {
            _ if self.up_is_home() => Some(HOLD_FOR_HOME),
            (None, Some(hint)) if self.buttons().is_empty() => Some(hint),
            _ => None,
        };
        if let Some(hint) = hold {
            text(
                target,
                hint,
                CENTRE + MENU_OK,
                &fonts::SMALL,
                palette.selected,
            )?;
        }
        Ok(())
    }
}

/// A point as half pixels from the centre of the screen.
///
/// The screen has an even number of pixels, so its centre lies between two of them, at 179.5.
/// Counting in half pixels puts it on a whole number, and the twelve segments come out mirror
/// images of each other instead of one pixel off on one side.
fn half_pixels(point: Point) -> (i32, i32) {
    (
        2 * point.x - (WIDTH as i32 - 1),
        2 * point.y - (HEIGHT as i32 - 1),
    )
}

/// Which segment a point lies in by its angle, and whether it falls in the gap beside it.
///
/// `(dx, dy)` is in half pixels from the centre. **Drawing and tapping both ask this**, so what
/// the finger hits is what the eye sees, pixel for pixel.
///
/// The segment is the one whose middle points closest to the point: the largest of twelve dot
/// products, integer arithmetic only. The difference between the largest and the second is the
/// distance to the line between the two segments, times [`NEIGHBOUR_DISTANCE`], which is what
/// makes the gap straight-sided.
fn slot_at(dx: i32, dy: i32) -> (usize, bool) {
    let (slot, _, margin) = nearest(dx, dy);
    // Half the gap either side of the line, in half pixels: GAP / 2 * 2.
    (slot, margin < GAP * NEIGHBOUR_DISTANCE)
}

/// The segment a point lies in, the neighbour it is nearest to, and the difference of the two
/// dot products -- the distance to the line between them, times [`NEIGHBOUR_DISTANCE`].
fn nearest(dx: i32, dy: i32) -> (usize, usize, i32) {
    let (mut best, mut second) = ((0, i32::MIN), (0, i32::MIN));
    for (slot, &(sx, sy)) in DIRECTION.iter().enumerate() {
        let along = dx * sx + dy * sy;
        if along > best.1 {
            second = best;
            best = (slot, along);
        } else if along > second.1 {
            second = (slot, along);
        }
    }
    (best.0, second.0, best.1 - second.1)
}

/// How far in from its edges a segment is shaded, in pixels.
const BEVEL: i32 = 4;
/// How strongly, in 256ths of the way to white on an edge facing the light, to black on one
/// facing away.
const BEVEL_STRENGTH: i32 = 104;

/// How much a segment is shaded at one of its pixels, lit from the top left like a key standing a
/// little proud of the screen: the `amount` for [`shade`], 0 away from the edges.
///
/// Within [`BEVEL`] of an edge, a pixel is lightened if that edge faces the light and darkened if
/// it faces away, the more the closer it is. **Only the nearest edge counts**, so the corners need
/// no case of their own. The light is in picture coordinates and turns with the menu, which is
/// upright wherever the user has put the picture.
///
/// All in half pixels from the centre and without a square root: near a rim of radius R, the
/// distance to it is close enough to (R² - r²) / 2R, and its normal is the point's direction over R.
///
/// It depends on where the pixel is and not on the colour, which is what lets a [`Ring`] keep it.
fn bevel(slot: usize, neighbour: usize, margin: i32, dx: i32, dy: i32, distance: i32) -> i32 {
    let (outer, inner) = (2 * OUTER, 2 * INNER);
    let to_outer = (outer * outer - distance) / (2 * outer);
    let to_inner = (distance - inner * inner) / (2 * inner);
    let to_side = margin / NEIGHBOUR_DISTANCE - GAP;
    // How squarely the nearest edge faces the light, in 1024ths. The light comes from (-1, -1)
    // over the square root of two, and 724 / 1024 is one over the square root of two.
    let (near, facing) = if to_outer <= to_inner && to_outer <= to_side {
        (to_outer, -(dx + dy) * 724 / outer)
    } else if to_inner <= to_side {
        (to_inner, (dx + dy) * 724 / inner)
    } else {
        // The side towards the neighbour faces the way from this segment's middle to its.
        let ((sx, sy), (nx, ny)) = (DIRECTION[slot], DIRECTION[neighbour]);
        (to_side, -((nx - sx) + (ny - sy)) * 724 / NEIGHBOUR_DISTANCE)
    };
    lit(near, 2 * BEVEL, facing)
}

/// The shade at `near` from an edge that faces the light by `facing` 1024ths, over `width`.
fn lit(near: i32, width: i32, facing: i32) -> i32 {
    if near >= width {
        return 0;
    }
    facing * (width - near) / width * BEVEL_STRENGTH / 1024
}

/// A button: a rounded rectangle of corner `radius`, shaded like the ring's segments.
///
/// In pixels this time, not half pixels: a button has no centre to be symmetric about. A pixel
/// beyond both straight edges of a corner belongs to the corner's arc, and is dropped outside it.
fn draw_key<D>(target: &mut D, area: Rectangle, radius: i32, colour: Rgb565) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let (left, top) = (area.top_left.x, area.top_left.y);
    let (right, bottom) = (
        left + area.size.width as i32 - 1,
        top + area.size.height as i32 - 1,
    );
    target.draw_iter((top..=bottom).flat_map(move |y| {
        (left..=right).filter_map(move |x| {
            let (ox, oy) = (
                x - x.clamp(left + radius, right - radius),
                y - y.clamp(top + radius, bottom - radius),
            );
            let (near, facing) = if ox != 0 && oy != 0 {
                let distance = ox * ox + oy * oy;
                if distance > radius * radius {
                    return None;
                }
                (
                    (radius * radius - distance) / (2 * radius),
                    -(ox + oy) * 724 / radius,
                )
            } else {
                // The nearest straight edge, and which way it faces: left, right, top, bottom.
                [
                    (x - left, 1024),
                    (right - x, -1024),
                    (y - top, 1024),
                    (bottom - y, -1024),
                ]
                .into_iter()
                .min_by_key(|&(near, _)| near)
                .map(|(near, facing)| (near, facing * 724 / 1024))
                .unwrap_or((0, 0))
            };
            Some(Pixel(
                Point::new(x, y),
                shade(colour, lit(near, BEVEL, facing)),
            ))
        })
    }))
}

/// `colour` moved `amount` 256ths of the way to white, or to black if `amount` is negative.
pub fn shade(colour: Rgb565, amount: i32) -> Rgb565 {
    let towards = |value: u8, max: u8| {
        let value = i32::from(value);
        let target = if amount > 0 { i32::from(max) } else { 0 };
        (value + (target - value) * amount.abs() / 256) as u8
    };
    Rgb565::new(
        towards(colour.r(), 31),
        towards(colour.g(), 63),
        towards(colour.b(), 31),
    )
}

/// The middle of segment `slot`, halfway through the ring, in picture coordinates.
fn icon_centre(slot: usize) -> Point {
    let (sx, sy) = DIRECTION[slot];
    // Twice the middle radius, so the arithmetic stays in half pixels until the last step.
    let middle = OUTER + INNER;
    Point::new(
        (WIDTH as i32 + sx * middle / 1024) / 2,
        (HEIGHT as i32 + sy * middle / 1024) / 2,
    )
}

/// The colour of every segment of `level`, and whether it stands up as a key.
///
/// Only a segment with something in it stands up; an empty one stays flat, part of the ground.
/// A hidden entry counts as nothing.
fn segment_colours(level: &Level, palette: &Palette) -> ([Rgb565; SLOTS], [bool; SLOTS]) {
    let selected = level.selected;
    let raised: [bool; SLOTS] =
        core::array::from_fn(|slot| slot == selected || level.entry(slot).is_some());
    let colours = core::array::from_fn(|slot| match (slot == selected, raised[slot]) {
        (true, _) => palette.selected,
        (false, true) => palette.ring,
        (false, false) => palette.empty,
    });
    (colours, raised)
}

/// The twelve segments, each shaded as a raised key (see [`bevel`]), worked out pixel by pixel.
///
/// One pass over the square around the ring, one [`nearest`] per pixel inside it -- about 45000,
/// each twelve multiplications -- and no trigonometry. A [`Ring`] keeps what this works out.
fn draw_segments<D>(target: &mut D, level: &Level, palette: &Palette) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let (colours, raised) = segment_colours(level, palette);
    let outer = (2 * OUTER) * (2 * OUTER);
    let inner = (2 * INNER) * (2 * INNER);
    target.draw_iter((0..HEIGHT as i32).flat_map(move |y| {
        (0..WIDTH as i32).filter_map(move |x| {
            let point = Point::new(x, y);
            let (dx, dy) = half_pixels(point);
            let distance = dx * dx + dy * dy;
            if distance >= outer || distance < inner {
                return None;
            }
            let (slot, neighbour, margin) = nearest(dx, dy);
            // The gaps are painted black rather than left alone, so that a ground drawn only
            // inside the ring (see `crate::cloud`) leaves nothing of the frame before in them.
            if margin < GAP * NEIGHBOUR_DISTANCE {
                return Some(Pixel(point, Rgb565::BLACK));
            }
            let colour = match raised[slot] {
                true => shade(
                    colours[slot],
                    bevel(slot, neighbour, margin, dx, dy, distance),
                ),
                false => colours[slot],
            };
            Some(Pixel(point, colour))
        })
    }))
}

/// How wide a page dot is, and how far apart two of their middles stand.
const DOT: u32 = 7;
const DOT_PITCH: i32 = 15;
/// How far above the centre the row of dots sits: inside the ring, under the top segment.
const DOTS_ABOVE: i32 = INNER - 16;
/// How far down towards black a page that is not on the screen has its dot.
const DOT_DIM: i32 = 120;

/// The row of dots under the top segment: one per page, the page on the screen lit.
///
/// **Dots rather than "1/2"**: the count is read at a glance, it needs no
/// font, and it can stand under About or Home because those are on every page -- the anchor the
/// pages turn under.
///
/// Nothing is drawn for a menu of one page, which is every menu until a ring runs out of room.
fn draw_pages<D>(target: &mut D, level: &Level, palette: &Palette) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let pages = level.pages();
    if pages < 2 {
        return Ok(());
    }
    let dim = shade(palette.icon, -DOT_DIM);
    let left = CENTRE.x - (pages as i32 - 1) * DOT_PITCH / 2;
    for page in 0..pages {
        let at = Point::new(left + page as i32 * DOT_PITCH, CENTRE.y - DOTS_ABOVE);
        let colour = if page == level.page {
            palette.icon
        } else {
            dim
        };
        Circle::with_center(at, DOT)
            .into_styled(PrimitiveStyle::with_fill(colour))
            .draw(target)?;
    }
    Ok(())
}

/// The icons of the entries in `level`, each in the middle of its segment.
fn draw_icons<D>(target: &mut D, level: &Level, palette: &Palette) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    for slot in 0..SLOTS {
        if let Some(entry) = level.entry(slot) {
            entry.icon.draw(target, icon_centre(slot), palette.icon)?;
        }
    }
    Ok(())
}

/// How far out a row reaches while it stays nearer the centre than `radius`: the largest odd
/// distance in half pixels, or -1 if no pixel of the row is that near.
///
/// `dy` is the row's distance from the centre, in half pixels too. A pixel is nearer than
/// `radius` when `dx² + dy² < (2 radius)²`, the test [`draw_segments`] makes pixel by pixel, and
/// `dx` is odd because the screen has an even number of pixels.
const fn reach(dy: i32, radius: i32) -> i32 {
    let room = 4 * radius * radius - dy * dy;
    if room < 2 {
        return -1;
    }
    let widest = (room - 1).isqrt();
    widest - (1 - widest % 2)
}

/// The ring's pixels in row `y`: the columns left of the hole and those right of it, each as a
/// start and an end past the last. A row above or below the hole is split in the middle.
const fn runs(y: usize) -> [(usize, usize); 2] {
    let dy = 2 * y as i32 - (HEIGHT as i32 - 1);
    let (outer, inner) = (reach(dy, OUTER), reach(dy, INNER));
    let last = WIDTH as i32 - 1;
    [
        (((last - outer) / 2) as usize, ((last - inner) / 2) as usize),
        (
            ((last + 2 + inner) / 2) as usize,
            ((last + 2 + outer) / 2) as usize,
        ),
    ]
}

/// What a [`Ring`] needs of external RAM: two bytes for each pixel of the ring, 86 KiB.
pub const RING_BYTES: usize = {
    let mut pixels = 0;
    let mut y = 0;
    while y < HEIGHT {
        let [(left, hole), (right, end)] = runs(y);
        pixels += (hole - left) + (end - right);
        y += 1;
    }
    2 * pixels
};

/// The first byte of a [`Ring`]'s cell for a pixel in a gap; a segment's is its slot.
const GAP_CELL: u8 = SLOTS as u8;

/// Where a shade of nothing stands in the second byte of a cell, which holds [`bevel`] from -128.
const SHADE_ZERO: i32 = 128;

/// The ring's shape, worked out once: which segment each of its pixels belongs to, and how far
/// the bevel lightens or darkens it there.
///
/// **The shape never changes, only the colours do.** Worked out pixel by pixel the
/// ring took about 110 ms of every frame, twelve dot products and a bevel for each of 44500
/// pixels, and with the moving cloud it held the menus at 6.7 frames a second. With the shape
/// kept, a frame looks each pixel up in a table of shades made for its palette. Nothing about it
/// can go stale: another selection, a hidden entry or another theme changes which colour a
/// segment has, never which segment a pixel is in.
///
/// **Measured on the device:** the shape takes 80 ms once, at boot. A menu frame, the ring with
/// the names and buttons over it, takes 13.5 ms instead of 98, and under the moving cloud the
/// menus run at 17 to 18 frames a second. A run that drew both ways in three themes and compared
/// them found no pixel that differed.
///
/// Two bytes a pixel, the slot or [`GAP_CELL`] and the shade, in the order [`runs`] walks the
/// ring. [`RING_BYTES`] of them are too many for the internal RAM, so it lives in the external
/// RAM like the picture.
pub struct Ring {
    cells: &'static [u8],
}

impl Ring {
    /// Works the shape out into the first [`RING_BYTES`] of `memory`, or returns `None` if there
    /// are fewer.
    pub fn new(memory: &'static mut [u8]) -> Option<Self> {
        let cells = memory.get_mut(..RING_BYTES)?;
        let mut at = 0;
        for y in 0..HEIGHT {
            for (start, end) in runs(y) {
                for x in start..end {
                    let (dx, dy) = half_pixels(Point::new(x as i32, y as i32));
                    let (slot, neighbour, margin) = nearest(dx, dy);
                    let cell = if margin < GAP * NEIGHBOUR_DISTANCE {
                        [GAP_CELL, 0]
                    } else {
                        // Up to BEVEL_STRENGTH either way, a little more at the inner rim, where
                        // the distance to it is a little under-estimated; a byte holds either.
                        let amount = bevel(slot, neighbour, margin, dx, dy, dx * dx + dy * dy);
                        [slot as u8, (amount + SHADE_ZERO).clamp(0, 255) as u8]
                    };
                    cells[at..at + 2].copy_from_slice(&cell);
                    at += 2;
                }
            }
        }
        Some(Self { cells })
    }

    /// Paints the segments of `level` in `palette` into `frame`, the gaps black, as
    /// [`draw_segments`] would.
    fn paint(&self, frame: &mut Framebuffer, level: &Level, palette: &Palette) {
        let bytes = |colour: Rgb565| RawU16::from(colour).into_inner().to_be_bytes();
        let (colours, raised) = segment_colours(level, palette);
        // Every shade a raised segment can take, in the two colours a raised segment has.
        let selected: [[u8; 2]; 256] =
            core::array::from_fn(|i| bytes(shade(palette.selected, i as i32 - SHADE_ZERO)));
        let ring: [[u8; 2]; 256] =
            core::array::from_fn(|i| bytes(shade(palette.ring, i as i32 - SHADE_ZERO)));
        let shades: [Option<&[[u8; 2]; 256]>; SLOTS] =
            core::array::from_fn(|slot| match (raised[slot], slot == level.selected) {
                (false, _) => None,
                (true, true) => Some(&selected),
                (true, false) => Some(&ring),
            });
        // A flat segment's colour, and after the last segment the gap's.
        let flat: [[u8; 2]; SLOTS + 1] =
            core::array::from_fn(|slot| bytes(colours.get(slot).copied().unwrap_or(Rgb565::BLACK)));

        let pixels = frame.bytes_mut();
        let mut cells = self.cells.chunks_exact(2);
        for y in 0..HEIGHT {
            for (start, end) in runs(y) {
                let row = &mut pixels[(y * WIDTH + start) * 2..(y * WIDTH + end) * 2];
                for (pixel, cell) in row.chunks_exact_mut(2).zip(&mut cells) {
                    let slot = usize::from(cell[0]);
                    let colour = match shades.get(slot).copied().flatten() {
                        Some(shades) => shades[usize::from(cell[1])],
                        None => flat[slot],
                    };
                    pixel.copy_from_slice(&colour);
                }
            }
        }
    }
}

/// One line of text centred on `at`, in picture coordinates.
///
/// Public so that a dialog's body is set the way the menu around it is.
pub fn text<D>(
    target: &mut D,
    line: &str,
    at: Point,
    font: &FontRenderer,
    colour: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    match font.render_aligned(
        line,
        at,
        VerticalPosition::Center,
        HorizontalAlignment::Center,
        FontColor::Transparent(colour),
        target,
    ) {
        Err(u8g2_fonts::Error::DisplayError(err)) => Err(err),
        // A character the font does not have is left out rather than failing the whole picture.
        _ => Ok(()),
    }
}

/// How much of the width of the disc a state at home may take, in pixels.
///
/// Its line stands 14 px above the centre, so the edge of it farther from the centre is about 23
/// px up, where the inside of the ring leaves a chord of `2 * sqrt(135^2 - 23^2)` = 266 px. Twelve
/// px off the ring on either side leaves this.
const STATE_ROOM: i32 = 242;

/// Room for an open dialog's name. It stands 82 px above the centre, so its upper edge is about
/// 94 px up, where the inside of the ring leaves `2 * sqrt(135^2 - 94^2)` = 193 px. About eleven px
/// off the ring on either side leaves this, just enough for "Card over Wi-Fi" in the body face.
const TITLE_ROOM: i32 = 170;

/// Room for the selected entry's name. It stands 48 px above the centre, so the top of a large
/// face is about 64 px up, where the inside of the ring leaves `2 * sqrt(135^2 - 64^2)` = 237 px.
/// Twelve px off the ring on either side leaves this.
const NAME_ROOM: i32 = 213;

/// The face a name is set in: the largest of large, body and small that fits into `room`, so a
/// long name steps down before it is cut or runs into the ring.
fn largest_fitting(name: &str, room: i32) -> &'static FontRenderer {
    if width(name, &fonts::LARGE) <= room {
        &fonts::LARGE
    } else if width(name, &fonts::BODY) <= room {
        &fonts::BODY
    } else {
        &fonts::SMALL
    }
}

/// The face a state line is set in: the body one while it fits, the small one when it would
/// otherwise be cut.
fn state_font(line: &str) -> &'static FontRenderer {
    if width(line, &fonts::BODY_LATIN1) <= STATE_ROOM {
        &fonts::BODY_LATIN1
    } else {
        &fonts::SMALL_LATIN1
    }
}

/// One line centred on `at` like [`text`], cut with "..." where it would run past `room` pixels.
fn fitted<D>(
    target: &mut D,
    line: &str,
    at: Point,
    font: &FontRenderer,
    room: i32,
    colour: Rgb565,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    const TAIL: &str = "...";
    let Some(start) = shortened(line, font, room, TAIL) else {
        return text(target, line, at, font, colour);
    };
    // Set from the left, since the cut line and its tail are two strings that centre as one.
    let head = width(start, font);
    let mut x = at.x - (head + width(TAIL, font)) / 2;
    for part in [start, TAIL] {
        match font.render_aligned(
            part,
            Point::new(x, at.y),
            VerticalPosition::Center,
            HorizontalAlignment::Left,
            FontColor::Transparent(colour),
            target,
        ) {
            Err(u8g2_fonts::Error::DisplayError(err)) => return Err(err),
            _ => x += head,
        }
    }
    Ok(())
}

/// How wide `line` is set in `font`, in pixels: its advance, which is what [`text`] centres.
pub fn width(line: &str, font: &FontRenderer) -> i32 {
    font.get_rendered_dimensions(line, Point::zero(), VerticalPosition::Baseline)
        .map_or(0, |dimensions| dimensions.advance.x)
}

/// The longest start of `line` that fits into `room` pixels with `tail` after it, or `None` if
/// the whole line fits as it is.
///
/// For a line cut with an ellipsis. The caller appends `tail` itself, because this crate has no
/// allocator. U8g2 fonts have no kerning, so a line is exactly as wide as the advances of its
/// characters, and one pass over them finds the cut.
pub fn shortened<'a>(line: &'a str, font: &FontRenderer, room: i32, tail: &str) -> Option<&'a str> {
    if width(line, font) <= room {
        return None;
    }
    let room = room - width(tail, font);
    let (mut used, mut end) = (0, 0);
    for (at, ch) in line.char_indices() {
        used += font
            .get_rendered_dimensions(ch, Point::zero(), VerticalPosition::Baseline)
            .map_or(0, |dimensions| dimensions.advance.x);
        if used > room {
            break;
        }
        end = at + ch.len_utf8();
    }
    Some(line[..end].trim_end())
}
