/// A one-colour picture of 24 by 24 pixels, the size of the icons in the settings ring.
///
/// Written as ASCII art, `#` for ink and anything else for none, so that it can be drawn with
/// nothing but an editor -- the firmware's own icons are written the same way. It is packed at
/// compile time into one `u32` per row, bit 23 leftmost, and that is also how it travels: in the
/// manifest, and into a drawing call. The colour is the caller's, not the icon's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icon {
    rows: [u32; Icon::SIZE],
}

impl Icon {
    /// Rows, and pixels in a row.
    pub const SIZE: usize = 24;

    /// # Panics
    ///
    /// If a row is not 24 characters -- at compile time, where icons are made.
    pub const fn new(art: &[&str; Self::SIZE]) -> Self {
        let mut rows = [0u32; Self::SIZE];
        let mut y = 0;
        while y < Self::SIZE {
            let row = art[y].as_bytes();
            assert!(row.len() == Self::SIZE, "an icon row is 24 characters");
            let mut x = 0;
            while x < Self::SIZE {
                if row[x] == b'#' {
                    rows[y] |= 1 << (Self::SIZE - 1 - x);
                }
                x += 1;
            }
            y += 1;
        }
        Self { rows }
    }

    /// An icon already packed, bit 23 of each row leftmost.
    pub const fn from_rows(rows: [u32; Self::SIZE]) -> Self {
        Self { rows }
    }

    pub const fn rows(&self) -> &[u32; Self::SIZE] {
        &self.rows
    }
}
