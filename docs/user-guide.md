# TeeToTum user guide

This guide is for people who have TeeToTum running on a **Waveshare ESP32-S3 Knob Touch LCD
1.8**: the round 360 x 360 touch screen with a rotary knob around it. This guide calls the
device **the Knob**, with a capital K, and the ring you turn **the knob**. Sections 1 to 9 need no
programming knowledge. [Section 10](#10-building-and-flashing-it-yourself) is for people who build
and flash the firmware themselves.

Related documents:

- [Using plugins](plugins.md): the bundled plugins in detail, starting and removing them, rights.
- [Writing plugins](plugin-development.md): for plugin authors.
- [The hardware, as measured](hardware/README.md): which chip does what, and what the board answered.
- [Factory firmware backups](../backup/README.md): how to go back to the factory demo.

Contents:

1. [What TeeToTum is](#1-what-teetotum-is)
2. [First start](#2-first-start)
3. [The controls](#3-the-controls)
4. [Home](#4-home)
5. [Music Player](#5-music-player)
6. [Card over Wi-Fi](#6-card-over-wi-fi)
7. [Settings](#7-settings)
8. [Plugins](#8-plugins)
9. [Troubleshooting](#9-troubleshooting)
10. [Building and flashing it yourself](#10-building-and-flashing-it-yourself)
11. [Glossary](#11-glossary)

---

## 1. What TeeToTum is

TeeToTum is an open-source firmware that replaces the factory demo on the Knob. A
teetotum (the toy, written without the capitals in the middle) is a spinning top you turn between
thumb and forefinger, which is how you use the device: you turn the knob and touch the screen.

The board carries **two microcontrollers**, and this explains most of what TeeToTum can and cannot
do:

- The **ESP32-S3** runs TeeToTum. It owns the touch screen, the knob, the vibration
  motor, Wi-Fi and Bluetooth Low Energy.
- The second chip, a **classic ESP32**, still runs its factory firmware. It owns Bluetooth audio,
  the sound output and a Bluetooth remote-control function. TeeToTum talks to it over a wire inside
  the board and asks it to do things: play, pause, skip, change the volume.

**What works:**

- Menus drawn as a ring of twelve segments around the round screen, worked with the knob and the
  touch screen.
- Clicks you feel under your finger from the vibration motor, in adjustable strength or off.
- Music from your phone through the second chip: title, artist, cover picture and volume on the
  screen, play/pause with a tap, previous and next track with a swipe, volume with the knob.
- Four picture orientations, eleven colour themes, ten brightness steps.
- A background of points in the colours of the theme behind Home, the menus and the Music Player,
  still or moving.
- The TF card read from a phone or computer over Wi-Fi, read-only, while its dialog is open.
- Plugins: small add-on programs that each get their own screen. Three come with the firmware.
- Settings that survive a restart.

**What not to expect:**

- **No internet.** The TeeToTum firmware scans for Wi-Fi networks nearby, and runs a network of
  its own for [Card over Wi-Fi](#6-card-over-wi-fi), but never joins one.
  Plugins cannot do so either today, since none of their rights allows it; if a later plugin can,
  its own documentation says so.
- **No sound of its own.** What you hear is your phone's music, played by the second chip.
- **No battery gauge.** If your Knob has a battery, TeeToTum does not show how full it is.
- **The factory demo is gone** from the ESP32-S3: its clock faces and animations are not shown.
  Its pictures are still on the microSD card, but TeeToTum does not use them. How to go back to the
  demo is in
  [section 10](#restoring-the-factory-firmware).
- **No installing plugins from the Knob alone.** Plugins beyond the bundled ones come from a
  browser over Bluetooth or from a computer over the USB cable, not from the SD card or over
  Wi-Fi. See
  [Using plugins](plugins.md#installing-other-plugins).
- **The knob cannot be pressed.** It only turns. Everything that works like a button is done on
  the touch screen.

## 2. First start

1. The Knob comes with or without a battery. **Without one**, it needs a USB-C cable to a USB
   charger or a computer. **With one**, it also runs without a cable, and the cable charges it.
2. Switch it on with the power switch.
3. **The screen stays dark for a moment.** TeeToTum shows no boot screen: the backlight comes on
   with the first picture, and that picture is Home.
4. **On the very first start of a board, the motor buzzes once.** It is calibrating itself. The
   result is stored, and later starts are quiet.

After that you are at **Home**, the ring from which everything else is opened (see
[section 4](#4-home)). The device starts at Home every time. It does not remember which screen was
open when it was switched off.

To switch it off, use the power switch; it switches the Knob off even with the cable plugged in.
TeeToTum has no shutdown step, and needs none: settings are already stored at the moment you
confirm them.

## 3. The controls

### The gestures

| You do | Where | What happens |
|---|---|---|
| **Turn the knob** | in a menu | The selection moves one entry per detent, skipping empty segments; with few entries, a small turn already goes round the ring. |
| | in an open setting | The value changes, one step per detent. |
| | on the Music Player | The volume changes. |
| | on a plugin's screen | The plugin gets the turn if it uses the knob; otherwise the knob keeps working the volume. |
| | on a QR code | Shows the next code. |
| **Tap** a segment of the ring | in a menu | The first tap selects the entry, a second tap on it opens it. |
| **Tap** the middle | in a menu | Opens the selected entry. |
| **Tap** | on the Music Player | Play or pause. |
| **Tap** | on a QR code | Closes it. |
| **Swipe** left or right | on the Music Player | Previous or next track. |
| **Swipe** in any direction | on a plugin's screen | Goes to the plugin. |
| **Long press** | anywhere but Home | Goes to Home. |
| | on Home | Opens the [QR codes](#qr-codes). |

**Long press:** rest a finger on the screen for about 0.6 seconds without moving it (a drift of
about 2 mm is fine). You feel a click while the finger is still down, and Home appears; lifting the
finger afterwards does nothing more. This works on every screen, in every menu and inside every
plugin, and no plugin can switch it off. **In an open setting a long press cancels first**: the
value goes back to what it was when you opened the setting, then Home appears. **On Home itself a
long press opens the [QR codes](#qr-codes)**, and the next one leads back to Home.

**Swipes** count in the directions of the picture as you see it, whichever way you have turned the
picture (see [Orientation](#orientation)). A swipe has to travel at least about a tenth of the
screen; a shorter movement counts as a tap. Up and down swipes do nothing on the Music Player, and no swipe does anything in the menus.

### How the menus work

Every menu is a ring of twelve segments around the edge of the screen, like the hours on a clock
face. Occupied segments carry an icon and stand slightly raised, like keys; empty segments are flat
and cannot be selected. The gaps between the segments are black. Inside the ring, every menu
stands on a cloud of points in the colours of the theme, darker towards the middle, where names
and values are read; how it looks is set under [Background](#background).

```
                      12   <- About (at Home: Home)
               11              1
          10                        2        the ring: twelve segments,
                    Settings                 icons only
         9          Orientation        3     small:  the menu's title
                    0 deg                    large:  the selected entry's name
          8   tap menu entry to open    4    large:  its current value
                                             small:  the hint
               7        [ v ]          5     button: tick (OK), where there is one
                       6
```

- **Names are not written in the ring.** The ring shows icons only. The selected entry's name
  stands large in the middle, with the menu's title small above it and the entry's current value
  below it. Under that stands `tap menu entry to open`, or `tap menu entry to choose` when Home
  itself is selected.
- **The top segment marks "up".** In every menu, About sits at twelve o'clock; at Home, the Home
  entry does. When you turn the picture, this segment turns with it.
- **Tap once to select, tap again to open.** A finger on or just past the rim counts as the ring,
  since on a round screen that is where a finger aiming at the ring lands. Tapping the middle opens
  whatever is selected.
- **Every detent is one entry.** Turning clockwise selects the next entry round the ring, and
  empty segments are skipped. The knob clicks **whenever the selection moves to another entry**,
  which is at every detent unless the menu has only one entry.

**When a ring is full: pages.** A ring has twelve segments and some of them are taken by the
firmware -- the top one, the Music Player and Card over Wi-Fi, at Home the gear and in the
Settings the firmware's own settings. What is left over is where plugins go: eight segments at
Home, four in the Settings, where `Receive` takes the one after the last plugin. A menu with more entries than that
**runs on to a second page**, where every segment but the top one is free and the plugins fill
them from one o'clock round to eleven, and a **row of dots** appears just inside the ring under the top segment, one dot per
page, the page you are on lit:

```
                          12   <- About (at Home: Home)
                   11    . . .     1          three pages, the first one lit
              10                        2
```

- **You will not normally see the dots.** They appear only when a menu has more than one page,
  and with the plugins TeeToTum ships neither ring is anywhere near full. They are described
  here because the rule is the menu's, not the plugins'.
- **The knob goes on across the pages.** Past the last entry of a page it moves to the first
  entry of the next, and past the last page it comes back to the first. The ring never ends, so
  two pages mean two turns round the menu, three pages three.
- **Only the top segment repeats.** About, or Home at Home, stands on every page: it is what the
  pages turn under and what the dots stand beneath, so the top segment still marks "up" wherever
  you are. **Everything else has one place in the whole menu** -- the gear, the Music Player, Card
  over Wi-Fi and the firmware's own settings are all on the first page, and a later page carries nothing but the
  entries that did not fit before it. A long press leads to Home from any of them.
- **Taps work on the page in front of you.** There is no gesture that turns a page and none is
  needed: the knob walks through all of them, and a long press still goes to Home, which is the
  first entry of the first page.
- **How many pages there can be.** The menus allow five. TeeToTum keeps at most sixteen plugins
  apart, though, and sixteen fit on two pages, so two is the most there can be today.

**Buttons** are icons: a **tick** means OK, a **cross** means Cancel.

- **In a menu one level below Home** (Settings, for example) there is no tick. Instead the menu
  says `hold for home`, because a long press is the way back there.
- **In a deeper menu** a tick stands below the name. It goes back up one level.
- **In a setting you can change**, the cross stands on the left and the tick on the right. While a
  setting is open, the knob changes its value and you see the result at once. The tick keeps the
  new value and stores it; the cross puts back the value from when the setting was opened. Taps on
  the ring do nothing while a setting is open.
- **In a setting that only shows something** (every About), there is only the tick.

**Clicks.** The motor clicks under a tap that does something, under a long press, under every
detent while a setting is open (except at the ends of Brightness, Haptics and the numbers under
Background, where the value cannot move any further), under every detent on the Music Player or a plugin's screen, under a tap or
swipe that reaches a plugin, and in the ring whenever the selection moves. With Haptics set to `Off` it does not click at all.

### "hold for home"

The Music Player and every plugin screen show `hold for home` at the bottom of the screen, in the
gap of the volume arc. TeeToTum writes this itself on top of every screen, so no plugin can cover
it. It always means the same thing: a long press takes you to Home.

## 4. Home

Home is where the device starts and where a long press leads from every other screen. From here you open the
Music Player, Card over Wi-Fi, the Settings and the screens of the plugins. The menu's title is `TeeToTum`.

```
                          Home
                           12
          Settings  11           1  HID remote
 Card over Wi-Fi  10                 2  Teetotum
   Music Player  9                     3  Nearby
                  8                  4
                     7            5
                           6
```

- **Left of Home is what belongs to the firmware**: the gear (`Settings`) at eleven o'clock,
  `Card over Wi-Fi` at ten and the `Music Player` at nine.
- **Right of Home are the plugins**, one per segment from one o'clock on. The three bundled ones
  are `HID remote`, `Teetotum` (a die -- the firmware itself is `TeeToTum`) and
  `Nearby`. They are described in [Using plugins](plugins.md).
- **A plugin that has been removed leaves no gap.** The plugins after it move up a segment.
  Removing and reinstalling is done in the plugin's settings (see
  [Plugin entries](#plugin-entries)).
- **From the ninth plugin on, Home has a second page**, with the dots under the Home segment; see
  [How the menus work](#how-the-menus-work). The ninth plugin then stands at one o'clock of the
  second page, where the first stands on the first.
- Home has **no OK button**: there is nothing above it. You leave it by opening one of its entries.
- Where other menus have OK, Home says `hold for QR codes`: a long press there opens the
  [QR codes](#qr-codes).

**The line under the name** shows the state of the selected entry:

| Selected | The line shows | Example |
|---|---|---|
| Home | where the firmware's source lives, on two lines | `look at github.com:` / `teetotum-rs/firmware` |
| Settings | theme and brightness; the picture's angle if it is not 0; `silent` if clicks are off; `moving` if the [background](#background) moves | `Red · 100 %`, `Grey · 40 % · 90 deg · silent`, `Red · 100 % · moving` |
| Music Player | the title that is playing, or `nothing playing` | `nothing playing` |
| Card over Wi-Fi | the size of the card found at start-up, or `no card` | `14.8 GB card` |
| a plugin | the plugin's own one-line summary, or `stopped` after it failed | `remote for the phone's player` |

Lines too long for the space are shortened with `...`.

### QR codes

A long press on Home opens a ring of QR codes. **Use them when friends ask** where to get the
hardware and where to find the firmware: open the code and let them scan it with their phone's
camera, which is quicker than spelling out a web address.

```
                          TeeToTum
                           12
      Code quality  11           1  Claude Code
   Plugin guide  10                 2  Waveshare
         Issues  9                     3  Espressif
   Author's blog  8                  4  wasmi
                   7              5  esp-rs
                           6
```

| Segment | Leads to |
|---|---|
| `TeeToTum` | this firmware and its documentation, `github.com/teetotum-rs/firmware` |
| `Claude Code` | the AI coding agent the firmware was written with, `claude.com/claude-code` |
| `Waveshare` | the hardware's wiki page, `waveshare.com/wiki/ESP32-S3-Knob-Touch-LCD-1.8` |
| `Espressif` | the maker of both chips on the board, `espressif.com` |
| `wasmi` | the WebAssembly runtime the plugins run in, `github.com/wasmi-labs/wasmi` |
| `esp-rs` | the Rust projects for Espressif's chips that the firmware is built on, `github.com/esp-rs` |
| `Author's blog` | the author's blog, `stefangruehn.github.io` |
| `Issues` | where to report a problem with the firmware, `github.com/teetotum-rs/firmware/issues` |
| `Plugin guide` | the guide to writing a plugin, in this firmware's repository, `github.com/teetotum-rs/firmware/blob/main/docs/plugin-development.md` |
| `Code quality` | the checks a change must pass, in this firmware's repository, `github.com/teetotum-rs/firmware/blob/main/docs/code-quality.md` |

- **A first tap selects a segment, a second one opens its code.** The code fills the middle of the
  screen, black on white. Below it always stands the site it leads to (for example `github.com`), and
  above it a short caption such as `firmware and docs`, if the space is wide enough. While a code is open, the screen is at full brightness,
  whatever the brightness setting says, so that a phone camera reads it easily; closing the code
  brings the setting's level back.
- **Turning the knob shows the next code** straight away. A tap on the code closes it, and a long
  press goes back to Home.
- **The codes are plain links**, made when the firmware is built: no referral codes, nothing is
  counted, and no place in the ring is paid for. The Waveshare code leads to the wiki page rather
  than the shop; the wiki links on to where the board is sold.

## 5. Music Player

The Music Player shows what your phone is playing and lets you control it. Open it from Home:
select `Music Player` at nine o'clock and tap it again.

### Pairing your phone

The Music Player works through the second chip, so your phone has to be connected to **that
chip as an audio device**:

1. On the phone, open the Bluetooth settings and search for new devices.
2. Pair **`TAIJI_KNOB_AUDIO`**.
3. Play music on the phone. The sound now comes out of the Knob's audio output (the board's
   3.5 mm headphone jack), and title, artist, volume and cover appear on the screen.

**Everything on this screen travels over that audio connection**: title, artist, cover, volume,
play/pause and skipping. If the phone is not connected as an audio device, taps and swipes do
nothing and no title appears. No error is shown, because the second chip does not report one.

The device appears under up to three Bluetooth names. Only one of them is needed here:

| Name | Comes from | What it is for |
|---|---|---|
| `TAIJI_KNOB_AUDIO` | the second chip's factory firmware | Audio. **Pair this one** for the Music Player. |
| `TAIJI_KNOB_HID` | the second chip's factory firmware | A Bluetooth remote control with media keys. The `HID remote` plugin uses it; see [Using plugins](plugins.md). |
| `TeeToTum` | TeeToTum itself (Bluetooth Low Energy) | Not needed for anything a user does today. **It does not appear in the phone's Bluetooth settings list**, only in Bluetooth scanner apps. That is expected. |

The two `TAIJI_KNOB_...` names come from the second chip and cannot be changed without replacing
that chip's firmware.

### The screen

```
              .-'''  volume arc  '''-.
            .'   (starts lower left,   '.
           /     runs over the top)      \
          |                               |
          |        cover picture          |
          |                               |
          |      Title of the track       |      white, larger
          |            Artist             |      grey, smaller
           \                             /
            '.                         .'
              '-._  hold for home  _.-'          gap at the bottom
```

- **The cover** is the ground of the screen (see [Cover](#cover) for its size). Until a cover
  arrives, the Player stands on the same cloud of points as the menus (see
  [Background](#background)).
- **Title and artist** stand below the middle, so the upper half of the cover stays free. Over a
  cover they sit on a darkened band so they stay readable.
- **The volume** runs around the rim as an arc over 270 degrees, from lower left over the top to
  lower right, in the colours of the theme. It is 8 pixels wide, and its outer edge is the rim of
  the screen. The gap at the bottom holds `hold for home`.

**When nothing is playing**, the Player says so and names its gestures:

```
                TeeToTum
            nothing playing

        tap to play  swipe to skip
             turn for volume
```

### Controls

| You do | What happens |
|---|---|
| Tap | Play or pause. |
| Swipe right | Next track. |
| Swipe left | Previous track (read like a timeline: to the left is earlier). |
| Turn the knob clockwise / anticlockwise | Louder / quieter. |
| Long press | Home. |

**Play/pause depends on the second chip.** It decides from the playback state the phone last
reported whether a tap means "play" or "pause", and if it has no usable state, it sends nothing.
Skipping does not have this problem. If a tap does nothing, start playback on the phone.

### Volume

The knob works the volume of the music playing through the Knob. The second chip reads the knob
directly for this; TeeToTum shows the result as the arc, with the number available in the
[Music Player's About](#music-player-menu) as `vol`, from 0 to 127.

**The volume only moves while music is streaming to the Knob.** When the phone pauses or stops
sending audio, the second chip ignores volume steps, and the **arc turns dim** to show that
turning will do nothing. When the music streams again, the arc lights up and the knob works
again.

### Long titles

Title and artist are each measured against the room they have on the round screen. What fits
stands centred. What does not fit stands at its start for 1.5 seconds, scrolls to its end at
40 pixels per second, stands there for 1.5 seconds and then stays at its start, shortened with
`...`. It scrolls once after every change of track and every time you come back to the Player.
It does not keep scrolling.

### Cover

The cover comes from the phone through the second chip, as a **200 x 200 pixel** picture: that is
the thumbnail size the second chip asks the phone for. It cannot ask for a larger one. The Cover setting in the
[Music Player menu](#music-player-menu) chooses between showing it at this size, sharp, in the middle of the screen
(`Sharp`, the default), or enlarged to a round picture 344 pixels across that ends at the inside
of the volume arc, black outside it (`Full screen`), which is bigger but softer.

The second chip asks for a cover **when the track changes**. After connecting, the first cover
may therefore only appear with the next track. Whether a cover arrives at all depends on the phone
and its player app.

## 6. Card over Wi-Fi

`Card over Wi-Fi` lets a phone or a computer read the TF card inside the Knob, without opening the
housing. Open it from Home (select the entry at ten o'clock and tap it again) or from the
Settings, where it stands at eleven o'clock. The line under its name shows the card TeeToTum found
at start-up, for example `14.8 GB card`, or `no card`.

**While the dialog is open, the Knob runs a Wi-Fi network of its own.** The dialog has no buttons.

- **A QR code joins the network.** It stands in the middle, the network's name above it and
  `192.168.4.1` below. Scan it with the phone's camera to join.
- **Turn the knob for the same in words**, for a computer without a camera: `join the network`,
  the name, `with the password`, the password, `then open` and `192.168.4.1`. Turning again
  brings the code back.
- **The name is `TeeToTum-` and four hex digits** taken from the Knob's Wi-Fi address, so two
  Knobs differ. **The password is made anew at every start**: a device that joined before needs
  the new one after a restart.
- **Then open `http://192.168.4.1` in a browser.** It lists the card's top folder as a table: name
  (folders end in `/`), size, created, modified, the day of the last access, and the attributes as
  letters (`R` read-only, `H` hidden, `S` system, `A` archive). Hidden entries are listed too. Times
  are what the card stores, local time without a zone; `—` means the writer set none. A folder
  opens its listing, `..` goes up, and a file is downloaded;
  pictures (JPEG, PNG, GIF, BMP), text (`.txt`, `.log`, `.csv`) and sound (`.mp3`, `.wav`) the
  browser shows or plays itself.
- **Read-only.** Nothing on the card can be written, renamed or deleted over the network.
- **No internet through the Knob.** It hands out addresses but no gateway, so a phone keeps its
  own route to the internet.
- **The network lasts as long as the dialog.** A tap on the screen closes the dialog, and so does
  a long press; either takes the network down, and a download still running with it.
- **Without a card** the dialog says `no card` and `none was found at boot`, and no network is
  started. The card is looked for only when the Knob starts.
- **While a file is on its way**, a moving [background](#background) holds still and the Wi-Fi
  scans (for [Nearby](plugins.md), for example) wait, because either would slow the download.

## 7. Settings

Open the Settings from Home: select the gear at eleven o'clock and tap it again. The menu's title
is `Settings`.

```
                          About
                           12
  Card over Wi-Fi  11           1  Orientation
     Music Player  10                 2  Theme
       Receive  9                     3  Brightness
         Nearby  8                  4  Haptics
        Teetotum  7             5  Background
                           6
                       HID remote
```

The firmware's own settings are About and the five entries clockwise from it; the fifth,
Background, is a menu of its own. [Card over Wi-Fi](#6-card-over-wi-fi) stands left of About,
the Music Player's menu left of that. The bundled
plugins' settings follow Background clockwise, one segment each, from six o'clock on. After the
last of them stands `Receive`, which takes a plugin over Bluetooth.

While an entry is selected in the ring, its **current value** stands under its name, so you can
read every setting without opening it. An entry that leads to a menu shows where that menu
stands: `Background` whether the cloud is `still` or `moving`, `Music Player` the cover as
`sharp` or `full screen`, and each plugin whether it is `loaded`, `not loaded` or `stopped`.
`Receive` shows how many slots are free for a plugin, for example `13 free slots`, and
`Card over Wi-Fi` the card, as at Home.

### How a setting is changed and stored

1. Select the entry and tap it again (or tap the middle). The setting opens.
2. Turn the knob. **The change shows at once**: the picture turns, the colours change, the screen
   gets brighter, the clicks get stronger, the background changes.
3. Tap the **tick** to keep it, or the **cross** to put back the value it had when you opened
   it. A long press does the same as the cross, and then goes to Home.

**A setting is stored when you tap the tick, and only if it actually changed.** It is kept in
the ESP32-S3's own flash memory and survives switching off. Nothing is stored while you turn: what
you see while the setting is open is a preview until you confirm it.

| Setting | Values | Default | At the end of the range |
|---|---|---|---|
| [Orientation](#orientation) | `0 deg`, `90 deg`, `180 deg`, `270 deg` | `0 deg` | goes round |
| [Theme](#theme) | eleven colour themes | `Red` | goes round |
| [Brightness](#brightness) | `10 %` to `100 %` in steps of 10 | `100 %` | stops |
| [Haptics](#haptics) | `Off`, then `1 / 9` to `9 / 9` | `9 / 9` | stops |
| [Motion](#background) (Background) | `Still`, `Moving` | `Still` | every detent flips it |
| [Points](#background) (Background) | `100` to `5000` in steps of 50 | `2450` | stops |
| [Brightest](#background) (Background) | `10 %` to `100 %` in steps of 2 | `98 %` | stops |
| [Dark centre](#background) (Background) | `0 px` to `130 px` in steps of 5 | `0 px` | stops |
| [Icon colour](#background) (Background) | `0 %` to `100 %` in steps of 5 | `40 %` | stops |
| [Cover](#music-player-menu) (Music Player) | `Sharp`, `Full screen` | `Sharp` | every detent flips it |
| [Installed](#plugin-entries) (per plugin) | `Yes`, `No` | `Yes` | every detent flips it |

To leave the Settings, long-press. Settings is one level below Home, so it shows
`hold for home` in place of a tick.

### About

The value under the name in the ring is the **firmware version**, for example `v0.1.0`. Opening
About shows the device's state:

```
            TeeToTum
        MIT OR Apache-2.0
     up 812 s  wi-fi 9 nets
    ble advertising  knob +37
```

| Line | Meaning |
|---|---|
| `TeeToTum`, `MIT OR Apache-2.0` | The firmware and its licence. |
| `up ... s` | Seconds since the device started. |
| `wi-fi ... nets` | How many Wi-Fi networks the last scan found. TeeToTum only counts them; it never connects. |
| `ble advertising` / `ble connected` | Whether a device is connected to TeeToTum's own Bluetooth Low Energy service. This is **not** the phone's music connection, which goes to the second chip. |
| `knob ...` | Detents counted since start, clockwise positive. |

About only shows information, so it has only the tick.

### Orientation

Turns the picture in quarter turns, so you can use the device in any position, for example with
the USB cable pointing away from you. Each detent clockwise turns the picture 90 degrees
clockwise; after `270 deg` comes `0 deg` again. The dialog says `turn the knob` and
`About marks the top`: the About segment is always at the top of the picture, so it shows where
"up" is.

Touch follows the picture: taps land where you see them, and swipes are named as you see them
(see [the gestures](#the-gestures)).

The display turns the picture itself, so a turned picture redraws as fast as an upright one.

### Theme

The colours of the ring, the buttons, the values and the [background](#background). There are
eleven themes, and the knob goes round them in this order:

`Teal`, `Orange`, `Magenta`, `Violet`, `Blue`, `Pink`, `Red`, `Green`, `Cyan`, `Indigo`,
`Grey`

The dialog says `the ring shows it`: the ring around the open dialog is drawn in whichever theme
the knob has reached.

### Brightness

How bright the screen is, in ten steps from `10 %` to `100 %`. The screen changes as you turn
(`the screen shows it`). The steps are spaced
 for the eye rather than evenly, so each one looks
like a similar change. **The range stops at both ends** rather than going round, so that one
detent too far never jumps from the brightest to the darkest. The darkest step is dim, not off.
At either end a further detent does not click, because nothing moves.

### Haptics

How strongly the motor clicks, in ten steps: `Off`, then `1 / 9` to `9 / 9`. Every detent in the
dialog clicks at the strength it has just reached (`the clicks show it`), so the dialog is its own
test. Like Brightness, it stops at both ends.

With `Off`, **nothing clicks anywhere**: not the knob, not a tap, not a long press, and not the
pulses a plugin asks for. Home then shows `silent` in the Settings' state line.

### Background

Home, the menus and the Music Player without a cover stand on a **cloud of points** in the colours
of the theme: most points in the theme's selected colour, a share in its icon colour. They get
darker towards the middle, where names, values and dialogs are read. Plugin faces do not get the
cloud; they start from black.

The entry at five o'clock, an icon of scattered dots in two sizes, opens a menu of its own, titled
`Background`, with About at the top and five settings clockwise from it. Its tick goes back up to
the Settings.

```
                  About
                   12
                          1  Motion
                             2  Points
                               3  Brightest
                             4  Dark centre
                          5  Icon colour
```

They work like the other settings: turning the knob in a dialog shows the change at once, because
the dialog stands on the cloud it changes. The tick keeps the change and stores it, so it survives
a restart; the cross puts back what was there when the dialog opened. The four numbers stop at
either end of their range, and a further detent there does not click.

| Entry | What it sets | Values | Default | The dialog says |
|---|---|---|---|---|
| Motion | Whether the cloud stands still or moves. `Moving` turns the whole cloud once in two minutes, and every point breathes brighter and darker at a pace of its own; the screen is redrawn every 40 ms. It moves wherever it is the ground: in the menus, and on the Music Player until a cover arrives. | `Still`, `Moving` | `Still` | `the cloud shows it` |
| Points | How many points there are. | `100` to `5000` in steps of 50 | `2450` | `50 a detent` |
| Brightest | How bright the points are at the rim of the screen; towards the middle they get darker. | `10 %` to `100 %` in steps of 2 | `98 %` | `at the rim` |
| Dark centre | A disc in the middle without points: no point is nearer the middle than this. | `0 px` to `130 px` in steps of 5 | `0 px` | `no points inside it` |
| Icon colour | The share of points in the theme's icon colour; the others are in its selected colour. | `0 %` to `100 %` in steps of 5 | `40 %` | `points in the icon colour` |

With `Moving`, Home adds `moving` to the Settings' state line.

**About** shows the cloud as it is set, and what it costs:

```
           Background
        a cloud of points
    2450 points, 98 % bright
  centre 0 px, 40 % icon colour
      still, ground 6.7 ms
```

| Line | Meaning |
|---|---|
| `... points, ... % bright` | Points and Brightest. |
| `centre ... px, ... % icon colour` | Dark centre and Icon colour. |
| `still` / `moving`, `ground ... ms` | Motion, and how long drawing the last background took, in milliseconds. |

### Music Player menu

The entry at ten o'clock opens a menu of its own, titled `Music Player`, with the About at the
top and Cover at one o'clock. Its tick goes back up to the Settings.

**About** shows what the second chip reports:

```
          Music Player
     through the other chip
         cover  200x200
       vol 63  encoder on
        audio streaming
       hid not connected
```

| Line | Meaning |
|---|---|
| `cover  200x200` / `no cover` | The size of the cover picture on the screen, or none yet. |
| `vol ...  encoder on` / `off` | The volume (0 to 127) and whether the second chip is reading the knob. |
| `other chip silent` | Shown instead while the second chip does not answer: before its first answer, and after about six seconds without one. |
| `audio streaming` / `no audio stream` | Whether music is streaming to the Knob. The volume only moves while it is. |
| `hid connected` / `hid not connected` | Whether a phone is connected to the second chip's remote control, `TAIJI_KNOB_HID`. |

**Cover** chooses how the cover stands behind the Player: `Sharp` (at its own 200 x 200 pixels in
the middle, the default) or `Full screen` (enlarged to a round picture 344 pixels across, ending at
the inside of the volume arc, black outside it). The dialog adds
`sharp keeps 200 px`. Any detent flips between the two. The change shows once you tap the tick:
the last cover is drawn again at the new size straight away, without waiting for the next track.

### Plugin entries

Each bundled plugin has an entry in the Settings ring (`HID remote`, `Teetotum` and `Nearby` at
six, seven and eight o'clock). It opens the **plugin's menu**, titled with the plugin's name:

```
                  About
                   12
   Main Settings 11      1  Installed
```

- **About** shows what the plugin is: its name, `bundled with the firmware` or `from slot` and
  its number, its size, the rights
  it asks for, and whether it is loaded, not loaded or stopped, with its load time and memory.
- **Installed** is `Yes` or `No`. The dialog adds `OK restarts`: with `No` the plugin leaves Home
  and the plugins after it move up, with `Yes` it comes back. Nothing happens until you tap the
  tick, which saves the choice and restarts the Knob.
- **Main Settings** (the gear at eleven o'clock) leads to the firmware's Settings, the same way it
  does in the menu of any plugin. The tick in the plugin's menu goes back up one level.

What the rights mean, and what a plugin does when it is stopped, is explained in
[Using plugins](plugins.md#plugin-settings).

### Receive

`Receive` stands after the last plugin's entry, at nine o'clock with the three bundled plugins. It
opens a dialog that takes one plugin over Bluetooth: `a plugin over BLE`, then
`waiting for a sender` and `visible as TeeToTum`, the name the Knob announces over Bluetooth. When a sender connects, the
dialog says `connected`, then shows how much has arrived and into which slot. At `written` the
Knob restarts and asks in the install dialog whether to install the plugin.

The Knob accepts a plugin only while this dialog is open. The tick closes it. How to send a
plugin, and what an orange message in the dialog means, is in
[Using plugins](plugins.md#sending-a-plugin-over-bluetooth).

## 8. Plugins

A plugin is a small add-on program with a screen of its own, called its **face**. Plugins appear
at Home to the right of the Home segment, and you start one the way you open the Player: select its
segment, tap it again. A long press brings you back to Home from any plugin, and `hold for home`
at the bottom of its face says so.

Three plugins come with the firmware:

| At Home | Summary it shows | In short |
|---|---|---|
| `HID remote` | `remote for the phone's player` | Controls the phone's music player as a Bluetooth remote, through `TAIJI_KNOB_HID`. Tap to play or pause, swipe to skip, turn to seek. |
| `Teetotum` | `a die of 2 to 256 sides` | A die: every detent of the knob throws it. |
| `Nearby` | `Wi-Fi and Bluetooth around you` | Shows Wi-Fi networks and Bluetooth devices nearby as dots, stronger ones closer to the middle. |

A plugin only gets what its **rights** allow: for example the knob, the vibration motor or the
radio. A plugin without the knob right leaves the knob to the volume. If a plugin fails, its face
shows the plugin's name, `stopped` and the reason; starting it again from Home loads it fresh.

Everything else about plugins (each bundled plugin in detail, rights, removing and reinstalling,
memory limits, and what is possible beyond the bundled ones) is in [Using plugins](plugins.md). If
you want to write one, see [Writing plugins](plugin-development.md).

## 9. Troubleshooting

**The screen stays dark.**
TeeToTum lights the screen only once Home is ready, so a short dark moment after switching on is
normal. If it stays dark, check that the power switch is on and that the USB-C cable carries power
(try another cable or charger); with a battery, the battery may also be flat. If
[Brightness](#brightness) was set very low, the screen is dim but never off.

**The knob does not change the volume.**
The volume only moves while music is streaming to the Knob, and the **dim arc** on the Player shows
when it is not. Check that:

- the phone is connected to `TAIJI_KNOB_AUDIO` as an audio device, and its sound comes out of the
  Knob rather than the phone's own speaker;
- music is actually playing, not paused. The Music Player's About says `audio streaming` or
  `no audio stream`.

In the menus and on a plugin that uses the knob, the knob does not work the volume. That is
intended.

**No title, no cover, and taps and swipes do nothing.**
All of these travel over the audio connection to the second chip. If one is missing, usually all
are, and the cause is the same: the phone is not connected as an audio device. Pair or reconnect
`TAIJI_KNOB_AUDIO` (see [Pairing your phone](#pairing-your-phone)). If only the cover is missing,
wait for the next track: the second chip asks for a cover when the track changes. If only the tap
does nothing, see [Controls](#controls).

**The phone shows the Knob as one combined device, and the HID remote does nothing.**
When a phone knows the Knob both as an audio device and as a remote control, it may list a
single entry, `TAIJI_KNOB_AUDIO`, without an "input device" switch, and the remote-control
connection never comes up. This happened on the phone used for testing, and **unpairing on the
phone did not fix it: unpaired is not the same as forgotten.** What helped was **restarting the
phone**; afterwards `TAIJI_KNOB_HID` could be paired under its own name. More in
[Using plugins](plugins.md#when-something-goes-wrong).

**`TeeToTum` does not appear in the phone's Bluetooth settings.**
That is expected. The phone's settings list only shows devices it can pair with as a system
function, such as audio or a remote control. TeeToTum's own Bluetooth service is not one of these.
Scanner apps do see it.

**The music info disappears for a moment, or the knob briefly does not respond.**
The second chip's factory firmware occasionally crashes and restarts on its own, especially after
many Bluetooth reconnections. It comes back by itself and reconnects to the phone, and TeeToTum
sets it up again when it answers. While it is away, the Music Player's About shows
`other chip silent`.

**A swipe is taken as a tap.**
Swipe a little further: a movement shorter than about a tenth of the screen counts as a tap.

**A setting changed back after a restart.**
Settings are only stored when you tap the **tick**. The cross, and a long press out of an open
setting, put the old value back.

**The computer does not see the right chip over USB** (only relevant for flashing).
Which of the two chips is connected to USB depends on **which way round the USB-C plug is
inserted**. The ESP32-S3, which runs TeeToTum, shows up as `303a:1001` (a serial port like
`/dev/ttyACM0` on Linux). If `lsusb` shows a **CH340** instead, you are connected to the second
chip: turn the plug over.

## 10. Building and flashing it yourself

This section is for people who put TeeToTum on the board. The web installer needs no toolchain;
everything after it is for building from source. Either way, only the ESP32-S3 is written, never the
second chip.

### Installing without building

The [web installer](https://teetotum-rs.github.io/firmware/) writes the latest release from the
browser. It needs desktop Chrome, Edge or Opera; Firefox and Safari cannot talk to serial ports.
Connect the board so that the **ESP32-S3** is on USB (see the last item of
[Troubleshooting](#9-troubleshooting)), press **Connect and install** and pick
`USB JTAG/serial debug unit`. Writing takes about half a minute.

Your settings and the plugins installed in the flash slots are kept. On a first install the dialog
offers to erase the device; that clears the whole flash and is the clean start after the factory
demo.

**Chrome installed as a Flatpak on Linux** cannot tell which device is on a port unless it may read
the device database: the list shows only `ttyACM0`, and the install stops with
"Failed to initialize". Allow it, quit Chrome completely and open the page again:

```
flatpak override --user --filesystem=/run/udev:ro com.google.Chrome
```

### What you need

- The Rust toolchain for the ESP32-S3's Xtensa core, installed with
  [`espup`](https://github.com/esp-rs/espup). The repository's `rust-toolchain.toml` selects it
  (`channel = "esp"`).
- `espflash`, the flashing tool.
- In every new shell, the environment espup sets up, before building:

  ```
  . ~/export-esp.sh
  ```

  Without it the build stops with ``linker `xtensa-esp32s3-elf-gcc` not found``.
- On Linux, membership in the `dialout` group, so that no `sudo` is needed.

The bundled plugins are already built and checked in under `firmware/assets/plugins/`; you do not
need to build them to build the firmware.

### Build and flash

Connect the board so that the **ESP32-S3** is on USB (see the last item of
[Troubleshooting](#9-troubleshooting)), then, from the repository root:

```
cargo build --release     # build only
cargo run --release       # build, flash, and read the log
```

`cargo run --release` calls `espflash flash --monitor` with the right settings (see
`.cargo/config.toml`) and then shows the device's log. The monitor needs a real terminal; to keep
a copy of the log, use `cargo run --release 2>&1 | tee knob.log`.

**It flashes the partition table in `partitions.csv`** along with the firmware: the settings, two
4 MB application slots and a 1 MB `plugins` partition. Calling `espflash flash` yourself, pass
`--partition-table partitions.csv`; without it espflash writes its own table, which has no
`plugins` partition.

**Use `-B 921600` whenever you call `espflash` yourself.** At the default speed one megabyte takes
about 93 seconds over the S3's USB, and long transfers abort with
`Timeout while running command`; at 921600 baud it takes about 14 seconds. `cargo run` already
passes it.

```
espflash board-info -B 921600      # chip, flash size, MAC
espflash monitor                   # read the log without flashing
```

Flashing a new build does not touch the microSD card, and your settings are kept: they live in a
part of the flash that flashing the firmware does not overwrite.

### Resetting the settings

To go back to all defaults, erase the two sectors the settings live in:

```
espflash erase-region -B 921600 0x9000 0x2000
```

The next start runs with default settings, and **the motor buzzes once** because it calibrates
again. This address is where `partitions.csv` puts the settings partition (`nvs`); TeeToTum looks the partition up at run time, so check it if you changed the
partition table.

### Restoring the factory firmware

A complete image of the ESP32-S3's original flash was taken before TeeToTum was first installed.
Writing it back brings the factory demo back, and **it replaces TeeToTum and its settings
completely**. The procedure has been tried and works, but it is not a single command: sixteen
megabytes in one go time out, so it is written in two pieces. Follow
[Factory firmware backups](../backup/README.md) step by step.

The same folder describes a backup of the second chip. TeeToTum never writes that chip, so there
is normally no reason to restore it.

## 11. Glossary

**About**
: The entry at the top of every menu except Home. It shows information and marks which way is up.

**Background**
: The cloud of points in the theme's colours behind Home, the menus and the Music Player without a
  cover. Set under [Background](#background).

**Cancel**
: The cross in a setting. Puts back the value the setting had when it was opened.

**Card over Wi-Fi**
: The entry that lets a browser read the TF card over a Wi-Fi network of the Knob's own, while
  its dialog is open. See [Card over Wi-Fi](#6-card-over-wi-fi).

**Detent**
: One click-stop of the knob.

**Face**
: The screen of a plugin, or the Music Player's screen.

**Home**
: The menu the device starts in and where a long press leads from anywhere else.

**Knob, knob**
: With a capital K, the whole device. With a small k, the ring you turn around the screen.

**Long press**
: A finger resting still on the screen for about 0.6 seconds. Leads to Home, and on Home to the
  QR codes.

**OK**
: The tick in a setting (keeps and stores the value) or in a menu (goes back up one level).

**Page**
: A menu with more entries than its twelve segments hold runs on to a second ring. A row of dots
  under the top segment says how many pages there are and which one you are on. See
  [How the menus work](#how-the-menus-work).

**Plugin**
: An add-on program with a face of its own. See [Using plugins](plugins.md).

**QR codes**
: A ring of links a long press opens on Home, each shown as a code a phone camera can read. See
  [QR codes](#qr-codes).

**Ring**
: The twelve segments around the edge of the screen that make up every menu.

**Rights**
: What a plugin is allowed to use: the knob, the motor, the radio and so on.

**Second chip**
: The classic ESP32 on the board. It runs its factory firmware and handles Bluetooth audio,
  the sound output and the Bluetooth remote control. TeeToTum sends it commands.

**`TAIJI_KNOB_AUDIO`, `TAIJI_KNOB_HID`**
: The Bluetooth names of the second chip: audio device and remote control.
