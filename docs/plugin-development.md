# Writing plugins

A guide and reference for Rust developers who want to write a plugin for the TeeToTum firmware
on the Waveshare ESP32-S3 Knob Touch LCD 1.8: a round 360 x 360 touch display with a rotary
knob around it and a haptic motor inside.

In the code a plugin is called a **face**, because what it gets is the screen: while it is shown,
it owns what is on the display and hears the taps and wipes on it. The SDK is the crate
[`teetotum-face`](../teetotum-face/src/lib.rs) in this repository.

If you want to know what the bundled plugins do or how a user removes one, read
[Using plugins](plugins.md). For the device itself — Home, the settings, flashing — read the
[user guide](user-guide.md).

> **Not stable yet.** Everything below describes the code as it stands in this repository.
>
> - `teetotum-face` is **not published on crates.io**. A plugin depends on it by path.
> - The API and the manifest format may change without notice. The manifest is at format 3, and
>   formats 1 and 2 are no longer read.
> - **Plugins come from two places:** the list `BUNDLED` in `firmware/src/bin/main.rs`, which
>   embeds each `.wasm` file in the firmware image with `include_bytes!`, and the sixteen slots of
>   the `plugins` partition, written over USB with `tools/teetotum-pack pack` and accepted on the glass
>   (see [Quick start](#2-quick-start)). Loading a plugin from the SD card, over Wi-Fi or over
>   Bluetooth is not implemented.

## Contents

1. [Overview](#1-overview)
2. [Quick start](#2-quick-start)
3. [Project layout](#3-project-layout)
4. [The `face!` macro and the manifest](#4-the-face-macro-and-the-manifest)
5. [Events](#5-events)
6. [Drawing](#6-drawing)
7. [Host calls](#7-host-calls)
8. [Resource limits](#8-resource-limits)
9. [Keeping a plugin small](#9-keeping-a-plugin-small)
10. [Lifecycle and errors](#10-lifecycle-and-errors)
11. [Testing and debugging](#11-testing-and-debugging)
12. [Licensing and contributing](#12-licensing-and-contributing)
13. [API reference](#13-api-reference)

## 1. Overview

### The model: the firmware draws, the plugin names what to draw

A plugin is a WebAssembly module, compiled from `no_std` Rust for the target `wasm32v1-none`.
The firmware runs it with the [`wasmi`](https://github.com/wasmi-labs/wasmi) 2.0 interpreter.
The plugin exports two functions and imports a few from the firmware:

```text
            firmware (native, Xtensa)                      plugin (wasm, interpreted)
  +------------------------------------------+        +-------------------------------+
  | touch, knob, radio, other chip           |        |                               |
  |        |                                 |        |                               |
  |        v                                 | event  |                               |
  |  Plugin::event(Event) -------------------+------->| on_event(u32) -> u32          |
  |        ^                                 |        |   Face::event(&mut self, ..)  |
  |        |  send_usage / random /          |<-------+   may call send, random,      |
  |        |  nearby / pulse                 |        |   nearby, pulse               |
  |        |                                 | "true" |                               |
  |  redraw wanted  <------------------------+--------+ returns true: draw me again   |
  |        |                                 |        |                               |
  |  clear to black                          |  draw  |                               |
  |  Plugin::draw ---------------------------+------->| draw()                        |
  |        ^                                 |        |   Face::draw(&self)           |
  |        |  text / arc / icon: each call   |<-------+   calls text, arc, icon       |
  |        |  is checked and kept in a list  |        |                               |
  |  render the list into the framebuffer    |        +-------------------------------+
  |  write "hold for home" into HINT         |
  |  send the picture to the panel           |
  +------------------------------------------+
```

The central rule: **the firmware draws, the plugin only names what to draw.** A plugin never
holds the framebuffer. During `draw` it makes calls such as "text at (180, 170), large, in the
theme's name colour". The firmware checks every call and appends it to a *draw list*. Once
`draw` has returned, the firmware renders the list natively. There are two reasons:

- **Speed.** Interpreted code runs about 30 times slower than native code. A pixel loop in the
  plugin would be slow; a whole face of three drawing calls takes about 19 µs.
- **Safety.** A plugin that traps halfway through `draw` leaves a picture that was never begun,
  not half of one, because nothing is rendered until `draw` returns.

### The sandbox

A plugin can reach nothing but its own memory and the functions the firmware offers:

- **Memory.** It gets exactly one 64 KiB page of linear memory, which the firmware provides.
  It cannot see the firmware's memory, other plugins, or the hardware.
- **Rights.** Every host call beyond drawing needs a *right*, and the plugin declares its rights
  in a manifest stored inside the `.wasm` file. The loader reads the manifest and checks the
  module's imports against it **before any plugin code runs**. A module that imports
  `send_usage` without the `HID` right is refused at load. So what the settings show as a
  plugin's rights is all it can do.
- **Time.** Each call into the plugin gets a fixed budget of wasmi *fuel*. A plugin caught in an
  endless loop runs out of fuel and traps. The device stutters for a few milliseconds; it does
  not hang.
- **Faults.** A trap (a panic, running out of fuel, a call the firmware refuses) stops the
  plugin. The firmware says so on the screen, and everything else keeps working.

What this does **not** give you: the ESP32-S3 has no MMU and esp-hal sets up no memory
protection, so the isolation comes entirely from the interpreter. There is no protection
against a plugin that is merely annoying within its budget, for example one that pulses the
motor for as long as it is shown.

Some things belong to the firmware and a plugin cannot take them:

- **The long press.** Holding a finger on the screen for about 600 ms always goes to Home,
  whatever is shown. It never reaches a plugin.
- **The hint box.** "hold for home" is written into the box [`HINT`](#the-hint-box) after the
  plugin has drawn, so a plugin cannot cover it.
- **The knob.** Detents go to the other chip's volume control unless the plugin has the `KNOB`
  right.

### Why WebAssembly, and not native code

Native code was ruled out for third-party plugins. The Xtensa backend of Rust cannot build
position-independent code, so a native plugin would have to be linked for a fixed address.
And without memory protection a native plugin could overwrite the firmware. WebAssembly under
wasmi gives:

- a sandbox that cannot write outside its own memory,
- a toolchain that is a stock `rustup` target (no ESP toolchain needed for the plugin),
- small modules: the bundled HID remote is 1381 bytes,
- validation of foreign modules before they run.

The price is speed (about 30x slower than native) and flash in the firmware image (wasmi was
measured at about 820 KB of `.text`). That is why the drawing model above keeps the pixel work
native.

## 2. Quick start

This takes you from nothing to your own face on the device: build a plugin, embed it in the
firmware, flash.

### Prerequisites

For **the plugin alone** you need no ESP toolchain:

- Rust 1.88 or newer (the SDK uses edition 2024), installed with `rustup`.
- The WebAssembly target:

  ```sh
  rustup target add wasm32v1-none
  ```

For **putting it on a device** you also need what the firmware needs: the Xtensa toolchain from
`espup` (`channel = "esp"` in `rust-toolchain.toml`) and `espflash`. The
[user guide](user-guide.md) describes that setup.

**Inside this repository** everything under it, `plugins/` included, builds with the `esp`
toolchain, because `rust-toolchain.toml` and `.cargo/config.toml` at the root apply. The root
config sets `[unstable] build-std = ["alloc", "core"]`, which builds `core` for `wasm32v1-none`
from source, so the example plugins build without `rustup target add`. Outside the repository
the stock target is enough.

### 1. Copy an example

The smallest example is `plugins/hid-remote`. Copy it, together with its hidden `.cargo`
directory:

```sh
cp -r plugins/hid-remote plugins/my-face
rm -rf plugins/my-face/target
```

In `plugins/my-face/Cargo.toml`, change `name = "hid-remote"` to `name = "my-face"`. In
`plugins/my-face/build.sh`, change the file names in the `cp` line (Cargo turns `-` into `_`,
so the build output is `my_face.wasm`):

```sh
cp target/wasm32v1-none/release/my_face.wasm "$out/my-face.wasm"
echo "$out/my-face.wasm: $(wc -c < "$out/my-face.wasm") bytes"
```

If the plugin lives **outside** this repository, point the dependency at the SDK by absolute or
relative path, for example:

```toml
[dependencies]
teetotum-face = { path = "/path/to/knob-display-rust/teetotum-face" }
```

### 2. Write the face

Replace `src/lib.rs` with this counter. The knob counts, a tap resets, a wipe changes the
colour of the number:

```rust
//! A counter: the knob counts, a tap resets, a wipe changes the colour.

#![no_std]

use teetotum_face::{Colour, Event, Face, Icon, Rights, Size, face};

const ICON: Icon = Icon::new(&[
    "........................",
    "........................",
    "........................",
    "........######..........",
    "......##########........",
    ".....###......###.......",
    "....###........###......",
    "....##..........##......",
    "...###..........###.....",
    "...##............##.....",
    "...##............##.....",
    "...##............##.....",
    "...##............##.....",
    "...##............##.....",
    "...##............##.....",
    "...###..........###.....",
    "....##..........##......",
    "....###........###......",
    ".....###......###.......",
    "......##########........",
    "........######..........",
    "........................",
    "........................",
    "........................",
]);

const COLOURS: [Colour; 3] = [Colour::VALUE, Colour::SELECTED, Colour::rgb(0xFFB547)];

struct Counter {
    count: i32,
    colour: usize,
}

impl Face for Counter {
    fn event(&mut self, event: Event) -> bool {
        match event {
            Event::Clockwise => self.count += 1,
            Event::Anticlockwise => self.count -= 1,
            Event::Tap => self.count = 0,
            Event::WipeLeft | Event::WipeRight => self.colour = (self.colour + 1) % COLOURS.len(),
            _ => return false,
        }
        true
    }

    fn draw(&self) {
        teetotum_face::arc(180, 180, 160, 135, 270, 12, Colour::RING);
        teetotum_face::icon(&ICON, 180, 110, Colour::ICON);
        let mut buf = [0u8; 12];
        teetotum_face::text(decimal(self.count, &mut buf), 180, 170, Size::Large, COLOURS[self.colour]);
        teetotum_face::text("turn to count, tap to reset", 180, 220, Size::Small, Colour::QUIET);
    }
}

/// `n` in decimal, without `core::fmt`.
fn decimal(n: i32, buf: &mut [u8; 12]) -> &str {
    let mut at = buf.len();
    let mut rest = n.unsigned_abs();
    loop {
        at -= 1;
        buf[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if n < 0 {
        at -= 1;
        buf[at] = b'-';
    }
    // SAFETY: only ASCII was written from `at` on.
    unsafe { core::str::from_utf8_unchecked(&buf[at..]) }
}

face! {
    name: "Counter",
    summary: "counts detents of the knob",
    icon: ICON,
    rights: Rights::KNOB,
    face: Counter = Counter { count: 0, colour: 0 },
}
```

This exact code was built against the current SDK and comes out at 1135 bytes. Note what it
does *not* use: `format!`, `write!` or `{}`. Formatting pulls in `core::fmt`, which would be
most of the module (see [Keeping a plugin small](#9-keeping-a-plugin-small)).

### 3. Build

```sh
./plugins/my-face/build.sh
```

This is a plain `cargo build --release` (the target and linker flags come from
`.cargo/config.toml`), a copy of the result to `firmware/assets/plugins/my-face.wasm` and a
signature. It prints the size and the start of the signing key. Outside the repository,
`cargo build --release` leaves the module at `target/wasm32v1-none/release/my_face.wasm`, and
you sign it yourself, with `tools/teetotum-pack` from a copy of this repository:

```sh
tools/teetotum-pack sign my_face.wasm     # sign (again)
tools/teetotum-pack check my_face.wasm    # verify, as the firmware does
```

`tools/teetotum-pack` builds the host tool in `teetotum-pack/` and runs it. It needs the `stable`
Rust toolchain next to the `esp` one (`rustup toolchain install stable`).

**The firmware loads only signed plugins.** `teetotum-pack sign` appends your Ed25519 key and a
signature over the module as its last section, `teetotum.signature`, replacing an earlier one.
The key is an Ed25519 private key in PEM, as `openssl genpkey -algorithm ed25519` writes it:
`--key`, else `$TEETOTUM_KEY`, else `~/.config/teetotum/face-key.pem`, created on first use. **Keep it and back it up.** Your key and
the plugin's name together are the plugin's identity: the Knob remembers a removed plugin by
it, and a build signed with another key is a different plugin. There is no central authority;
any key is accepted, but the bytes must be the ones that key signed.

A face that does not fit the SDK's limits fails **at compile time**. The manifest is a constant,
so a summary that is too long, for example, is a compile error:

```text
error[E0080]: evaluation panicked: a face's summary is at most 32 bytes
```

### 4. Install it on the device

If the Knob already runs TeeToTum, the plugin needs no firmware build: write it into a slot of the
`plugins` partition. With the device on USB (native USB of the ESP32-S3, see the
[user guide](user-guide.md) about the cable orientation), from the repository root:

```sh
tools/teetotum-pack pack firmware/assets/plugins/my-face.wasm --slot 0 --write
```

`pack` checks the manifest and the signature, writes `my-face.slot` next to the module (a 64-byte header
with a magic, the plugin's id, the length and the start of the module's SHA-512, then the module)
and calls `espflash write-bin -B 921600` at the slot's address from `partitions.csv`. espflash
restarts the board, and the firmware asks on the glass before the plugin gets a place (step 5).
There are sixteen slots of 64 KiB, the header included. Write a new build into the same slot: a
slot written again asks again, and of two slots holding the same plugin the lower one is used.
[Installing other plugins](plugins.md#installing-other-plugins) describes the same from the
user's side, emptying a slot included.

**Or build it into the firmware**, the way the bundled plugins are. Open `firmware/src/bin/main.rs` and find `BUNDLED`. Add your module **at the end** and raise
the array length by one:

```rust
const BUNDLED: [&[u8]; 4] = [
    include_bytes!("../../assets/plugins/hid-remote.wasm"),
    include_bytes!("../../assets/plugins/teetotum-plugin.wasm"),
    include_bytes!("../../assets/plugins/nearby.wasm"),
    include_bytes!("../../assets/plugins/my-face.wasm"),
];
```

**Why at the end:** the order decides where each plugin stands in the rings. The settings record
stores removed plugins by their identity (key and name), not by position; only records written
before version 11 used positions, so the first three entries stay where they are.

**How many fit:** a ring holds nine plugin faces at Home (segments 1 to 9) and five plugin menus
in the settings (segments 6 to 10). **Beyond that the ring runs on to a second page**, which
holds eleven (segments 1 to 11, everything but the top one); `place` in `main.rs` works out a
plugin's page and segment, and `pages_for` how many pages a ring needs. Compile-time assertions
next to `BUNDLED` check that neither ring needs more than `menu::MAX_PAGES` = 5 pages and that
the settings record can still keep the plugins apart (`Settings::PLUGINS_MAX` = 16). Sixteen is
the binding limit, and sixteen fit on two pages; with three there is no paging to see.

**Only the top entry repeats**: About, or Home in the home menu. A page after the first is built
with `Menu::new` or `Menu::home_page` and carries nothing else the pages before it carry, so the
gear and the Music Player stand on the first page alone -- which is also why a later page starts
its plugins at one o'clock rather than where the first page has them. A row of dots under the top segment counts the pages. Nothing in a
face notices any of this: it has a place in both rings either way.

A new plugin appears on Home right away, after the plugins before it. Home keeps no gaps: a
plugin the user removes gives up its segment, and the ones after it move up.

### 5. Find it

If you built the plugin into the firmware, flash it first, from the repository root:

```sh
cargo run --release
```

This builds the firmware, flashes it with `espflash` at 921600 baud and starts the monitor. For a
plugin in a slot, `espflash monitor` shows the log. Then, on the device:

1. A plugin from a slot is offered first, in a dialog with its name, `unknown key` (unless it is
   signed with the project's key), the start of your key, its rights, version, size and heap
   estimate. Tap the tick; the Knob restarts. A bundled plugin skips this step.
2. Home appears after boot. Your face is on the ring right of the house, after the bundled ones.
   Turn the knob or tap its segment to select it, tap again to open it. The plugin is loaded now,
   not before.
3. Turn the knob. The number counts, and each detent clicks.
4. Hold a finger on the screen to go back to Home.
5. In the settings (the gear on Home) your plugin has its own segment, with **About** (size,
   rights, load time, heap) and **Installed**.

In the monitor you should see a line of this form when the face opens:

```text
Plugin: Counter loaded in <microseconds> us, heap +<bytes> bytes, <bytes> free
```

## 3. Project layout

A plugin is a small Cargo package of its own. Three files matter besides `src/lib.rs`.

### `Cargo.toml`

```toml
[package]
edition = "2024"
license = "MIT OR Apache-2.0"
name    = "hid-remote"
publish = false
version = "0.0.0"

[lib]
crate-type = ["cdylib"]

[dependencies]
teetotum-face = { path = "../../teetotum-face" }

[profile.release]
codegen-units = 1
lto           = true
opt-level     = "z"
panic         = "abort"
strip         = true

# Its own workspace, so that cargo does not go looking for the firmware's.
[workspace]
```

| Setting | Why |
|---|---|
| `crate-type = ["cdylib"]` | Produces a `.wasm` module with the exports `on_event` and `draw`, instead of an rlib. |
| `teetotum-face` by `path` | The SDK is not on crates.io yet. It has no dependencies of its own. |
| `opt-level = "z"`, `lto`, `codegen-units = 1` | Size. Every byte of module costs internal RAM when it is loaded (see [Resource limits](#8-resource-limits)). |
| `panic = "abort"` | No unwinding. The `face!` macro brings the panic handler, which executes `unreachable`, a trap. |
| `strip = true` | Drops the name and debug sections. The manifest section survives; all three bundled modules are stripped and carry it. |
| `[workspace]` | An empty workspace table. Without it, a plugin inside this repository would be taken as part of the firmware's workspace. |

Plugins are **not** members of the firmware workspace, on purpose: Cargo takes the build target
from the directory it runs in, not per package, and the firmware builds for
`xtensa-esp32s3-none-elf`.

### `.cargo/config.toml`

```toml
[build]
target = "wasm32v1-none"

[target.wasm32v1-none]
rustflags = [
  "-C", "link-arg=-zstack-size=4096",
  "-C", "link-arg=--initial-memory=65536",
  "-C", "link-arg=--max-memory=65536",
  "-C", "link-arg=--import-memory",
]
```

`target = "wasm32v1-none"` makes a plain `cargo build` produce WebAssembly. The four linker
flags are required:

| Flag | What it does |
|---|---|
| `-zstack-size=4096` | Makes the shadow stack 4 KiB instead of rustc's default of 1 MiB. |
| `--initial-memory=65536` | Makes the module start with exactly one 64 KiB page. |
| `--max-memory=65536` | Caps it at that one page. |
| `--import-memory` | Makes the module import its memory as `env.memory` instead of defining its own. |

**Without the first three**, rustc reserves a 1 MiB stack and the module asks for 1088 KiB of
memory. The loader refuses it with *asks for 17 pages of memory, a face gets 1*.

**Without `--import-memory`**, the module brings its own memory, which wasmi would allocate from
the chip's small internal RAM: 76.6 KB of heap, where an imported page in external RAM costs
11.3 KB (both measured). The loader refuses it with *does not import its memory*.

With all four flags the module imports `env.memory` with a minimum and maximum of one page. The
firmware creates that page in external RAM (PSRAM) and hands it in.

The 4 KiB stack lives inside the same 64 KiB page, together with your statics. Deep recursion
or large arrays on the stack overflow it; keep big buffers in the face's struct instead.

### `build.sh`

```sh
#!/bin/sh
set -eu
cd "$(dirname "$0")"
out=../../firmware/assets/plugins
mkdir -p "$out"
cargo build --release -q
cp target/wasm32v1-none/release/hid_remote.wasm "$out/hid-remote.wasm"
../../tools/teetotum-pack sign "$out/hid-remote.wasm"
```

Nothing more than a build, a copy into `firmware/assets/plugins/`, where `BUNDLED` embeds it
from, and the signature (see [Build](#3-build)). The built `.wasm` files are committed, so the firmware builds without building the plugins
first. After changing a plugin, run its `build.sh` and rebuild the firmware.

### The examples

| Plugin | Size | Rights | Shows how to |
|---|---|---|---|
| `plugins/hid-remote` | 1381 bytes | `HID`, `KNOB` | map taps, wipes and detents to phone media keys with `send`, react to `Linked`/`Unlinked` |
| `plugins/teetotum-plugin` | 2737 bytes | `KNOB`, `RANDOM` | use the knob, draw unbiased random numbers, draw a segmented ring with `arc`, format numbers without `core::fmt` |
| `plugins/nearby` | 7266 bytes | `KNOB`, `RADIO`, `HAPTIC` | read radio rounds with `nearby`, keep the motor pulsing with `pulse`, build text lines without `core::fmt`, avoid `memmove` |

## 4. The `face!` macro and the manifest

### The `Face` trait

```rust
pub trait Face {
    /// Something happened. Answer whether the face has to be drawn again.
    fn event(&mut self, event: Event) -> bool;

    /// Draw the whole face, on black, through `text`, `arc` and `icon`.
    fn draw(&self);
}
```

One value of your type lives for as long as the plugin is loaded. The firmware calls it from one
thread, one call at a time.

### The macro

Every plugin ends with exactly one `face!` invocation. The five fields are required and must
appear **in this order**:

```rust
face! {
    name: "HID remote",                       // &str, 1 to 20 bytes
    summary: "remote for the phone's player", // &str, 0 to 32 bytes
    icon: ICON,                               // an `Icon` constant
    rights: Rights::HID,                      // a `Rights` constant
    face: Remote = Remote { last: None, linked: false }, // the type, and its value at load
}
```

| Field | Type | Rules | Where the user sees it |
|---|---|---|---|
| `name` | `&str` | 1 to `manifest::NAME_MAX` = 20 bytes, UTF-8 | Large in the middle of the Home ring and of the settings ring when the segment is selected; first line of the plugin's About. |
| `summary` | `&str` | 0 to `manifest::SUMMARY_MAX` = 32 bytes, UTF-8 | Under the name on Home, in a smaller font, shortened with "..." where the ring runs out. May be empty. |
| `icon` | `Icon` | 24 x 24, see [Icons](#icons) | The plugin's segment on Home and in the settings ring. |
| `rights` | `Rights` | combine with `Rights::A.union(Rights::B)` in a constant context | The plugin's About: `rights hid knob`, or `rights none`. |
| `face` | `Type = expr` | `expr` must be a constant expression | — |

The initial value is a **constant**: it goes into the module's data, and no code of the plugin
runs until the first event. There is no constructor and no `init` call. If your face needs to
compute something first, do it lazily in the first `event`.

`Rights` implements `BitOr`, but `|` is not a `const` operation, so inside `face!` (which builds
the manifest in a `static`) write `Rights::KNOB.union(Rights::RANDOM)`.

The macro generates:

- a `static` in the link section `teetotum.manifest` holding the encoded manifest,
- a `static mut` holding your face,
- the exports `on_event(u32) -> u32` and `draw()`, which call your `Face` methods (an event
  number the SDK does not know is answered with `0` and never reaches your code),
- the `#[panic_handler]`, which executes `unreachable`. **A panic is a trap, and a trap stops
  the plugin.**

### The manifest

The manifest says what a plugin is without running any of it. It travels inside the `.wasm` as
a custom section named `teetotum.manifest`, so the module and its manifest cannot be separated
or mixed up. The firmware reads it at boot for every bundled plugin, to put the name and icon
on Home and in the settings, and again when it loads the plugin.

Format version 3, 163 bytes, laid out by hand (`teetotum-face/src/manifest.rs`):

```text
offset     content
0          format version, 3
1..5       rights, u32 little-endian
5          length of the name in bytes, 1 to 20
6..26      the name, UTF-8, padded with zeros
26..122    the icon: 24 rows, u32 little-endian, bit 23 leftmost
122        length of the summary in bytes, 0 to 32
123..155   the summary, UTF-8, padded with zeros
155..157   the host ABI the face was built against, u16 little-endian
157..163   the face's own version: major, minor, patch, u16 little-endian each
```

`face!` fills in both numbers: the ABI from `teetotum_face::abi::VERSION`, the version from your
crate's `Cargo.toml` (a pre-release suffix such as `-beta` is dropped). A firmware refuses a
plugin built against a newer ABI than its own. Formats 1 and 2 are no longer read.

### The signature

A second custom section, `teetotum.signature`, 96 bytes: the author's Ed25519 public key (32),
then the signature (64) over every byte of the module before this section. It must be the
module's **last** section, so nothing can be appended to a signed module. The key is not in the
manifest because it is not part of the source: the same source signed by someone else is someone
else's plugin. `teetotum-pack sign` writes the section; `teetotum_face::manifest::Signed` reads
it without checking the signature, which `teetotum-pack` does, on the host and in the firmware.

### Rights

| Right | Bit | Grants | Shown as |
|---|---|---|---|
| `Rights::NONE` | — | drawing, taps and wipes only | `none` |
| `Rights::HID` | `1 << 0` | `send` (import `send_usage`), events `Linked`/`Unlinked` | `hid` |
| `Rights::KNOB` | `1 << 1` | events `Clockwise`/`Anticlockwise`; the knob stops controlling the volume while the face is shown | `knob` |
| `Rights::RANDOM` | `1 << 2` | `random` | `random` |
| `Rights::RADIO` | `1 << 3` | `nearby`, event `Nearby`; the radio scans continuously while the face is shown | `radio` |
| `Rights::HAPTIC` | `1 << 4` | `pulse` | `haptic` |

Ask for what you use and nothing more: the user sees the list in the plugin's About. A right
this firmware does not know makes the whole manifest unreadable, and the plugin is refused
rather than run with less than it asked for.

### What the loader checks before running any code

In this order (`firmware/src/plugin.rs`, `instantiate` and `check_imports`):

1. **Manifest.** The custom section is found by walking the module's section headers, without
   validating anything else. Refused if the module is not WebAssembly, has no manifest, has two,
   is too short, has another format version, has an empty/too long/non-UTF-8 name or summary,
   has unknown rights, or was built against a newer host ABI.
2. **Signature.** The section `teetotum.signature` must be there, be the last section and be 96
   bytes long, and the signature must hold for the bytes before it and the key in it. Nothing
   has been compiled yet; the check runs in software on the chip.
3. **Heap estimate.** Before wasmi allocates anything, the loader estimates the heap the module
   will need from its size (`heap_needed`, see [Heap](#heap-what-limits-the-largest-plugin)) and
   refuses it if that is more than is free.
4. **Validation and compilation.** wasmi validates and translates the whole module (eagerly).
5. **Imports.** Every import must be either `env.memory` (a memory of at most one page) or one of
   the functions in the module `teetotum`: `text`, `arc`, `icon`, `send_usage`, `random`,
   `nearby`, `pulse`. The four functions behind a right are only accepted if the manifest has
   that right. There must be an imported memory.
6. **Instantiation** with limits: one memory of 64 KiB, one table, one instance. The page is
   linked in, and the module must export `on_event` and `draw` with the right signatures.

Only the functions the manifest grants are defined in the linker at all. The exact refusal
messages are listed under [Lifecycle and errors](#10-lifecycle-and-errors).

## 5. Events

`Face::event` receives one `Event` at a time. Return `true` if the face has to be drawn again.
The SDK calls a swipe a *wipe*; the user-facing guides say swipe, and both mean the same.

**Events only reach the face while it is on the screen**, with no menu open. Nothing is queued
while the user is on Home or in the settings.

| Event | Value | Right | When |
|---|---|---|---|
| `Tap` | 0 | none | A finger was put down and lifted without wiping. |
| `WipeLeft` | 1 | none | A wipe to the left, in the picture as the user sees it. |
| `WipeRight` | 2 | none | A wipe to the right. |
| `Clockwise` | 3 | `KNOB` | One detent of the knob clockwise. |
| `Anticlockwise` | 4 | `KNOB` | One detent anticlockwise. |
| `WipeUp` | 5 | none | A wipe upwards. |
| `WipeDown` | 6 | none | A wipe downwards. |
| `Nearby` | 7 | `RADIO` | A new round of radio results is in; read it with `nearby`. Also once when the face comes up. |
| `Linked` | 8 | `HID` | A phone is connected to the other chip over BLE HID. Once when the face comes up, then on every change. |
| `Unlinked` | 9 | `HID` | No phone is connected over BLE HID; `send` goes nowhere. As `Linked`. |

The details:

- **Directions are the picture's.** The touch controller reports in the frame the panel is
  mounted in, and the user can turn the picture in quarter turns. The firmware turns every
  wipe back into the picture's frame before it delivers it. `WipeLeft` means the finger went
  left as the user sees the face.
- **Taps carry no position.** There is no event with touch coordinates. A face cannot have
  buttons at different places on the screen; it has one tap, four wipes and, with `KNOB`, the knob.
- **A contact that moved at least 36 px** but was not named as a wipe by the controller is
  delivered as a wipe in the direction it moved, not as a tap. Double taps and other gestures of
  the controller are not delivered.
- **The long press never arrives.** Holding the finger still for 600 ms goes to Home, decided by
  the firmware before any screen sees the finger.
- **The firmware gives the haptic feedback.** A tap or wipe on a face, and every detent with
  `KNOB`, clicks the motor at the strength the user has set. The plugin does not need to, and
  cannot, change that.
- **Knob detents** are delivered one event per detent. At most 8 detents are delivered per pass
  of the firmware's main loop; a larger backlog is cut, so the face stops when the hand stops.
  Without `KNOB` the knob stays with the other chip, which uses it for the volume of audio
  playing through the knob.
- **`Nearby`** comes once when the face comes up, so it can start from what is already known,
  and then after every round of either radio. See [`nearby`](#nearby).
- **`Linked` / `Unlinked`** come once when the face comes up and again whenever the other chip
  reports a change of its BLE HID connection.
- The event number is a `u32`. The generated `on_event` converts it with `Event::from_u32` and
  answers `0` for a number it does not know, so a face built against this SDK ignores events a
  later firmware might add.

A typical `event` matches on what it uses and returns `false` for the rest:

```rust
fn event(&mut self, event: Event) -> bool {
    match event {
        Event::Tap => { self.running = !self.running; true }
        Event::Clockwise => { self.value += 1; true }
        Event::Anticlockwise => { self.value -= 1; true }
        _ => false,
    }
}
```

## 6. Drawing

### The picture

```text
 x:  0                       180                       359
 y:  0 +---------------------------------------------------+
       |                  ..---"""""---..                  |
       |             .-"'        -90°        '"-.          |
       |          .'       (12 o'clock)          '.        |
       |        /                                   \      |
       |       /                                     \     |
       |      |                                       |    |
  180  |      | 180°           + (180,180)         0° |    |
       |      |                                       |    |
       |       \                                     /     |
       |        \                                   /      |
       |         '.                               .'       |
  288  |           '-.     +---------------+    .-'        |
       |              '"-. |     HINT      |.-"'           |
  312  |                   +---------------+               |
       |                         90°                       |
  359  +---------------------------------------------------+
                            x 120 .. 240
```

- **360 x 360 pixels, (0, 0) at the top left**, x to the right, y down. The centre is
  (180, 180).
- **The screen is round.** Only the disc of radius 180 around the centre is visible; the corners
  of the square are not there. Near the top and bottom a line has less room than in the middle.
  At a distance `d` from the centre, a circle of radius `r` leaves a width of `2 * sqrt(r² - d²)`;
  inside the rim (`r` = 170) that is 340 px at the centre, about 318 px at `d` = 60, 288 px at
  `d` = 90, 241 px at `d` = 120, 160 px at `d` = 150.
- **Draw upright.** The user may turn the picture in quarter turns. The firmware turns the
  whole finished picture, so the face never knows.
- **Every `draw` starts from black.** The firmware clears the framebuffer before it calls
  `draw`, so draw everything, every time. Home, the menus and the Music Player without a cover
  stand on a cloud of points in the theme's colours; a face does not get it.

### The hint box

```rust
pub const HINT: Area = Area { left: 120, top: 288, right: 240, bottom: 312 }; // right, bottom exclusive
```

After your `draw`, the firmware writes "hold for home" into this box, in the theme's
`selected` colour. Whatever you draw there ends up underneath the hint. Keep it free.

The box sits in the gap at the foot of a 270-degree arc at the rim (the shape of the player's
volume arc, `arc(180, 180, 176, 135, 270, 8, ..)`: 8 px wide, its outer edge at the rim of the
screen) and inside a full ring of radius 160: the two rims that suit a round screen both leave it
free.

### When `draw` is called

- after an event for which `event` returned `true`,
- when the face comes on the screen,
- **and whenever else the firmware redraws the screen**, for example after the theme or
  orientation changed. In the current firmware the redraw is driven by a change of the device
  state, which includes an uptime counter in seconds, so `draw` is also called about once a
  second while the face is shown.

So `draw` takes `&self` and must only depict the state; it must not change it. The host calls
that change something (`send`, `random`, `nearby`, `pulse`) trap when called from `draw`.

### How the draw list works

The drawing functions do not draw. Each call is checked and appended to a list; the firmware
renders the list after `draw` has returned:

```text
  Face::draw()                    firmware, during the call        firmware, after draw returns
  ------------                    -------------------------        ----------------------------
  arc(180,180,170,135,270,8,RING) check limits, colour  -> list[0]
  icon(&ICON, 180, 118, ICON)     check 96 bytes in page -> list[1]
  text("Next", 180,170, Large, ..) check length, UTF-8   -> list[2]
  return                                                           for each entry: render it,
                                                                   reading text and icon bytes
                                                                   from the plugin's page
                                                                   then: write "hold for home"
```

Two consequences:

- **At most 64 drawing calls per `draw`** (`abi::DRAWS_MAX`). The 65th traps. Budget them: the
  teetotum draws its ring as segments only up to 20 sides; Nearby shows at most 24 dots.
- **Text and icon bytes are read after `draw` returns**, from where they were in the plugin's
  memory. A string literal or a `const` icon is always still there. A buffer on the stack is
  still there too, as long as nothing overwrote it before `draw` returned. So **do not reuse one
  buffer for two `text` calls in the same `draw`**, and be careful with helper functions that
  format into their own stack buffer: once a helper has returned, the next call can reuse the
  same stack space, and the earlier line would show the later line's bytes. Keep one buffer per
  line alive in `draw` itself, or keep prepared text in the face's struct.

### `text`

```rust
pub fn text(line: &str, x: i32, y: i32, size: Size, colour: Colour)
```

Sets one line **centred** on (`x`, `y`), horizontally and vertically. At most 128 bytes
(`abi::TEXT_MAX`); longer traps. The firmware checks that the bytes are UTF-8 (they are, coming
from a `&str`). There is no wrapping and no way to measure a line from a plugin; keep lines short.

| `Size` | Font | Characters |
|---|---|---|
| `Size::Small` | Helvetica, 14 px | ASCII and Latin-1 |
| `Size::Body` | Helvetica, 18 px | ASCII and Latin-1 |
| `Size::Large` | Helvetica bold, 24 px | ASCII only |

A character the font does not have is left out; the rest of the line still appears. Latin-1
means U+0000 to U+00FF: "Café" works in `Small` and `Body`, not the `é` in `Large`, and nothing
outside Latin-1 works at all (no emoji, no "…" (U+2026); write "..." instead).

### `arc`

```rust
pub fn arc(cx: i32, cy: i32, radius: u32, start: i32, sweep: i32, width: u32, colour: Colour)
```

An arc around (`cx`, `cy`) with `radius` to the middle of the stroke and a stroke `width`
pixels wide. Angles are in degrees: **0 is three o'clock, and positive angles run clockwise**
(so 90 is six o'clock and -90 is twelve o'clock). The arc starts at `start` and runs through
`sweep` degrees. `radius` at most 512, `width` at most 64 (`abi::RADIUS_MAX`, `abi::WIDTH_MAX`);
more traps.

Useful shapes:

```rust
// A full ring.
teetotum_face::arc(180, 180, 160, 0, 360, 16, Colour::EMPTY);
// The player's rim: from lower left, clockwise over the top, gap at the bottom, outer edge at the rim.
teetotum_face::arc(180, 180, 176, 135, 270, 8, Colour::RING);
// A dot of about 9 px: a tiny full circle with a wide stroke (as in Nearby).
teetotum_face::arc(x, y, 2, 0, 360, 5, Colour::ICON);
// A segment starting at twelve o'clock.
teetotum_face::arc(180, 180, 160, -90, 30, 16, Colour::SELECTED);
```

There are no rectangles, lines or filled shapes. A filled disc is an arc whose stroke is at least
twice its radius: `arc(x, y, r, 0, 360, 2 * r, ..)` fills a disc about `4 * r` pixels across, so
the 64 px stroke limit makes the largest such disc about 128 px across.

### `icon`

```rust
pub fn icon(icon: &Icon, x: i32, y: i32, colour: Colour)
```

Draws a 24 x 24 icon **centred** on (`x`, `y`) in one colour; pixels without ink stay as they
are.

### Icons

```rust
const ICON: Icon = Icon::new(&[
    "........................",
    "....####################",  // 24 characters per row, 24 rows
    // ...
]);
```

`Icon::new` takes 24 strings of 24 characters: `#` is ink, anything else is not. It packs the
art at compile time into one `u32` per row (bit 23 is the leftmost pixel). A row that is not 24
characters is a compile error. `Icon::from_rows([u32; 24])` takes rows already packed.

The icon in the manifest is the one shown in the menus, always in the theme's icon colour. It
can be the same constant you draw on the face, as all three examples do.

### `Colour`

A colour is either a **theme role**, which follows the user's colour theme, or a **fixed RGB565**
value. **Prefer the theme roles**: a face drawn in them looks like part of the device in every
theme the firmware has, and every one it will get.

| Constant | Role | In the firmware's words |
|---|---|---|
| `Colour::RING` | `Role::Ring` | the ring's segments: the theme's colour, dark |
| `Colour::SELECTED` | `Role::Selected` | the selected segment and the OK button: the theme's colour, bright |
| `Colour::EMPTY` | `Role::Empty` | an empty segment: darker than `RING` |
| `Colour::ICON` | `Role::Icon` | every icon in the ring |
| `Colour::NAME` | `Role::Name` | a name, a label: white |
| `Colour::VALUE` | `Role::Value` | a value to be read off the screen: green |
| `Colour::QUIET` | `Role::Quiet` | anything that explains rather than informs: grey |

Fixed colours:

```rust
Colour::rgb(0xFFB547)      // from 24-bit hex, converted to RGB565 at compile time
Colour::from_raw(0xFD89)   // a raw RGB565 value, 0x0000 to 0xFFFF
```

On the wire a colour is a `u32`: values up to `0xFFFF` are RGB565, `0x0001_0000 + role` is a
theme role. Anything else traps with *not a colour*; with the SDK's constructors you cannot make
such a value except through `from_raw`.

## 7. Host calls

Besides the three drawing calls, there are four functions that do something. Each needs a right;
a plugin that imports one without the right is refused at load, not when it calls. **All four
trap when called from `draw`.** Call them from `event`.

### `send`

```rust
pub fn send(usage: Usage)   // needs Rights::HID
```

Sends one consumer-control usage to the phone, over the BLE HID link of the board's other chip
(a classic ESP32, which the phone pairs with as `TAIJI_KNOB_HID`). The usages are collected
during the event and handed to the other chip after `event` returns; if the event traps, none is
sent.

- **At most 4 per event** (`abi::USAGES_MAX`). More are dropped, with a warning in the log.
- **Nothing comes back.** The other chip sends the report whether or not a phone is connected,
  and a report nobody receives is lost silently. Use `Event::Linked` / `Event::Unlinked` to know
  whether a phone is listening.
- **There is no volume usage**: the other chip's HID descriptor has none.

| `Usage` | ID | Notes |
|---|---|---|
| `Power` | `0x30` | |
| `Play` | `0xB0` | |
| `Pause` | `0xB1` | had no effect on the phone it was tried with; use `PlayPause` |
| `Record` | `0xB2` | |
| `FastForward` | `0xB3` | how far it skips is up to the phone's player |
| `Rewind` | `0xB4` | as `FastForward` |
| `Next` | `0xB5` | measured: skips the track |
| `Previous` | `0xB6` | |
| `Stop` | `0xB7` | |
| `PlayPause` | `0xCD` | measured: pauses and resumes |

These ten are exactly the ones the other chip's firmware was read to map. The SDK documents
`Next` and `PlayPause` as measured on a phone; the others were tried in a
test run, all working except `Pause`. What a phone's player does with each is up to the phone.

### `random`

```rust
pub fn random(buf: &mut [u8]) -> Source   // needs Rights::RANDOM
```

Fills `buf` from the chip's hardware random number generator and says where the bytes came
from:

- `Source::Physical`: the radio was running, and with it the entropy source the generator mixes
  in. In the current firmware the radio always runs, so this is what you get.
- `Source::Pseudo`: no entropy source was running. A face that promises chance should say so on
  the screen, as the teetotum does.

**At most 64 bytes per event** (`abi::RANDOM_MAX`), summed over all calls in that event. Past
that the face **traps**: unlike a usage, a random byte that is quietly not delivered is a bug
nobody sees. For an unbiased number from 1 to n, do not use `random % n`; see `throw` in
`plugins/teetotum-plugin/src/lib.rs`, which rejects draws from the incomplete last stretch.

### `nearby`

```rust
pub fn nearby(radio: Radio, into: &mut [Signal]) -> usize   // needs Rights::RADIO
```

Copies what the radio heard in its **last round** into `into`, strongest first, and returns how
many entries it filled; the rest of `into` is untouched. `Radio::Wifi` lists access points,
`Radio::Bluetooth` lists advertising BLE devices. A round keeps at most 20 Wi-Fi networks and
40 Bluetooth devices. Call it when `Event::Nearby` arrives.

While a face with `RADIO` is on the screen, the firmware runs the scans back to back, so a new
round arrives every few seconds.

A `Signal` has accessors only:

| Method | Returns |
|---|---|
| `key()` | `u32` that stands for the address: stable while the firmware runs, different after a restart |
| `strength()` | `i8`, dBm: about -30 next to the knob, about -95 at the edge of hearing |
| `channel()` | `Option<u8>`: the Wi-Fi channel, `None` for Bluetooth |
| `has_name()` | `bool` |
| `name()` | `&str`, at most 32 bytes, always whole UTF-8; `""` if none was given |

`Signal::EMPTY` is the value to fill an array with. Each `Signal` is 40 bytes of your 64 KiB
page (`abi::SIGNAL_BYTES`).

**No plugin ever sees an address.** The key is an FNV-1a hash of the address and a number the
firmware draws at boot and keeps to itself; it cannot be looked up in a list of addresses. A
Bluetooth device that changes its own address (phones do, every few minutes) comes back under a
new key.

### `pulse`

```rust
pub fn pulse(every_ms: u32)   // needs Rights::HAPTIC
```

Keeps the motor pulsing once every `every_ms` milliseconds until the next call; `0` stops it.
A plugin has no clock, so the firmware keeps the time.

- Intervals are clamped to 150 to 5000 ms (`abi::PULSE_MIN_MS`, `abi::PULSE_MAX_MS`). Below
  150 ms the clicks run together into a buzz.
- A new interval takes effect from the last pulse, so a pulse that speeds up does not wait for
  the old interval to end.
- The motor pulses **only while the face is on the screen**, not on Home or in the settings. The
  setting is kept while the plugin stays loaded, so the pulse resumes when the user comes back.
- Each pulse is the same click as a tap, **at the strength the user set**, which may be off. Do
  not rely on the pulse alone; show on the screen what it means.

## 8. Resource limits

### Per call

| Limit | Value | On violation |
|---|---|---|
| Fuel per call of `on_event` or `draw` | 20 000 units of wasmi fuel (`abi::FUEL`) | trap |
| Drawing calls per `draw` | 64 (`abi::DRAWS_MAX`) | trap |
| Bytes per `text` line | 128 (`abi::TEXT_MAX`) | trap |
| `arc` radius / stroke width | 512 / 64 px (`abi::RADIUS_MAX`, `abi::WIDTH_MAX`) | trap |
| Usages per event | 4 (`abi::USAGES_MAX`) | the rest are dropped, with a warning in the log |
| Random bytes per event | 64 (`abi::RANDOM_MAX`) | trap |
| Pulse interval | 150 to 5000 ms (`abi::PULSE_MIN_MS`, `abi::PULSE_MAX_MS`) | clamped |
| Knob detents per main-loop pass | 8 | the rest of the backlog is not delivered |

What typical calls cost, measured on the device (ESP32-S3 at 240 MHz):

| Call | Fuel | Time |
|---|---|---|
| a wipe in the HID remote (`event` with one `send`) | 24 | about 10 µs |
| a `draw` with three drawing calls | 58 | about 19 µs |

20 000 units is over three hundred such draws, roughly 7 ms by an estimate from these two
points. That is plenty for a face that names what to draw, and short enough that a face caught
in a loop costs the device a stutter. Loops over a few hundred elements are fine; anything
that computes per pixel is not what a face is for.

### Memory: one page, shared, cleared before each load

- A plugin has **exactly one 64 KiB page** of linear memory. It holds your statics (the face
  value), the 4 KiB stack, and whatever buffers you declare. There is no allocator: the SDK is
  `no_std` without `alloc`, and there is no `Vec` or `String`.
- The page lies in **external RAM (PSRAM)** and is lent to one plugin at a time. All plugins
  share the same page, one after the other.
- **The page is cleared before every load.** A plugin never sees what the one before it left
  there, and every byte your module does not initialise starts as zero. Independently of that,
  `wasm-ld` writes even the zero-initialised data (`.bss`) as a data segment when the memory is
  imported, so every static gets its initial value at instantiation.
- **There is no persistent storage.** A plugin's state lives as long as it is loaded (see
  [Lifecycle](#10-lifecycle-and-errors)) and is gone after a restart of the device.

### Heap: what limits the largest plugin

The page costs nothing in internal RAM, but wasmi's own structures for a loaded plugin do. They
come out of the firmware's internal heap (about 136 KiB), which the Wi-Fi and Bluetooth stacks
also use. Measured on the board:

| | Module | Heap while loaded | Load time |
|---|---|---|---|
| none loaded | — | 56 948 bytes free at boot | — |
| HID remote | 1 381 bytes | +11 208 bytes | about 54 ms, 34 of them the signature check |
| Teetotum (dice) | 2 737 bytes | +14 112 bytes | about 80 ms, 36 of them the signature check |
| Nearby | 7 266 bytes | +21 324 bytes (35 384 left free) | about 123 ms, 40 of them the signature check |

The heap was measured in the firmware on 2026-09-11 and again by the `faceheap` run on
2026-09-13, after plugins were signed: to the same byte. The load times are that run's, with no
radio running. The Ed25519 check runs in software and allocates nothing.

Where it goes:

- about **8 KB of base cost** (engine and module about 4.5 KB, store and instance 2 KB, the
  interpreter's stack 1.6 KB from the first call on),
- plus the **translated code, about 2.4 bytes of heap per byte of the module's code section**,
- plus, only while loading, a **peak 3 to 5 KB** above what stays. The data segments are copied
  into the page at instantiation and their heap copy is freed afterwards.

> **The loader checks this before it starts.** wasmi allocates infallibly, so a plugin that does
> not fit would take the firmware down rather than be refused. The firmware therefore estimates
> the cost from the module's size -- the 8 KB of base, 2.5 bytes per byte of module, the peak,
> and 8 KB left over for itself -- and refuses the plugin if that is more than is free. Your
> plugin's face then says `needs about <bytes> bytes of heap, <bytes> free`. The estimate is a
> generous fit to earlier measurements of the same three plugins, so it errs towards refusing:
> against the 56 948 bytes free at boot it draws the line at about **13.7 KB of module**, well
> below what would really have fitted. Keep plugins small, and measure yours (below).

The load time is validation and translation, which grow with the size of the code, plus about
4 ms to clear the page; it is paid
each time the plugin is loaded (see [Lifecycle](#10-lifecycle-and-errors)).

### Measuring your plugin

- **On the device, in the plugin's About** (Settings, the plugin's segment, About): six lines
  with the name, "bundled with the firmware" or "from slot N", the module size in bytes, the rights, the load
  time (`loaded in 19.7 ms`) and the heap cost (`9.8 KB heap`). While the plugin is not loaded
  it says `not loaded`; after a trap or refusal, `stopped` and the reason.
- **In the log**, when the face is opened:

  ```text
  Plugin: <name> loaded in <microseconds> us, heap +<bytes> bytes, <bytes> free
  ```

- **On your computer**, the code section size predicts the heap cost; see
  [Inspecting a module](#inspecting-a-module).

## 9. Keeping a plugin small

Module size is heap at run time and load time at every start. These are the techniques that
took Nearby from 10 344 to about 7 100 bytes, and its heap cost from 42 KB down to about 21 KB
(together with an interpreter setting in the firmware).

**No `core::fmt`.** `write!`, `Display`, `{}` in any form pull in the formatting machinery,
which would be most of a small module. Format numbers by hand into a byte buffer; both
`decimal` in `plugins/teetotum-plugin` and `Line` in `plugins/nearby` show how.

**Avoid copies that turn into `memmove`.** When a value is copied from one reference into
another, the compiler may be unable to tell that the two do not overlap, and calls `memmove`.
In Nearby that cost 1 225 bytes. The fix there was to find the index first and copy from the
array by index, as `List::fill` in `plugins/nearby/src/lib.rs` does now:

```rust
match self.heard().iter().position(|s| s.key() == key) {
    Some(i) => self.chosen = Some(self.signals[i]),
    None => self.chosen_heard = false,
}
```

If `memmove` shows up in your module (see [Inspecting a module](#inspecting-a-module)), look for
struct copies through references.

**Build in place, not by value.** A builder that takes and returns `self` by value copies its
whole buffer on every call. Nearby's `Line` takes `&mut self` and returns `&mut Self`:

```rust
let mut line = Line::new();
line.number(chosen.strength().into()).push(b" dBm");
teetotum_face::text(line.as_str(), CX, 186, Size::Small, Colour::VALUE);
```

**Cut text on bytes, not characters.** Counting characters needs code to walk UTF-8. Cut at a
byte length, then step back to the start of a character (a continuation byte is
`0b10xxxxxx`):

```rust
let mut end = NAME_SHOWN;
while end > 0 && name[end] & 0xC0 == 0x80 {
    end -= 1;
}
```

**Do not validate UTF-8 twice.** If you only ever put whole UTF-8 pieces into a buffer, turn it
back into a `&str` with `core::str::from_utf8_unchecked` and a `SAFETY` comment saying why,
as the examples do. The SDK does the same in `Signal::name`, and the firmware checks every
`text` line anyway.

**Mind large initial values.** Because `.bss` is written as a data segment (see
[Memory](#memory-one-page-shared-cleared-before-each-load)), zeros in your face's initial value are bytes in
the module: Nearby's lists of signals are 2 032 of them. They are cheap in heap (freed after
loading) but not free in size and load peak.

**Keep the release profile** from the examples: `opt-level = "z"`, `lto = true`,
`codegen-units = 1`, `panic = "abort"`, `strip = true`.

### Inspecting a module

No disassembler is needed to see what matters. This Python script (standard library only) lists
a module's sections with their sizes, its imports and exports, and the manifest:

```python
#!/usr/bin/env python3
"""Lists a face's sections, imports, exports and manifest."""
import sys

def leb(b, i):
    v = s = 0
    while True:
        x = b[i]; i += 1
        v |= (x & 0x7F) << s; s += 7
        if x < 0x80:
            return v, i

def name(b, i):
    n, i = leb(b, i)
    return b[i:i + n].decode(), i + n

wasm = open(sys.argv[1], "rb").read()
assert wasm[:8] == b"\0asm\x01\0\0\0", "not a wasm module"
i = 8
while i < len(wasm):
    sid, i = wasm[i], i + 1
    size, i = leb(wasm, i)
    body, i = wasm[i:i + size], i + size
    if sid == 0:
        sec, j = name(body, 0)
        print(f"custom  {sec:24} {size:6} bytes")
        if sec == "teetotum.manifest":
            m = body[j:]
            print(f"  format {m[0]}, rights {int.from_bytes(m[1:5], 'little'):#x}, "
                  f"name {m[6:6 + m[5]].decode()!r}, summary {m[123:123 + m[122]].decode()!r}")
        continue
    print(f"section {sid:2}                       {size:6} bytes")
    if sid in (2, 7):  # imports, exports
        n, j = leb(body, 0)
        for _ in range(n):
            if sid == 2:
                mod, j = name(body, j)
            nm, j = name(body, j)
            kind, j = body[j], j + 1
            if sid == 2 and kind == 2:  # a memory: flags, minimum, maybe maximum
                flags, j = body[j], j + 1
                lo, j = leb(body, j)
                hi = None
                if flags & 1:
                    hi, j = leb(body, j)
                print(f"  import {mod}.{nm}: memory, {lo} to {hi} pages")
                continue
            _, j = leb(body, j)
            print(f"  import {mod}.{nm}" if sid == 2 else f"  export {nm}")
```

For the bundled HID remote it prints (section 10 is the code, 11 the data):

```text
section  1                           47 bytes
section  2                           85 bytes
  import env.memory: memory, 1 to 1 pages
  import teetotum.arc
  import teetotum.icon
  import teetotum.send_usage
  import teetotum.text
section  3                            4 bytes
section  6                            7 bytes
section  7                           19 bytes
  export draw
  export on_event
section 10                          478 bytes
section 11                          416 bytes
custom  teetotum.manifest           181 bytes
  format 3, rights 0x3, name 'HID remote', summary "remote for the phone's player"
custom  teetotum.signature          115 bytes
```

Check three things: the memory import is `1 to 1 pages`; every `teetotum.*` import behind a
right has that right in the manifest (`send_usage` needs bit `0x1`); and the file size is
far from the roughly 13.7 KB above which the loader refuses it: the estimate counts every byte of
the module, data and manifest included, not only the code section.

To see **which functions** ended up in the module, build once without stripping and look at the
names:

```sh
CARGO_PROFILE_RELEASE_STRIP=false cargo build --release
strings -n 4 target/wasm32v1-none/release/my_face.wasm | grep -E 'memmove|memcpy|memset|fmt'
```

Names such as `memmove` or `core::fmt` functions show what to hunt down.

## 10. Lifecycle and errors

### From boot to stop

```text
 boot
  |  offer each plugin waiting in a slot (install dialog); restart after an OK
  |  read the manifests, bundled and from slots (no code runs, nothing is loaded)
  |  put name, icon, summary on Home; plugins the user removed get no segment
  v
 Home  --- user opens the face ---------------------------------------------+
  ^                                                                         |
  |                                              same plugin loaded, not stopped?
  |                                                 |yes              |no
  |                                                 |                 v
  |                                                 |     unload the plugin that ran before
  |                                                 |     load this one: manifest, validate,
  |                                                 |     check imports, instantiate
  |                                                 |        |ok               |refused
  |                                                 v        v                 v
  |                                              RUNNING <---+           "stopped" screen
  |                                                 |                    with the reason
  |   long press: back to Home ---------------------+  (stays loaded, state kept;
  |                                                 |   no events, no pulse, no rounds)
  |                                                 |
  |                                                 | a trap in event or draw
  |                                                 v
  +---------------------------------------------- STOPPED
                                                  "stopped" screen with the reason;
                                                  opening the face again loads it anew
```

- **Nothing is loaded at boot.** A plugin is loaded when its face is opened on Home.
- **One plugin runs at a time.** Opening another plugin's face unloads the one before, and all
  of its state is gone. Next time it starts from its initial value.
- **The one that ran last stays loaded** until another plugin is started or it is removed. Going
  to Home or the Music Player and back finds it as it was left.
- **A trap stops the plugin for good** (until it is loaded again). The firmware logs it, the face
  shows the plugin's name, "stopped" and the reason, and the About says the same. Opening the
  face again from Home loads it anew, from its initial value. The instance would still answer
  calls, but a plugin that trapped holds whatever state the trap cut it off in.
- If `event` traps, the usages it sent are not sent.
- **Removing** a plugin (its settings, Installed: No) saves the choice and restarts the Knob.
  Home is laid out again without it, and the faces after it move up. Installing it again (Yes)
  restarts the same way and loads nothing; the plugin is loaded the next time its face is opened.
  The bytes stay where they were, in the firmware image or in the slot. [Using plugins](plugins.md) describes this from the user's side.

### Refused at load

The face stays on Home; opening it shows "stopped" and the reason, and the log says
`Plugin: refused -- <reason>`. The reasons, as the firmware words them:

| Message | Cause |
|---|---|
| `not a WebAssembly module` | the bytes do not start like a module, or its sections do not add up |
| `no manifest` | no `teetotum.manifest` section: the module was not built with `face!` |
| `two manifests` | two such sections |
| `manifest too short` | shorter than 163 bytes |
| `manifest format N, this firmware reads 3` | built against another version of the SDK |
| `built for host ABI N, this firmware offers N` | built against a newer SDK than this firmware |
| `not signed` | no `teetotum.signature` section: run `tools/teetotum-pack sign` |
| `signature section malformed or not last` | something was appended after signing, or the section is not 96 bytes |
| `signature does not match its bytes and key` | the module changed after it was signed, or was signed with another key than the one it names |
| `manifest name empty, too long or not UTF-8` | |
| `manifest summary too long or not UTF-8` | |
| `rights 0x.. include some this firmware lacks` | built for a newer firmware |
| `needs about N bytes of heap, N free` | the module is too large for the free internal heap; see [Heap](#heap-what-limits-the-largest-plugin) |
| `does not import its memory` | built without `--import-memory` |
| `asks for N pages of memory, a face gets 1` | built without the memory flags |
| `imports M.N, which the firmware does not offer` | an import that is not one of the seven functions or `env.memory` |
| `imports send_usage without the right hid in its manifest` | a right-gated import without the right; likewise `random`/`random`, `nearby`/`radio`, `pulse`/`haptic` |
| (a wasmi error) | invalid module, a missing or mistyped `on_event`/`draw` export, limits exceeded |

With the SDK and the example's `.cargo/config.toml` only the heap refusal and the last group can
normally happen;
the manifest and imports are right by construction, since `face!` writes the manifest and the
compiler only imports the functions you call.

### Traps while running

A trap is any error while the plugin runs. The log says `Plugin: <name> stopped -- <error>`, and
the error text also stands on the screen. The firmware's own checks produce these messages:

| Message | Cause |
|---|---|
| `drawing outside draw` | `text`, `arc` or `icon` called from `event` |
| `send_usage from draw`, `random from draw`, `nearby from draw`, `pulse from draw` | a host call from `draw` |
| `more drawing calls than DRAWS_MAX` | the 65th drawing call in one `draw` |
| `text longer than TEXT_MAX` | a line over 128 bytes |
| `arc larger than RADIUS_MAX or WIDTH_MAX` | radius over 512 or width over 64 |
| `random past RANDOM_MAX in one event` | more than 64 random bytes in one event |
| `not a colour`, `not a text size`, `not a usage the other chip maps`, `not a radio` | a raw value the SDK's types cannot produce |
| `text is not UTF-8`, `points outside the face's memory`, `random outside the face's memory`, `nearby outside the face's memory`, `span overflows` | a bad pointer or length; only possible by going around the SDK |

Besides these, wasmi itself traps, for example on running out of fuel, on `unreachable` (which
is what a **panic** becomes, including an index out of bounds or an `unwrap` on `None`), and on
a memory access outside the page. Their messages are wasmi's.

## 11. Testing and debugging

There is no simulator; a face is tested on the device.

**A plugin cannot log.** There is no import for it. To see a value while you develop, draw it:
a line of `Size::Small` in `Colour::QUIET` costs one of your 64 drawing calls.

**Read the firmware log.** Everything the loader decides is logged. Three ways to read it:

- `cargo run --release` from the repository root flashes and then stays in the interactive
  monitor. This is the normal way while you try a face with your hands.
- `espflash monitor` reads without flashing. It needs a real terminal: started from a script or
  a background job it fails with `Failed to initialize input reader`, and by then it has already
  put the chip into its serial bootloader, so the firmware is not running at all.
- `tools/listen.py --seconds 30 | tee run.log` resets the board and copies the port to standard
  output, without needing a terminal (Python with `pyserial`). Use it for runs that do not need
  a hand.

The lines that concern plugins:

| Log line | When |
|---|---|
| `Plugin: none loaded yet, heap N bytes free` | at boot |
| `Plugin: slot N, N bytes, id [..], waits to be accepted` | at boot, for a slot written but not yet accepted |
| `Plugin: slot N not offered -- <reason>` | at boot, for a waiting plugin whose manifest or signature does not hold; no dialog opens |
| `Plugin: slot N offered for install` | the install dialog opens |
| `Plugin: slot N accepted` | the tick in the install dialog |
| `Plugin: slot N not accepted, asked again at boot` | the cross or a long press in the install dialog |
| `Plugin: N accepted -- restarting` | after the last install dialog |
| `Plugin: slot N, N bytes, id [..], in place N` | at boot, for an accepted slot, with its place in the rings |
| `Plugin: slot N skipped, <reason>` | at boot: another slot holds the same plugin, the rings are full, or no external RAM is left |
| `Plugin: plugin N has no usable manifest -- <reason>` | at boot, for a module whose manifest cannot be read; it gets no segment on Home |
| `Plugin: installed plugins changed -- restarting` | Installed was changed and confirmed |
| `Plugin: <name> loaded in N us, heap +N bytes, N free` | the face was opened and the plugin loaded |
| `Plugin: refused -- <reason>` | loading failed; see [Refused at load](#refused-at-load) |
| `Plugin: <Event> -> [<usages>]` | every event delivered, with the usages it sent, e.g. `Plugin: WipeRight -> [Next]` |
| `Plugin: <name> sent N usages past the limit, dropped` | more than 4 usages in one event |
| `Plugin: <name> stopped -- <error>` | a trap; see [Traps while running](#traps-while-running) |
| `Plugin: N unloaded, heap N bytes free` | another plugin was started, or this one removed |
| `Home: Plugin(N) on the glass` | a face was opened from Home |
| `Touch: <gesture>, nothing a face hears` | a controller gesture that is not delivered |

**Check the module before you flash**: run the inspection script from
[Inspecting a module](#inspecting-a-module) and compare the imports with the rights.

**A checklist when something does not work:**

- The face is not on Home: the manifest could not be read (see the boot log), or the plugin was
  removed in its settings (Installed: No).
- The face shows "stopped": the reason is on the screen, in the About and in the log.
- The knob does nothing on your face: the manifest lacks `Rights::KNOB`.
- A `send` has no effect: the phone must be paired with `TAIJI_KNOB_HID` over Bluetooth, and
  `Event::Linked` tells you whether it is connected. Pairing is described in
  [Using plugins](plugins.md).
- A line of text is missing characters: they are not in the font (`Large` is ASCII only), or the
  line was built from a buffer that was overwritten before the firmware read it (see
  [How the draw list works](#how-the-draw-list-works)).
- The face says `needs about <bytes> bytes of heap`: the plugin is larger than the free memory
  allows, and the firmware refused it rather than risk the allocation (see
  [Heap](#heap-what-limits-the-largest-plugin)).

## 12. Licensing and contributing

The firmware and the SDK are licensed `MIT OR Apache-2.0` (texts in
[`LICENSE-MIT`](../LICENSE-MIT) and [`LICENSE-APACHE`](../LICENSE-APACHE)). Neither writing a
plugin nor building the firmware into something of your own comes with conditions beyond that.

For your own plugin, the same dual licence is suggested and is what the three examples use
(`license = "MIT OR Apache-2.0"` in their `Cargo.toml`). It keeps the door open for your plugin
to be bundled or collected later without a licence patchwork.

A plugin draws text in the firmware's fonts but does not contain them. The fonts are Helvetica
from U8g2, under the notice in [`teetotum/LICENSE-FONTS`](../teetotum/LICENSE-FONTS), which
travels with the firmware.

A contributed plugin passes the same checks as the firmware: rustfmt and Clippy with every
warning an error. [Code quality](code-quality.md) describes them and the script that runs them.

Two conventions of this project that a contributed plugin should keep:

- **A plugin that shows what the radio hears nearby carries a note on the law** in its
  `README.md`: receiving and showing radio signals is regulated differently from country to
  country. `plugins/nearby/README.md` has such a section.
- **Identifiers stay harmless.** Nothing in names, comments or strings that sounds like
  surveillance or attack tooling: the radio API is called `nearby`, not "sniff", "scan",
  "track" or "target".

**There is no plugin repository or catalogue.** The SDK is not on crates.io and its API is not
stable; the examples in `plugins/` are the reference and the regression test for the API.

## 13. API reference

Everything a plugin uses is in the crate `teetotum_face`. The drawing and host functions exist
only when compiling for `wasm32`.

### Trait and macro

| Item | Signature | Notes |
|---|---|---|
| `Face` | `trait Face { fn event(&mut self, event: Event) -> bool; fn draw(&self); }` | `event` returns whether to redraw |
| `face!` | `face! { name: .., summary: .., icon: .., rights: .., face: Type = init }` | exactly once per plugin, fields in this order |

### Functions

| Function | Right | From | Limits |
|---|---|---|---|
| `text(line: &str, x: i32, y: i32, size: Size, colour: Colour)` | — | `draw` | 128 bytes; centred on (x, y) |
| `arc(cx: i32, cy: i32, radius: u32, start: i32, sweep: i32, width: u32, colour: Colour)` | — | `draw` | radius ≤ 512, width ≤ 64; degrees, 0 = three o'clock, clockwise |
| `icon(icon: &Icon, x: i32, y: i32, colour: Colour)` | — | `draw` | centred on (x, y) |
| `send(usage: Usage)` | `HID` | `event` | 4 per event, more dropped |
| `random(buf: &mut [u8]) -> Source` | `RANDOM` | `event` | 64 bytes per event, more traps |
| `nearby(radio: Radio, into: &mut [Signal]) -> usize` | `RADIO` | `event` | last round, strongest first |
| `pulse(every_ms: u32)` | `HAPTIC` | `event` | clamped to 150..5000 ms, 0 stops |

At most 64 drawing calls per `draw`, 20 000 fuel per call.

### Types

| Type | Contents |
|---|---|
| `Event` | `Tap`, `WipeLeft`, `WipeRight`, `Clockwise`, `Anticlockwise`, `WipeUp`, `WipeDown`, `Nearby`, `Linked`, `Unlinked`; `from_u32` |
| `Rights` | `NONE`, `HID`, `KNOB`, `RANDOM`, `RADIO`, `HAPTIC`; `union`, `contains`, `bits`, `from_bits`, `BitOr`, `Display` |
| `Size` | `Small` (14 px, Latin-1), `Body` (18 px, Latin-1), `Large` (bold 24 px, ASCII) |
| `Colour` | `RING`, `SELECTED`, `EMPTY`, `ICON`, `NAME`, `VALUE`, `QUIET`; `rgb(0xRRGGBB)`, `from_raw`, `to_raw`, `paint` |
| `Role`, `Paint` | the firmware's view of a colour; a plugin does not need them |
| `Icon` | `Icon::new(&[&str; 24])`, `Icon::from_rows([u32; 24])`, `rows()`, `Icon::SIZE` = 24 |
| `Area`, `HINT` | `Area { left, top, right, bottom }` (right and bottom exclusive); `HINT` = 120, 288, 240, 312 |
| `Usage` | `Power`, `Play`, `Pause`, `Record`, `FastForward`, `Rewind`, `Next`, `Previous`, `Stop`, `PlayPause` |
| `Source` | `Physical`, `Pseudo` |
| `Radio` | `Wifi`, `Bluetooth` |
| `Signal` | `key()`, `strength()`, `channel()`, `has_name()`, `name()`, `Signal::EMPTY`; `record` is the firmware's |

### Modules

| Item | Value |
|---|---|
| `manifest::SECTION` | `"teetotum.manifest"` |
| `manifest::VERSION` | 3 |
| `manifest::NAME_MAX`, `manifest::SUMMARY_MAX` | 20, 32 bytes |
| `manifest::LEN` | 163 bytes |
| `manifest::SIGNATURE`, `manifest::KEY_LEN`, `manifest::SIGNATURE_LEN` | `"teetotum.signature"`, 32, 64 bytes |
| `manifest::Manifest`, `manifest::Error`, `manifest::encode`, `manifest::Version` | reading and writing manifests; used by the firmware and `face!` |
| `manifest::Signed` | a module split at its signature section; the firmware checks the signature |
| `abi::VERSION` | 1 |
| `abi::FUEL` | 20 000 |
| `abi::DRAWS_MAX`, `abi::TEXT_MAX` | 64, 128 |
| `abi::RADIUS_MAX`, `abi::WIDTH_MAX` | 512, 64 |
| `abi::USAGES_MAX`, `abi::RANDOM_MAX` | 4, 64 |
| `abi::PULSE_MIN_MS`, `abi::PULSE_MAX_MS` | 150, 5000 |
| `abi::SIGNAL_BYTES`, `abi::NAME_BYTES` | 40, 32 |
| `abi::PAGES` | 1 |

### The raw interface

For reference, or for a plugin written without the SDK. All values are 32-bit integers; pointers
are offsets into the plugin's memory.

```text
memory    import env.memory, minimum 1 page, maximum 1 page

exports   on_event(event: i32) -> i32      nonzero: draw again
          draw()

imports   module "teetotum"
          text(at, len, x, y, size, colour)
          arc(cx, cy, radius, start, sweep, width, colour)
          icon(rows, x, y, colour)                 rows: 24 x u32 little-endian, bit 23 leftmost
          send_usage(usage)                        right HID
          random(at, len) -> source                right RANDOM; 1 physical, 0 pseudo
          nearby(radio, at, max) -> count          right RADIO; records of 40 bytes
          pulse(every_ms)                          right HAPTIC

custom    section "teetotum.manifest", 163 bytes, format 3 (see section 4)
          section "teetotum.signature", 96 bytes, last (see section 4)

colour    0x0000..0xFFFF RGB565; 0x00010000 + role for a theme role (0 ring, 1 selected,
          2 empty, 3 icon, 4 name, 5 value, 6 quiet)
size      0 small, 1 body, 2 large
radio     0 Wi-Fi, 1 Bluetooth
event     0 tap, 1 wipe left, 2 wipe right, 3 clockwise, 4 anticlockwise,
          5 wipe up, 6 wipe down, 7 nearby, 8 linked, 9 unlinked
```

The authoritative sources are `teetotum-face/src/abi.rs` and `firmware/src/plugin.rs`.
