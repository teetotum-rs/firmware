// The links behind the QR codes, one a segment of the ring a long press opens at home.
//
// Plain items and nothing to import, because `build.rs` includes this file as well: it encodes
// every `url` at build time, so the firmware carries finished codes and no encoder.

/// One segment of the QR ring.
pub struct Link {
    /// Segment, clockwise from twelve o'clock. The first link stands at twelve.
    pub slot: usize,
    /// The entry's name in the ring.
    pub name: &'static str,
    /// Above the code: what it leads to.
    pub caption: &'static str,
    /// Below the code, always: the base URL, without scheme or path.
    pub host: &'static str,
    pub url: &'static str,
}

pub const LINKS: [Link; 10] = [
    Link {
        slot: 0,
        name: "TeeToTum",
        caption: "firmware and docs",
        host: "github.com",
        url: "https://github.com/teetotum-rs/firmware",
    },
    Link {
        slot: 1,
        name: "Claude Code",
        caption: "AI coding agent",
        host: "claude.com",
        url: "https://claude.com/claude-code",
    },
    Link {
        slot: 2,
        name: "Waveshare",
        caption: "the hardware",
        host: "waveshare.com",
        url: "https://www.waveshare.com/wiki/ESP32-S3-Knob-Touch-LCD-1.8",
    },
    Link {
        slot: 3,
        name: "Espressif",
        caption: "the chips",
        host: "espressif.com",
        url: "https://www.espressif.com/",
    },
    Link {
        slot: 8,
        name: "Author's blog",
        caption: "the author's blog",
        host: "stefangruehn.github.io",
        url: "https://stefangruehn.github.io/",
    },
    Link {
        slot: 4,
        name: "wasmi",
        caption: "the plugin runtime",
        host: "github.com",
        url: "https://github.com/wasmi-labs/wasmi",
    },
    Link {
        slot: 10,
        name: "Plugin guide",
        caption: "writing a plugin",
        host: "github.com",
        url: "https://github.com/teetotum-rs/firmware/blob/main/docs/plugin-development.md",
    },
    Link {
        slot: 5,
        name: "esp-rs",
        caption: "Rust on Espressif",
        host: "github.com",
        url: "https://github.com/esp-rs",
    },
    Link {
        slot: 9,
        name: "Issues",
        caption: "report a problem",
        host: "github.com",
        url: "https://github.com/teetotum-rs/firmware/issues",
    },
    Link {
        slot: 11,
        name: "Code quality",
        caption: "code quality",
        host: "github.com",
        url: "https://github.com/teetotum-rs/firmware/blob/main/docs/code-quality.md",
    },
];
