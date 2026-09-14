# Using plugins

This guide is for people who **use** plugins on a Knob running the TeeToTum firmware. It
explains what a plugin is, how to start and remove one, what the three plugins that come with
the firmware do, and what to expect when something goes wrong.

- For the firmware in general (the controls, Home, the Music Player, the settings), see the
  [user guide](user-guide.md).
- If you want to **write** a plugin, see [plugin development](plugin-development.md).

Contents:

1. [What a plugin is](#what-a-plugin-is)
2. [Where plugins live](#where-plugins-live)
3. [The bundled plugins](#the-bundled-plugins):
   [HID remote](#hid-remote), [Teetotum](#teetotum-the-die), [Nearby](#nearby)
4. [Plugin settings](#plugin-settings)
5. [Rights](#rights)
6. [When something goes wrong](#when-something-goes-wrong)
7. [Memory and limits](#memory-and-limits)
8. [Installing other plugins](#installing-other-plugins)

---

## What a plugin is

A plugin is a small program that gives the Knob a new **face**: a screen of its own, with its
own idea of what the knob, a tap and a swipe should do. The firmware calls a plugin's screen its
*face*, and so does this guide.

A few things hold for every plugin, and they are what makes it safe to try one:

- **It runs in a sandbox.** A plugin is a WebAssembly module, run by an interpreter inside the
  firmware. It gets one small block of memory (64 KiB) and cannot see or change anything
  outside it: not the firmware, not your settings, not another plugin.
- **The firmware draws, the plugin only asks.** A plugin never touches the screen. It hands the
  firmware a list — "text here, an arc there, this icon" — and the firmware checks each item and
  draws it.
- **It can only do what its rights allow.** Sending keys to your phone, taking over the knob,
  reading what the radio hears: each needs a right that the plugin declares up front, and that
  you can read in its settings. See [Rights](#rights).
- **The long press always belongs to the firmware.** Hold a finger still on the screen for
  0.6 seconds and you are back at Home, whatever the plugin is doing. The firmware sees the long
  press before any plugin does, so no plugin can take it away, and it writes **"hold for home"**
  at the foot of every plugin face itself, where no plugin can cover it. A plugin cannot trap
  you.

---

## Where plugins live

After the Knob starts, it shows **Home**: a ring of twelve segments. Home sits at the top, the
firmware's own entries on the left, and the plugins' faces clockwise from Home, starting at one
o'clock. With the three bundled plugins, Home looks like this:

```
                         12  Home
            11  Settings            1  HID remote
       10  Music Player                  2  Teetotum
       9   (free)                          3  Nearby
       8   (free)                        4  (free)
            7  (free)               5  (free)
                          6  (free)

   Segments 1 to 9 are the places for plugin faces, three in use. Beyond nine,
   Home runs on to a second page; the Settings ring holds five on its first,
   see "Memory and limits".
```

**Starting a plugin.** Turn the knob or tap a segment to select a plugin; its name appears in
the middle of the ring, and under it the plugin's one-line summary (for example "remote for the
phone's player"). Tap the selected segment a second time, or tap the middle, to open its face.
Home itself says "tap menu entry to choose" or "tap menu entry to open", depending on what is
selected.

**Leaving a plugin.** Hold a finger on the screen. You are back at Home.

**One plugin runs at a time.** A plugin is loaded into memory when you open its face, and not
before. Opening a different plugin first unloads the one that ran before.

**The last one stays loaded.** Until you open another plugin, or remove this one, it stays in
memory. Go to Home, the settings or the Music Player and come back: the plugin is exactly as you
left it (the die still shows its last throw, Nearby still has the same device chosen). Opening
a *different* plugin in between means the first one starts afresh next time.

**Home has no gaps.** The installed plugins stand side by side from one o'clock on, the bundled
ones first. When you remove a plugin (see [Plugin settings](#plugin-settings)), the Knob
restarts and the plugins after it move up one segment.

---

## The bundled plugins

The firmware ships with three plugins. All three are installed out of the box, and you can
remove any of them.

| Plugin     | Summary on Home                  | Size        | Rights                  |
|------------|----------------------------------|-------------|-------------------------|
| HID remote | "remote for the phone's player"  | 1381 bytes  | `hid knob`              |
| Teetotum   | "a die of 2 to 256 sides"        | 2737 bytes  | `knob random`           |
| Nearby     | "Wi-Fi and Bluetooth around you" | 7266 bytes  | `knob radio haptic`     |

Sizes are those of the modules in the firmware as of 12 September 2026.

### HID remote

A remote control for the media player **on your phone**. It works when the music plays through
the phone's own speaker or headphones, not through the Knob. The Knob acts as a Bluetooth
keyboard with media keys (the Bluetooth profile is called HID, "human interface device").

```
   .----------------------------------.
   |        (rim, no volume on it)    |
   |               |> ||              |      the icon
   |            Play/Pause            |      the last key sent
   |       tap to play or pause       |
   |           swipe to skip          |
   |           turn to seek           |
   |        pair TAIJI_KNOB_HID       |      or "phone connected"
   |           hold for home          |      written by the firmware
   '----------------------------------'
```

| You do           | The phone gets     |
|------------------|--------------------|
| Tap the screen    | Play/Pause         |
| Swipe left       | Previous track     |
| Swipe right      | Next track         |
| Turn the knob    | Fast forward (clockwise) or rewind (anticlockwise), one press per detent |

Left is back in time: a swipe to the left goes to the previous title, as on a timeline.

The large line in the middle shows the last key the plugin sent: "Play/Pause", "Next",
"Previous", "Fast forward" or "Rewind". Before the first one it says "HID remote". **Nothing comes back from the phone**:
over this link the Knob cannot read the title, the artist or the volume, so the face only shows
what it sent.

The bottom line tells you whether a phone is listening:

- **"pair TAIJI_KNOB_HID"** means no phone is connected as a remote. Pair your phone with the
  Bluetooth device named `TAIJI_KNOB_HID` in the phone's Bluetooth settings.
- **"phone connected"** (in green) means a phone is connected and will receive the keys.

**Why the name is not "TeeToTum".** The Knob has two chips. The Bluetooth remote belongs to the
second chip, which still runs its factory firmware and advertises itself as `TAIJI_KNOB_HID`.
The plugin asks that chip to send each key.

**The knob seeks, and it costs you the volume.** While this face is shown the knob belongs to
the plugin: each detent sends fast forward or rewind, and turning it no longer changes the
volume of whatever plays *through the Knob*. Leave the face to get the volume back. How far one
detent jumps is your player's decision, not the Knob's. Volume is the one thing the remote
cannot send either way: the second chip's list of media keys contains no volume.

**Pairing pitfall: "unpaired is not forgotten".** If your phone already knows the Knob as an
**audio device** (`TAIJI_KNOB_AUDIO`), it may merge the remote and the audio device into one
entry, without the switch that makes it an input device, and then no key arrives. In our tests
unpairing did not fix this; **restarting the phone** did, after which `TAIJI_KNOB_HID` appeared
under its own name and could be paired. Using the remote and playing audio through the Knob at
the same time worked on one day and not on another with the same phone; if the remote does not
respond, try pairing it on its own.

**Known quirks** (seen on 10 September 2026, not yet explained):

- Very rarely, a Next is ignored. Every swipe reached the plugin, so either the phone dropped
  the key or the swipe was not registered as a contact at all.
- Whether a quick double tap toggles twice has not been checked.

The touch controller does not name every swipe as one. The firmware treats a contact that moved
at least 36 pixels as a swipe in that direction, so a swipe is not mistaken for a tap.

### Teetotum, the die

A teetotum is a spinning top with numbered sides, turned between thumb and forefinger. The
project is named after it, and this plugin makes the knob its stem: a die with 2 to 256 sides.

```
   .----------------------------------.
   |   ring: one segment per side,    |
   |   the thrown side highlighted    |
   |             6 sides              |
   |                4                 |      the side it came to rest on
   |       turn the knob or tap       |
   |          swipe for sides         |
   |           true random            |      or "pseudo-random only"
   |           hold for home          |
   '----------------------------------'
```

| You do                         | The die                                        |
|--------------------------------|------------------------------------------------|
| Turn the knob (either way)     | throws once **per detent**; where your hand stops, the side stays |
| Tap the screen                  | throws once                                    |
| Swipe right                    | more sides                                     |
| Swipe left                     | fewer sides                                    |

**Sides.** A swipe walks through 2, 4, 6, 8, 10, 12, 16, 20, 32, 42, 64, 100, 128 and 256 sides,
and wraps around at either end (from 256 a swipe right goes back to 2). The die starts with
**6 sides** each time the plugin is loaded. Changing the number of sides clears the last throw,
and the face shows the teetotum icon until you throw again.

**The ring.** Up to 20 sides, the ring is divided into one segment per side, starting at twelve
o'clock and running clockwise, and the side thrown is highlighted. Above 20 sides the ring is
plain, and a short mark shows where the thrown side lies.

**Fair throws.** Every side is equally likely. The plugin draws a random number and throws away
the rare draws that would favour the low sides, instead of taking a remainder.

**True random or pseudo-random.** The bottom line says where the randomness came from:

- **"true random"** (green): the numbers came from the chip's hardware generator while it was
  mixing in physical noise. The chip does that while its radio is running, and the firmware
  keeps the radio (Wi-Fi and Bluetooth) running all the time, so this is what you normally see.
- **"pseudo-random only"**: no source of physical noise was running, so the numbers are only
  pseudo-random. You would see this if the radio did not start.

The plugin shows which it got because a die that promises chance should not quietly hand out a
predictable sequence instead.

### Nearby

Shows the Wi-Fi networks or the Bluetooth devices around the Knob as dots, and helps you find
one of them — the headphones you put down somewhere, say — by pulsing the motor faster as its
signal gets stronger, like a game of hot and cold.

```
   .----------------------------------.
   |    .     rings at -50, -70 and   |
   |  .    -90 dBm; dots in between   |
   |       Wi-Fi, 7 networks          |      which radio, how many
   |   .      Example-Net         .   |      the chosen one's name
   |       -58 dBm, channel 6         |      its strength (green)
   |   .       tap to find            |
   |           hold for home          |
   '----------------------------------'
```

**Reading the dots.**

- **One dot per network or device. The stronger its signal, the nearer the middle.** A signal
  of -35 dBm or stronger stands right at the inner edge, -95 dBm or weaker at the rim.
- **The angle means nothing.** A single antenna cannot tell where a signal comes from. A dot's
  position around the circle only keeps it in the same place from one round to the next. The
  bottom of the circle stays free for "hold for home".
- Dots of networks or devices that give a name are drawn in the theme's icon colour, nameless
  ones dimmer. The chosen one is a larger dot in the highlight colour.
- For each radio the plugin keeps the 24 strongest signals of the last round.

**Choosing.**

| You do            | Nearby                                                        |
|-------------------|---------------------------------------------------------------|
| Swipe (either way)| switches between Wi-Fi and Bluetooth (and stops finding)      |
| Turn the knob     | chooses one: the first detent picks the strongest; clockwise goes to weaker ones, anticlockwise to stronger ones, and the list wraps around |
| Tap               | starts finding the chosen one; tap again to stop              |

What the middle shows:

- Before the first round comes in: the radio's name and "listening".
- Then the radio and the count, for example "Wi-Fi, 7 networks" or "Bluetooth, 1 device", and
  "turn to choose" with "swipe for Bluetooth" or "swipe for Wi-Fi".
- Once something is chosen: its name, cut short with ".." if it is long; "hidden network" for a
  Wi-Fi network without a name, "no name" for a nameless Bluetooth device. Under it the
  strength, and for Wi-Fi the channel, for example "-58 dBm, channel 6", then "tap to find".
- "not heard this round" if the latest round missed the chosen one. The choice is kept, so it
  comes back when the signal does.

**Finding.** After a tap the face changes: "finding" at the top, the chosen name, its strength
in large digits, and a bar round the rim that grows with the signal. **The motor pulses**, once
every 1.5 seconds at -95 dBm and faster as the signal gets stronger, up to every 0.2 seconds at
-35 dBm. In a round that does not hear the chosen one, the strength shows "--" with "not heard
this round", and the motor stays silent. Carry the Knob about and follow the pulse. "tap to
stop" ends it; a swipe also ends it. While finding, the knob does nothing.

Some details of the pulse:

- The firmware keeps the rhythm and uses the same click as a tap, at the strength set under
  **Haptics** in the settings. **With Haptics set to Off, the pulse is silent too.**
- The pulse only runs while the face is on the screen. Opening Home or the settings pauses it;
  coming back to the face resumes it.

**Faster rounds while you look.** Normally the Knob listens for Bluetooth devices for five
seconds every 30 seconds. While the Nearby face is on the screen, the radio works flat out: a new Wi-Fi scan
starts one second after the last one ends, and Bluetooth listens in back-to-back windows of two
seconds. Each round brings the face up to date.

**What the plugin never sees: addresses.** Nearby receives names, signal strengths, Wi-Fi
channels and a key for each entry, never the hardware address of a network or device. The
firmware makes the key from the address and a random number it draws at every start and keeps
to itself. So the plugin can follow one device while the Knob runs, but cannot look the key up
in a list of addresses, and after a restart every device has a new key. A Bluetooth device that
changes its own address, as phones do, turns up again under a new key. No right gives a plugin
the addresses.

**The law where you use it.** Receiving radio signals and showing what is heard is regulated
differently from country to country, by telecommunications and data protection law among
others. **Observe the rules that apply where you use this plugin.** This note is not legal
advice. (The same note is in the plugin's own [README](../plugins/nearby/README.md).)

---

## Plugin settings

Every plugin has a small menu of its own in the firmware's settings. To get there, open
**Settings** (the gear at eleven o'clock on Home). The plugins' menus follow the firmware's own
settings clockwise:

```
                         12  About
            11  Music Player            1  Orientation
       10  (empty)                           2  Theme
       9   (empty)                             3  Brightness
       8   Nearby                            4  Haptics
            7  Teetotum             5  Background
                          6  HID remote
```

Opening a plugin's menu does **not** start the plugin; plugins are started only from Home. A
plugin's menu has two entries, plus the way out:

```
                         12  About
            11  Main Settings           1  Installed
```

- **About** tells you about the plugin (see below).
- **Installed** puts the plugin's face on Home, or takes it off.
- **Main Settings**, the gear left of About, leads into the firmware's settings. In a plugin's
  menu it is called "Main Settings" rather than "Settings", because "Settings" would name the
  menu you are already in.

The tick (OK) goes back one level. A long press goes straight to Home; in an open dialog it
first undoes what you changed there, as the cross (Cancel) would.

### About

The About dialog is filled from the plugin's manifest, a short description inside the plugin
file that the firmware can read **without running any of the plugin's code**. For Teetotum it
looks like this:

```
          Teetotum
   bundled with the firmware
         2737 bytes
     rights knob random
      loaded in 45.3 ms
        14.1 KB heap
```

| Line                          | Meaning                                                   |
|-------------------------------|-----------------------------------------------------------|
| Name                          | The plugin's name, as on Home.                            |
| "bundled with the firmware"   | It is built into the firmware.                            |
| "from slot N"                 | It was installed into slot N, see [Installing other plugins](#installing-other-plugins). |
| "… bytes"                     | The size of the plugin file.                              |
| "rights …"                    | What it is allowed to do beyond drawing and hearing taps and swipes, or "rights none". See [Rights](#rights). |
| "loaded in … ms"              | How long loading took, while it is in memory.             |
| "… KB heap"                   | How much of the firmware's working memory it holds, while it is in memory (1 KB = 1000 bytes here). |

The last two lines change with the plugin's state:

- **"not loaded"** (and an empty last line): the plugin is not in memory right now, because it
  has not been opened since the Knob started, or another plugin has been opened since.
- **"stopped"**, with the reason on the last line: the plugin was stopped or refused. See
  [When something goes wrong](#when-something-goes-wrong).

### Installed

The dialog shows **"Yes"** or **"No"**, with "turn the knob" and "OK restarts" under it. Any
detent of the knob flips between the two. Nothing happens until you confirm with the tick; the
cross leaves everything as it was.

A tick that changes something saves your choice and **restarts the Knob**, because Home is laid
out when it starts:

- **No** takes the plugin's face off Home, and the plugins after it move up one segment.
- **Yes** puts the face back on Home, in its place in the row. It loads nothing: the plugin is
  loaded the next time you open its face.

Your choice survives the restart and every one after it.

**Removing frees memory, not storage.** A bundled plugin is part of the firmware file in the
Knob's flash memory, a plugin from a slot stays in its slot, and removing either only takes it off
Home. That is also why a removed plugin keeps its menu in the settings: you can always set
Installed back to Yes. How to empty a slot for good is under
[Installing other plugins](#installing-other-plugins).

---

## Rights

A plugin that does nothing but draw its face and react to taps and swipes needs no right at all.
Anything beyond that needs a right, declared in the plugin's manifest and shown under About.

| Right    | What it lets the plugin do | Why it matters to you |
|----------|----------------------------|-----------------------|
| `hid`    | Send media keys to your phone through the second chip's Bluetooth remote (`TAIJI_KNOB_HID`): Play/Pause, Play, Pause, Stop, Next, Previous, Fast Forward, Rewind, Record, Power. At most four keys per tap or swipe. It is also told whether a phone is connected. | It can control your phone's media player. |
| `knob`   | Receive the knob's turns while its face is shown. | Without it, the knob keeps changing the volume of what plays through the Knob. With it, the knob belongs to the plugin, and the volume does not move. |
| `random` | Get random numbers from the chip's hardware generator, and learn whether they came from physical noise. At most 64 bytes per tap, swipe or detent. | Harmless; it is what makes a fair die possible. |
| `radio`  | Read what the radio hears nearby: names, signal strengths and Wi-Fi channels of networks and Bluetooth devices, and a key for each. **Never their addresses** — no right gives those. | It shows what is around you. While such a face is on the screen, the radio also scans much more often. |
| `haptic` | Keep the motor pulsing at a steady rhythm, between once every 0.15 and once every 5 seconds, only while its face is shown, and at the strength you chose under Haptics. | You feel it. Haptics set to Off silences it. |

These are all the rights the firmware knows today.

**Rights are checked before any plugin code runs.** When you open a face, the firmware first
reads the manifest, then compares what the plugin file asks the firmware for with the rights in
that manifest. A plugin that uses something without having declared the right for it is
**refused** and never starts. So what About shows is everything the plugin can do.

The same holds the other way round: a plugin only gets the knob's turns if it has `knob`, only
rounds of the radio if it has `radio`, and only word of a connected phone if it has `hid`.

**A plugin that asks for a right this firmware does not know is refused, not trimmed.** It was
written for a newer firmware, and running it with less than it asked for would make a plugin
that half works without saying why.

**What no plugin can do**, whatever its rights: read or change the firmware's memory, your
settings or another plugin; draw anywhere except through the firmware's checked list; see the
long press; or keep the Knob busy. Every call into a plugin runs on a small budget (about 7 ms of
work), and a plugin that runs past it is stopped. A picture may have at most 64 items, a line of
text at most 128 bytes.

---

## When something goes wrong

On the screen, both kinds of failure look the same: the plugin's name, the word **"stopped"**,
and a short reason under it, with "hold for home" at the foot.

```
   .----------------------------------.
   |                                  |
   |            HID remote            |
   |              stopped             |
   |        <the reason, small>       |
   |                                  |
   |           hold for home          |
   '----------------------------------'
```

On Home, the line under the plugin's name says **"stopped"** instead of its summary, and its
About shows "stopped" with the same reason.

### A plugin that stops while running

If a running plugin breaks one of the sandbox's rules (runs past its time budget, draws more
items than allowed, asks for more random bytes than allowed, and so on), the firmware stops it
on the spot. A stopped plugin is not called again: taps and turns do nothing on its face, and a
pulse it had asked for stops.

**The way back:** hold for Home, then open its face again. A stopped plugin is always **loaded
anew**, so it starts from scratch; whatever it showed before is gone. Opening another plugin, or
setting Installed to No, also clears the "stopped".

### A plugin that is refused

When you open a face, the firmware checks the plugin before running it. If the check fails, the
plugin is refused and its face says why. **A refused plugin stays on Home** (it is not taken off),
precisely so that its face can tell you the reason. Opening it again tries again.

The reasons the firmware can give (a developer will recognise them; a bundled plugin should
never show one):

| On the screen                                              | Meaning |
|-----------------------------------------------------------|---------|
| "does not import its memory"                              | The plugin was built without the setting that puts its memory where the firmware lends it. |
| "asks for N pages of memory, a face gets 1"               | It wants more than the one 64 KiB block every plugin gets. |
| "imports NAME, which the firmware does not offer"         | It asks for a function this firmware does not have. |
| "imports NAME without the right RIGHT in its manifest"    | It uses a function it has not declared the right for, for example "imports send_usage without the right hid in its manifest". |
| "signature does not match its bytes and key"              | The file was changed after its author signed it. |
| "no memory to run in"                                     | The memory block for plugins was not available. |

The interpreter can also refuse a file that is not valid WebAssembly; the reason is then its own
message.

A plugin whose manifest cannot be read at all (for example one written for a newer manifest
format or a newer firmware), or that is not signed, does not appear anywhere, neither on Home nor
in the settings.

---

## Memory and limits

In plain words: a plugin needs two kinds of memory.

- **Its own block of 64 KiB** in the Knob's external memory. There is exactly one such block, and
  it is lent to one plugin at a time. That is why only one plugin runs at once.
- **Some of the firmware's working memory** (the *heap*), where the interpreter keeps the
  plugin in a form it can run quickly. This is the scarce one.

Measured on 11 September 2026, with the radio running:

| Plugin     | Heap while loaded | Loading takes |
|------------|-------------------|---------------|
| HID remote | about 9.8 KB      | about 20 ms   |
| Teetotum   | about 14.1 KB     | about 45 ms   |
| Nearby     | about 21.3 KB     | about 85 ms   |

About 57 KB of heap are free before any plugin is loaded; with Nearby loaded, about 35 KB remain.
Unloading a plugin gives its memory back. Loading includes clearing the 64 KiB block, so that no
plugin sees what the one before left in it; that takes about 4 ms.

The HID remote grew from 1137 to 1248 bytes on 12 September 2026, when a detent became fast
forward and rewind; the figures above are the ones measured before that.

What this means for you:

- **The heap limits the largest plugin, not the number of plugins**, because only the one you
  are using is loaded.
- **The number of plugins is limited by places, and the places run on to a second page.** Home
  has nine places for plugin faces on its first page (one to nine o'clock), the Settings ring
  five (six to ten o'clock). A sixth plugin puts the Settings on two pages, a tenth Home as well;
  a row of dots under the top segment says which page you are on, and the knob turns from the last entry
  of one page to the first of the next; on a second page the plugins fill the ring from one
  o'clock round to eleven, because nothing else stands there. See "How the menus work" in the
  [user guide](user-guide.md#how-the-menus-work). **Sixteen plugins is the ceiling** the
  firmware refuses to build past -- that is what the settings record can keep apart -- and
  sixteen fit on two pages. With the three bundled plugins neither ring pages at all.

**A plugin that is too large is refused, not loaded.** The interpreter cannot report a lack of
memory -- an allocation it cannot make takes the firmware down with it -- so the firmware works
out beforehand what a plugin of that size will need and compares it with what is free. A plugin
that does not fit keeps its place on Home, and its face says why:
`needs about 40360 bytes of heap, 36496 free`. The estimate is deliberately generous, so a
plugin can be turned away that would just have fitted; the three bundled plugins fit with room
to spare, and so does anything up to roughly 13 KB of module.

---

## Installing other plugins

Besides the plugins that come with the firmware, the Knob keeps up to sixteen more in a part of
its flash memory set aside for them, the `plugins` partition. It is cut into sixteen **slots**,
one plugin each. A plugin gets into a slot **over the USB cable, from a computer**, and the Knob
asks on its screen before the plugin gets a place on Home. There is no way yet to install a
plugin from the SD card, over Wi-Fi or over Bluetooth.

What you need:

- The plugin's `.wasm` file, signed by its author. How to build and sign one is described in
  [plugin development](plugin-development.md).
- A copy of this repository for `tools/teetotum-pack`, which needs the `stable` Rust toolchain, and
  `espflash`, as in
  [Building and flashing it yourself](user-guide.md#9-building-and-flashing-it-yourself).
- A Knob running TeeToTum with the partition table from `partitions.csv`. `cargo run --release`
  writes it; a Knob flashed with espflash's own table has no `plugins` partition.

### Writing a plugin into a slot

Connect the board so that the ESP32-S3 is on USB, then, from the repository root:

```
tools/teetotum-pack pack my-face.wasm --slot 0 --write
```

The tool checks the plugin's manifest and signature, writes `my-face.slot` next to the file (a short header, then the
plugin) and hands it to `espflash write-bin` at the slot's address. Without `--write` it only
writes the file and prints the espflash command. The Knob restarts when espflash is done.

Writing a slot that already holds a plugin replaces it. **Pick a free slot for a new plugin, and
the same slot for a new version of one.** If two slots hold the same plugin, the Knob uses the
lower one and skips the other.

### The install dialog

At the next start, before Home, the Knob opens a dialog for every plugin that has been written
but not yet accepted. The dialog is filled from the plugin's manifest and **runs none of the
plugin's code**:

```
             <name>
   project key  or  unknown key
       key <16 hex digits>
         rights <rights>
     v<version>  <size> bytes
    <N> KB heap of <N> free
```

| Line | Meaning |
|---|---|
| Name | The plugin's name, as it will stand on Home. |
| "project key" | It is signed with the same key as the bundled plugins. |
| "unknown key", in orange | It is signed with any other key. The Knob cannot know whose key that is: compare the next line with the key the author publishes. |
| "key …" | The first eight bytes of the author's key. Plugins with the same key come from the same author. |
| "rights …" | What it will be allowed to do. See [Rights](#rights). |
| "v…  … bytes" | Its version and the size of its file. |
| "… KB heap of … free" | How much working memory it will need once you open it, estimated as in [Memory and limits](#memory-and-limits), and how much is free. **"too large: … KB heap"** means it would be refused when opened; you can install it anyway. |

- **The tick** installs it. If more plugins wait, the next dialog follows. After the last one the
  Knob restarts, and the new plugins stand on Home after the bundled ones, each with its own menu
  in the settings.
- **The cross, or a long press,** installs nothing. The plugin stays in its slot, and **the next
  start asks again**. To stop the question, empty the slot.

### After installing

- **Its About says "from slot N"** where a bundled plugin says "bundled with the firmware".
  Everything else in its settings works as for any plugin, Installed included.
- **A plugin with the same key and name as a bundled one** (a newer build of it) takes that
  plugin's place on Home rather than a new one.
- **Flashing a new firmware keeps the slots.** `cargo run --release` writes the firmware and the
  partition table, not the `plugins` partition. Restoring the factory firmware overwrites them.
- **Removing a plugin** (Installed: No) takes it off Home and leaves it in its slot. **Emptying
  the slot** deletes it for good:

  ```
  espflash erase-region -B 921600 0x810000 0x10000
  ```

  That is slot 0. Slot N starts at `0x810000` plus N times `0x10000`; `tools/teetotum-pack pack`
  prints the address of the slot it writes. At the next start the plugin is gone from Home and
  from the settings.

**The limits are the same for every plugin.** A slot holds a file of up to 63 936 bytes, but the
heap limits what can be loaded to roughly 13 KB of module, and the rings hold sixteen plugins
altogether, the bundled ones included. See [Memory and limits](#memory-and-limits).

Plugins can also be built into the firmware, the way the bundled ones are.
[Plugin development](plugin-development.md#4-install-it-on-the-device) describes both ways.
