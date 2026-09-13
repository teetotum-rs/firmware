//! A screenshot of the glass, down the same wire the log goes.
//!
//! The README shows this device rendered rather than photographed, and the render needs
//! something on its glass. Redrawing the interface a second time in another language would be a
//! second implementation of the same rules, wrong the day either one changes. So the firmware
//! hands over exactly what it drew: the framebuffer, byte for byte, base64 on stdout, bracketed
//! by two markers `tools/shot.py` looks for.
//!
//! It is the log's own wire, so it costs nothing to set up and nothing when unused. A full
//! picture is 259200 bytes, 345600 characters of base64; it takes a few seconds, during which
//! the interface stands still -- which is the point, since the picture must not change under
//! the dump.

use esp_println::println;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Three bytes of the picture per four characters, 96 bytes a line.
const CHUNK: usize = 96;

/// Write the framebuffer out as base64, between the markers the reader looks for.
///
/// `width` and `height` go in the header so the reader needs no agreement with us about the
/// panel; `rgb565be` is the order the panel wants and therefore the order the buffer holds.
pub fn dump(bytes: &[u8], width: usize, height: usize) {
    println!("--- shot {width}x{height} rgb565be {} bytes ---", bytes.len());
    let mut line = [0u8; CHUNK / 3 * 4];
    for block in bytes.chunks(CHUNK) {
        let mut n = 0;
        for triple in block.chunks(3) {
            let b0 = triple[0] as u32;
            let b1 = *triple.get(1).unwrap_or(&0) as u32;
            let b2 = *triple.get(2).unwrap_or(&0) as u32;
            let word = b0 << 16 | b1 << 8 | b2;
            line[n] = ALPHABET[(word >> 18 & 63) as usize];
            line[n + 1] = ALPHABET[(word >> 12 & 63) as usize];
            // A short last triple is padded with '=' rather than with zeroes, so the reader
            // does not have to trust the byte count in the header.
            line[n + 2] = if triple.len() > 1 { ALPHABET[(word >> 6 & 63) as usize] } else { b'=' };
            line[n + 3] = if triple.len() > 2 { ALPHABET[(word & 63) as usize] } else { b'=' };
            n += 4;
        }
        println!("{}", core::str::from_utf8(&line[..n]).unwrap_or(""));
    }
    println!("--- end shot ---");
}
