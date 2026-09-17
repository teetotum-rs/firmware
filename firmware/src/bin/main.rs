#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
// Frames over the 1024-byte threshold are expected one by one, each with its reason. The main
// stack is 88 KiB and peaks at 60 KiB, measured while a cover decodes.
#![deny(clippy::large_stack_frames)]

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use bt_hci::controller::ExternalController;
use bt_hci::param::LeAdvReportsIter;
use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_futures::join::{join, join5};
use embassy_futures::select::{Either3, select3};
use embassy_futures::yield_now;
use embassy_net::StackResources;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Arc, PrimitiveStyle, Rectangle};
use embedded_storage::nor_flash::NorFlash;
use esp_backtrace as _;
use esp_hal::Blocking;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Io, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::system::software_reset;
use esp_hal::time::Duration as HalDuration;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use esp_radio::ble::controller::BleConnector;
use esp_radio::wifi::AuthenticationMethod;
use esp_radio::wifi::ap::AccessPointConfig;
use esp_radio::wifi::scan::{ScanConfig, ScanTypeConfig};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{Config as WifiConfig, WifiController};
use esp_storage::FlashStorage;
use log::{error, info, warn};
use static_cell::StaticCell;
use teetotum::cloud::Cloud;
use teetotum::companion::{
    BAUD, COVER_MAX_BYTES, Companion, Direction, Event as CompanionEvent, MediaKey, QueueKey,
};
use teetotum::cover::{self, Cover, Step as CoverStep};
use teetotum::encoder::Encoder;
use teetotum::fat::Volume;
use teetotum::framebuffer::{BYTES as SCREEN_BYTES, Framebuffer, HEIGHT, WIDTH};
use teetotum::haptic::{Actuator, CalTime, Haptic, Library, Mode as HapticMode};
use teetotum::menu::{
    BODY, Buttons, Entry, FIRMWARE_SLOT, Icon, Id, Kind, MAX_PAGES, Menu, Navigator, Outcome,
    Owner, RING_BYTES, Ring, SLOTS, fonts, icons, shade, shortened, text as menu_text,
    width as menu_width,
};
use teetotum::screen::ORIENTATIONS;
use teetotum::screen::{Path, Screen, ScreenPins};
use teetotum::sd::{self, SdCard};
use teetotum::store::Store;
use teetotum::touch::{Gesture, Press, Taps, Touch};
use teetotum_face::manifest::{Manifest, Signed, Version};
use teetotum_face::{Event as FaceEvent, HINT, Radio, Rights, Usage};
use teetotum_firmware::VERSION;
use teetotum_firmware::backlight::Backlight;
use teetotum_firmware::flash::{self, Region, TABLE_SCRATCH};
use teetotum_firmware::nearby::{self, Heard};
use teetotum_firmware::plugin::{self, Page, Plugin, PluginId};
use teetotum_firmware::qr::{self, LINKS};
use teetotum_firmware::settings::{
    self, Bond, Brightness, CloudShape, CoverStyle, Haptics, Motion, Part, Settings, Theme,
};
use teetotum_firmware::share;
use teetotum_firmware::shot;
use teetotum_firmware::slots::{self, Slots, Upload};
use teetotum_firmware::upload::{self, Command, Status as UploadStatus};
use trouble_host::connection::ScanConfig as BleScanConfig;
use trouble_host::prelude::*;
use trouble_host::scan::Scanner;

extern crate alloc;

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 1;

/// How long to wait between scans.
const SCAN_INTERVAL: Duration = Duration::from_secs(30);
/// Upper bound on the networks a single scan reports.
const SCAN_MAX_NETWORKS: usize = 20;
/// How long to listen on each channel before moving on.
///
/// The default of 10-20 ms is too short in practice: beacons arrive roughly every 100 ms, so a
/// channel is often left before anything on it has spoken. The default found only 1 of 8
/// reachable networks; these values find them all, at the cost of a scan that takes a few
/// seconds instead of a fraction of one.
const SCAN_DWELL_MIN: HalDuration = HalDuration::from_millis(100);
const SCAN_DWELL_MAX: HalDuration = HalDuration::from_millis(300);

/// How long each BLE scan window stays open.
const BLE_SCAN_WINDOW: Duration = Duration::from_secs(5);
/// How long to wait between BLE scan windows.
const BLE_SCAN_INTERVAL: Duration = Duration::from_secs(25);
/// Upper bound on the advertisers remembered per window, to cap the allocation.
const BLE_MAX_DEVICES: usize = 40;

/// While a face that listens is on the screen -- one with `Rights::RADIO`, see
/// `teetotum_firmware::nearby` -- the two loops come round as fast as they go: a Wi-Fi scan
/// follows the last after this pause, which with the dwell times above makes a round every few
/// seconds...
const NEARBY_WIFI_GAP: Duration = Duration::from_secs(1);
/// ...and a BLE window lasts this long, with none between.
const NEARBY_BLE_WINDOW: Duration = Duration::from_secs(2);
/// How often a long pause looks whether such a face has come up.
const NEARBY_POLL: Duration = Duration::from_millis(250);

/// How often to check whether the peer is still there.
///
/// Only affects how precisely the connection duration is reported, not the connection itself.
const CONNECTION_POLL: Duration = Duration::from_millis(500);
/// The name the knob advertises itself under.
///
/// Scanning cannot find a phone by name -- Android advertises to strangers with a rotating
/// address and no name, by design. Being findable is therefore the direction that works: the
/// knob advertises, and the phone lists it.
const BLE_DEVICE_NAME: &str = "TeeToTum";

/// What the knob claims to look like, and the UUID it claims to speak.
///
/// Neither makes a phone's Bluetooth settings screen list the knob: that screen only offers
/// profiles it can pair to (such as a HID device), not an arbitrary GATT peripheral. They stay
/// because each is useful on its own: GAP has a category for exactly this object (0x13/0x0d,
/// "Dial"), and a service UUID on the air is what a Web Bluetooth filter selects on -- which is
/// how a plugin upload would find the knob.
const KNOB_APPEARANCE: BluetoothUuid16 = appearance::control_device::DIAL;
/// The AD type for the appearance; trouble-host has no `AdStructure` variant for it.
const AD_TYPE_APPEARANCE: u8 = 0x19;
/// `KnobService`'s UUID, little-endian, for the scan response.
///
/// It has to be written out a second time because `#[gatt_service]` takes a string literal and
/// gives nothing back to refer to. **Both spellings are the same UUID and have to stay that way.**
const KNOB_SERVICE_UUID_LE: [u8; 16] = [
    0xbb, 0xd6, 0xba, 0x78, 0xcd, 0xa6, 0x39, 0xaa, 0x5b, 0x4c, 0x8f, 0x6b, 0xb7, 0x54, 0x71, 0xaa,
];

/// Whether a peer is currently connected.
///
/// Scanning and a connection share one radio, so the scan loop stands down while a peer is
/// connected. Connections still drop after exactly 30 s even with scanning paused, which points
/// at a timeout on the peer rather than at missed connection events here.
///
/// Set by the advertising loop, read by the scanning loop.
static PEER_CONNECTED: AtomicBool = AtomicBool::new(false);

/// Networks seen in the most recent Wi-Fi scan, published over GATT.
static WIFI_NETWORKS: AtomicU8 = AtomicU8::new(0);

/// Whether the receive dialog is open, the only time an upload is taken.
///
/// Set by the device loop, read by the advertising loop.
static RECEIVING: AtomicBool = AtomicBool::new(false);

/// The settings a sender reads over BLE, as [`teetotum_pack::settings`] encodes them.
///
/// Set by the device loop, read by the advertising loop.
static SHARED_SETTINGS: Mutex<Cell<[u8; teetotum_pack::settings::LEN]>> =
    Mutex::new(Cell::new([0; teetotum_pack::settings::LEN]));

/// How often the connected peer's view of the characteristics is refreshed.
const GATT_REFRESH: Duration = Duration::from_secs(1);

/// How long the knob waits after an upload or a delete before it restarts: long enough for the
/// status to reach the sender.
const UPLOAD_RESTART: Duration = Duration::from_millis(500);

/// How long the loop sleeps before coming round again.
///
/// **The knob no longer sets this number.** It used to: a detent was a pin that had to be read
/// while it was low, so the period was the difference between counting a fast turn and losing
/// half of it. Since the edge is latched by the GPIO interrupt (`teetotum::encoder`), a pass
/// that comes late collects everything that happened while it was away, and what is left here
/// is how quickly a turn or a finger shows up on the screen.
const INPUT_PERIOD: Duration = Duration::from_millis(10);

/// How often the screen is asked for a finger.
///
/// The controller names a slide **while the finger is still down** rather than on release, so a
/// slow poll does not merely delay a gesture, it drops it.
const TOUCH_PERIOD: Duration = Duration::from_millis(20);

/// How often the other chip is asked how it is doing.
const STATUS_PERIOD: Duration = Duration::from_secs(2);

/// Status queries in a row without an answer before the other chip counts as silent.
const STATUS_UNANSWERED: u8 = 3;

/// What the other chip is asked to be: bit 0 so it looks at its own encoder at all, and in bits
/// 1..3 who gets a detent -- mode 1 has it work the volume by itself, mode 2 sends the detent to
/// us as `BD 07` or `BD 08`. [`KNOB_VOLUME_AT_CHIP`] chooses.
///
/// It is **re-asserted whenever the answer disagrees, not set once.** This byte is a global in
/// the other chip's RAM, and that chip restarts on its own after a crash. A setting that
/// survives only until the other end reboots is not a setting.
///
/// Bits 4, 5 and 6 are the other chip's own and are left out of the comparison.
const COMPANION_STATE: u8 = if KNOB_VOLUME_AT_CHIP { 0x03 } else { 0x05 };

/// The state byte asked for while the settings are open, and while a face with the knob right is
/// shown -- see [`companion_state`].
///
/// **Mode is a command, not a wiring choice**, so the knob can change hands at a long press: in
/// the settings it walks the menu and turns the setting, and the phone's volume must not follow
/// it there. Mode 2 rather than mode 0 because the second encoder's frames are the only witness
/// this side has that the shaft moved at all.
const COMPANION_STATE_SETTINGS: u8 = 0x05;
const COMPANION_STATE_MASK: u8 = 0x0F;

/// Who works the volume: the other chip from its own encoder, or this firmware from ours.
///
/// **Not a matter of taste but of what arrives.** The way round that goes through us loses half
/// of a fast turn twice over -- once at our polled encoder, which the loop does not ask often
/// enough while it redraws, and once at the other chip, which took only three of six keys even
/// at 120 ms apart. Its own encoder is on the same shaft and reported every detent of the same
/// turns.
///
/// The reason to keep the knob on this side was that a plugin should be able to borrow it --
/// and that survives, because the mode is a command and not a wiring choice: `A3 09` switches
/// it either way at any moment.
const KNOB_VOLUME_AT_CHIP: bool = true;

/// Which way a piece of the picture reaches the bus.
///
/// [`Path::Copied`] is what has ever put a picture on this screen out of the firmware, and it
/// stays: [`Path::Direct`] is faster (6.6 ms a frame instead of 14.4, see `src/bin/psramdma.rs`)
/// but streaks at 80 MHz, because the SPI bus outruns what the external RAM can read.
/// [`Path::Staged`] is the slowest of the three and not a candidate. See
/// [`Screen::set_path`](teetotum::screen::Screen::set_path).
const SCREEN_PATH: Path = Path::Copied;

/// How long the very first picture stays on the screen before the loop is allowed to draw again.
///
/// A picture wrong from the *first* present points at something static -- the frame's address,
/// the mapping, the bring-up -- rather than at other traffic. The loop redraws far too quickly
/// for an eye to catch the first frame on its own, so this holds it for inspection. `None`
/// outside such a debugging run.
const FIRST_FRAME_HOLD: Option<Duration> = None;

/// How many of the first frames are asked whether the external RAM holds them.
///
/// [`external_probe`](teetotum::display::external_probe) checks this without eyes: write the
/// cache back exactly as a transfer does, invalidate the range, and read the picture again off
/// the external bus. Two passes over the picture cost about 16 ms, so only the first few frames
/// pay it. 0 outside such a debugging run.
const EXTERNAL_PROBES: usize = 0;

/// The click under one detent of the knob, and the one under a tap on the screen.
///
/// Numbers into the DRV2605L's ROM library: 24 is listed as "Sharp Tick 1" and 1 as "Strong
/// Click". The names are the datasheet's; which one belongs under which gesture was decided at
/// the knob, and `src/bin/effects.rs` is the run that plays all 123 of them for exactly that.
const CLICK_DETENT: u8 = 24;
const CLICK_TAP: u8 = 1;
/// What a face's pulse is made of: a tap's click, since a pulse is a tap the firmware makes on
/// the face's behalf.
const CLICK_PULSE: u8 = CLICK_TAP;

/// How long a click is allowed to run before it is cut off.
///
/// A ROM waveform has no length to set, so the only way to shorten one is to clear `GO` early:
/// the click starts exactly as before and simply stops sooner. The trade is the effect's own
/// braking at the end, which a cut click does not get.
///
/// Resolution is [`INPUT_PERIOD`], since that is when the loop comes back to look.
const CLICK_LENGTH: Duration = Duration::from_millis(12);

/// How long a click may run while the settings are open: shorter than [`CLICK_LENGTH`].
///
/// **In the menu a click used to last as long as the redraw behind it**, not [`CLICK_LENGTH`]:
/// every detent moves the ring, and sending the picture blocks for 14 to 42 ms before the loop
/// comes back to cut the click. So the menu ends its click itself, right before it draws --
/// which is also what lets this be shorter than [`INPUT_PERIOD`]. Seven milliseconds is a little
/// over one period of the 161 Hz actuator: the shortest cut that is still a whole oscillation
/// rather than a twitch.
const CLICK_LENGTH_MENU: Duration = Duration::from_millis(7);

/// The drive every click had before there was a setting for it, as rated voltage and overdrive
/// clamp in the DRV2605L's own units: what the chip held after power-up, since this firmware
/// never wrote either until then. **Measured**: the first boot of the build that writes them
/// read `0x3e` and `0x8c`, the datasheet's defaults -- and not the `0x3F` and `0x89` that
/// `src/bin/haptic.rs` had called the chip's own.
///
/// **There is room above it.** The clamp comes to roughly 3 V peak, and the driver sits on the
/// 5.3 V USB rail. If the strongest step is not strong enough, this is the number to raise, and
/// [`DRIVE`] with it.
const DRIVE_FULL: (u8, u8) = (0x3E, 0x8C);

/// The drive for steps 1 to 9 of [`Haptics`]; off is no click at all, see [`click`].
///
/// **Both registers scale together**, so a click keeps its shape and only gets softer: the ROM
/// waveform is the same at every step. In closed loop the clamp is what the short kick at the
/// start of a click is driven at, and the rated voltage is where the loop stops overdriving. A
/// click cut after 7 or 12 ms is mostly that kick.
///
/// **The steps are geometric, from a fifth of full to full**, each about 1.22 times the one below:
/// equal steps would crowd the top, where a hand tells amplitudes apart the least. Chosen on
/// paper, and the dialog is the measurement -- every detent in it clicks at the step it reaches.
const DRIVE: [(u8, u8); 9] = [
    (12, 28),
    (15, 34),
    (19, 42),
    (23, 51),
    (28, 63),
    (34, 77),
    (41, 94),
    (51, 114),
    DRIVE_FULL,
];

/// Where the factory demo keeps its full-screen backgrounds, and where this firmware looks
/// for its own.
///
/// A file counts as a background if it is exactly one screen of RGB565, with or without the
/// four-byte header the demo's files carry. That is the whole of the format check: the pixels
/// are already in the panel's order, so a background is a read into memory and not a decode.
const BACKGROUND_FOLDER: &str = "/CLOCKBG";
/// How many backgrounds the screen will swipe through.
const MAX_BACKGROUNDS: usize = 12;
/// Whether the card's photographs are laid behind the status screen at all.
///
/// Off, because the status screen is there to be read, and text over a photograph reads worse
/// than text on black. Nothing is torn out -- with this back at `true`,
/// boot puts the first picture up and swiping up and down walks the rest, exactly as before.
/// It switches off at the one place that matters, the list of names: an empty list is the path
/// a card without pictures already takes, so no other code learns a second way to have no
/// background. The cover of a playing track is a different backdrop and is not affected.
const CARD_BACKGROUNDS: bool = false;

/// The firmware's settings as the ring shows them: About at twelve o'clock, then clockwise the
/// picture's orientation, the colour theme, the screen's brightness, the strength of the clicks,
/// whether the background moves and the bundled plugins,
/// the rest free for what comes next. When the card's backgrounds come back (see [`CARD_BACKGROUNDS`]) they come back as an
/// entry here, because a list to choose from is what settings are for.
///
/// **A long press opens the home menu, from any screen, and the gear left of Home opens these**;
/// OK in their top menu goes back home. The rules the ring keeps are `teetotum::menu`'s. A long
/// press reaches them instead of a vertical swipe because the settings have to be reachable from
/// inside every plugin, and a swipe is exactly what a plugin's own screen is likely to want for
/// itself.
///
/// The orientation is not a constant -- the knob turns it a quarter at a time, the screen turns
/// with it, and the `nvs` partition keeps it.
///
/// **A `const`, not the menu itself.** The plugin's entry is known only once its manifest has been
/// read at boot, so `main` puts them in from [`PLUGIN_SLOT`] on and keeps the result for good.
const SETTINGS: Menu = Menu::new(
    "Settings",
    Entry::setting("About", &icons::ABOUT, SETTING_ABOUT, Buttons::Ok),
)
.with(
    1,
    Entry::setting(
        "Orientation",
        &icons::ORIENTATION,
        SETTING_ORIENTATION,
        Buttons::OkCancel,
    ),
)
.with(
    2,
    Entry::setting("Theme", &icons::THEME, SETTING_THEME, Buttons::OkCancel),
)
.with(
    3,
    Entry::setting(
        "Brightness",
        &icons::BRIGHTNESS,
        SETTING_BRIGHTNESS,
        Buttons::OkCancel,
    ),
)
.with(
    4,
    Entry::setting(
        "Haptics",
        &icons::HAPTICS,
        SETTING_HAPTICS,
        Buttons::OkCancel,
    ),
)
.with(5, Entry::menu("Background", &icons::CLOUD, &BACKGROUND))
.with(6, Entry::menu("App", &icons::APP, &APP))
.with(
    SETTINGS_PLAYER_SLOT,
    Entry::menu("Music Player", &icons::MUSIC, &PLAYER),
)
.with(
    SETTINGS_SHARE_SLOT,
    Entry::setting(SHARE_NAME, &icons::CARD_WIFI, SETTING_SHARE, Buttons::None),
);

/// The phone app's settings: which phone the knob is paired with, and forgetting it. An ordinary
/// submenu, like the player's.
///
/// **Forgetting is done by hand, here and not from the app.** A phone that could unpair itself
/// could also be one that should not have paired in the first place.
const APP: Menu = Menu::new(
    "App",
    Entry::setting("About", &icons::ABOUT, SETTING_APP_ABOUT, Buttons::Ok),
)
.with(
    1,
    Entry::setting(
        "Forget phone",
        &icons::FORGET,
        SETTING_FORGET,
        Buttons::OkCancel,
    ),
);

/// The player's settings, left of About in the firmware's ring.
///
/// **The player has a menu like a plugin's**: About on top, its own entries clockwise. It is the
/// firmware's, so it is an ordinary submenu and not a
/// [`Kind::Plugin`](teetotum::menu::Kind::Plugin) one, and OK leads back up to the settings.
const PLAYER: Menu = Menu::new(
    "Music Player",
    Entry::setting("About", &icons::ABOUT, SETTING_PLAYER_ABOUT, Buttons::Ok),
)
.with(
    1,
    Entry::setting("Cover", &icons::COVER, SETTING_COVER, Buttons::OkCancel),
);

/// The background's settings: whether the cloud moves, and its shape as the four sliders of the
/// mockup it was chosen at. An ordinary submenu, like the player's.
const BACKGROUND: Menu = Menu::new(
    "Background",
    Entry::setting(
        "About",
        &icons::ABOUT,
        SETTING_BACKGROUND_ABOUT,
        Buttons::Ok,
    ),
)
.with(
    1,
    Entry::setting("Motion", &icons::MOTION, SETTING_MOTION, Buttons::OkCancel),
)
.with(
    2,
    Entry::setting("Points", &icons::POINTS, SETTING_POINTS, Buttons::OkCancel),
)
.with(
    3,
    Entry::setting(
        "Brightest",
        &icons::BRIGHTEST,
        SETTING_BRIGHTEST,
        Buttons::OkCancel,
    ),
)
.with(
    4,
    Entry::setting(
        "Dark centre",
        &icons::CENTRE,
        SETTING_CENTRE,
        Buttons::OkCancel,
    ),
)
.with(
    5,
    Entry::setting(
        "Icon colour",
        &icons::ACCENT,
        SETTING_ACCENT,
        Buttons::OkCancel,
    ),
);

/// What the firmware's dialogs are called when the menu reports on them.
const SETTING_ABOUT: Id = Id(0);
const SETTING_ORIENTATION: Id = Id(1);
const SETTING_THEME: Id = Id(2);
const SETTING_BRIGHTNESS: Id = Id(3);
const SETTING_HAPTICS: Id = Id(4);
const SETTING_PLAYER_ABOUT: Id = Id(5);
const SETTING_COVER: Id = Id(6);
const SETTING_MOTION: Id = Id(7);
const SETTING_BACKGROUND_ABOUT: Id = Id(8);
const SETTING_POINTS: Id = Id(9);
const SETTING_BRIGHTEST: Id = Id(10);
const SETTING_CENTRE: Id = Id(11);
const SETTING_ACCENT: Id = Id(12);
/// The first QR code's dialog; the others follow in the order of [`LINKS`].
const SETTING_QR: u16 = 13;
/// The install dialog, past the QR codes.
const SETTING_INSTALL: Id = Id(SETTING_QR + LINKS.len() as u16);
/// The receive dialog: while it is open, a plugin can be uploaded over BLE.
const SETTING_RECEIVE: Id = Id(SETTING_INSTALL.0 + 1);
/// What the card over Wi-Fi is called, in both rings and over its code.
const SHARE_NAME: &str = "Card over Wi-Fi";
/// The card over Wi-Fi: while it is open, the card can be read over the knob's access point.
const SETTING_SHARE: Id = Id(SETTING_RECEIVE.0 + 1);
/// The app's About: the bonded phone and whether it is connected.
const SETTING_APP_ABOUT: Id = Id(SETTING_SHARE.0 + 1);
/// Forgetting the bonded phone, on OK.
const SETTING_FORGET: Id = Id(SETTING_SHARE.0 + 2);

/// Where the install dialog stands: a menu of one entry, left by its buttons or a long press.
static INSTALL_MENU: Menu = Menu::new(
    "Install",
    Entry::setting(
        "Install",
        &icons::PLUGIN,
        SETTING_INSTALL,
        Buttons::OkCancel,
    ),
);

/// What a long press opens at home, said under the ring there.
const HOLD_FOR_QR: &str = "hold for QR codes";

/// The QR codes: a ring that a long press opens at home, one link a segment. Opening one fills
/// the disc with its code; a tap closes it, and the knob goes on to the next.
static QR_MENU: Menu = qr_menu();

const QR_ICONS: [&Icon; LINKS.len()] = [
    &qr::icons::CODE,
    &qr::icons::TERMINAL,
    &qr::icons::KNOB,
    &qr::icons::CHIP,
    &qr::icons::PAGE,
    &qr::icons::WASM,
    &qr::icons::BOOK,
    &qr::icons::CRAB,
    &qr::icons::BUG,
    &qr::icons::CHECKLIST,
    &qr::icons::INSTALL,
];

#[expect(
    clippy::large_stack_frames,
    reason = "a const fn evaluated at compile time for `QR_MENU`; it never runs on the device"
)]
const fn qr_menu() -> Menu {
    assert!(
        LINKS[0].slot == 0,
        "the first link stands at twelve o'clock"
    );
    let mut menu = Menu::new("QR codes", qr_entry(0));
    let mut n = 1;
    while n < LINKS.len() {
        menu = menu.with(LINKS[n].slot, qr_entry(n));
        n += 1;
    }
    menu
}

const fn qr_entry(n: usize) -> Entry {
    Entry::setting(
        LINKS[n].name,
        QR_ICONS[n],
        Id(SETTING_QR + n as u16),
        Buttons::None,
    )
}

/// Which link a dialog id shows, if it is a QR code's.
fn qr_index(id: Id) -> Option<usize> {
    (id.0 as usize)
        .checked_sub(SETTING_QR as usize)
        .filter(|&n| n < LINKS.len())
}

/// Whether the menu on the screen is home itself, with nothing open over it.
fn at_home(nav: &Navigator) -> bool {
    nav.depth() == 1 && nav.menu().is_home() && nav.opened().is_none()
}

/// What the home menu is called, on every one of its pages.
const HOME_TITLE: &str = "TeeToTum";

/// Where the first bundled plugin's settings stand in the ring, right after the firmware's own;
/// the others follow clockwise.
///
/// **A plugin's settings do not start it.** Which face is on the screen is chosen at home, and
/// the Face setting that once chose it here is gone.
const PLUGIN_SLOT: usize = 7;

/// Where the faces of the bundled plugins stand in the home menu: right of Home, the first one
/// next to it.
const HOME_PLUGIN_SLOT: usize = 1;
/// Where the player stands in the home menu: an hour before the card over Wi-Fi, on the
/// firmware's side of Home.
const HOME_PLAYER_SLOT: usize = 9;
/// Where the card over Wi-Fi stands in the home menu: between the player and the gear.
const HOME_SHARE_SLOT: usize = 10;
/// Where the player's menu stands in the settings: an hour before the card over Wi-Fi.
const SETTINGS_PLAYER_SLOT: usize = FIRMWARE_SLOT - 1;
/// Where the card over Wi-Fi stands in the settings: left of About, in the segment the gear has
/// at home and in a plugin's menu. The settings need no gear, being where it leads.
const SETTINGS_SHARE_SLOT: usize = FIRMWARE_SLOT;

/// What the screen shows when no menu is up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Face {
    /// The player, the firmware's own.
    #[default]
    Player,
    /// A plugin, by its place among those [`gather_modules`] found.
    Plugin(u8),
}

impl Face {
    /// What the home menu's entry for this face hands back in [`Outcome::Screen`].
    fn id(self) -> Id {
        match self {
            Face::Player => Id(0),
            Face::Plugin(n) => Id(u16::from(n) + 1),
        }
    }

    fn of(id: Id) -> Self {
        match id.0 {
            0 => Face::Player,
            n => Face::Plugin((n - 1) as u8),
        }
    }
}

/// The plugins that come with the firmware, each signed (`tools/teetotum-pack sign`).
///
/// **Three so far, taking turns in one page of external RAM** -- see [`start_plugin`]. The HID
/// remote (`plugins/hid-remote`), the teetotum (`plugins/teetotum-plugin`) and Nearby
/// (`plugins/nearby`), each built by its `build.sh`. They are embedded rather than read off the card, because the card sits inside the
/// housing and the FAT code only reads -- a plugin that ships is a plugin in the image, and
/// removing it takes its face off home, not its bytes out of flash.
///
/// **More come from the `plugins` partition**, without a firmware build: see [`gather_modules`]
/// and `tools/teetotum-pack pack`.
///
/// **The order decides only where each stands in the rings.** The settings record names removed
/// plugins by [`PluginId`]; records before version 11 named them by place here, so the first
/// three have to stay where they are for those to be read.
///
/// **Three fit either ring without paging**, so nothing here shows the pages off. To see them,
/// run `plugins/dummy/build.sh` and add its nine modules below: twelve plugins put both rings on
/// two pages.
const BUNDLED: [&[u8]; 3] = [
    include_bytes!("../../assets/plugins/hid-remote.wasm"),
    include_bytes!("../../assets/plugins/teetotum-plugin.wasm"),
    include_bytes!("../../assets/plugins/nearby.wasm"),
];

/// How many plugin faces the **first** page of the home ring holds: the segments between Home
/// and the player.
const FACES_ON_FIRST_HOME_PAGE: usize = HOME_PLAYER_SLOT - HOME_PLUGIN_SLOT;
/// And how many plugin menus the first page of the settings ring holds: the segments between the
/// firmware's own entries and the player's menu.
const PLUGINS_ON_FIRST_SETTINGS_PAGE: usize = SETTINGS_PLAYER_SLOT - PLUGIN_SLOT;

/// Where the plugins start on a page after the first, and how many fit there.
///
/// **A later page carries nothing the pages before it carry** except the top segment, so every
/// other segment is free -- and the plugins fill them from one o'clock round to eleven. It would
/// be simpler to give every page the first page's slots, and it would leave half of a second
/// ring empty.
const LATER_PAGE_SLOT: usize = 1;
const PLUGINS_PER_LATER_PAGE: usize = SLOTS - 1;

/// Which page bundled plugin `n` stands on and in which segment, in a ring whose first page
/// holds `first` of them from segment `slot` on.
const fn place(n: usize, first: usize, slot: usize) -> (usize, usize) {
    if n < first {
        (0, slot + n)
    } else {
        let past = n - first;
        (
            1 + past / PLUGINS_PER_LATER_PAGE,
            LATER_PAGE_SLOT + past % PLUGINS_PER_LATER_PAGE,
        )
    }
}

/// How many pages such a ring needs for `count` plugins; one page even for none.
const fn pages_for(count: usize, first: usize) -> usize {
    if count <= first {
        1
    } else {
        1 + (count - first).div_ceil(PLUGINS_PER_LATER_PAGE)
    }
}

// Both rings need room for at least one plugin on their first page, or the first page would hand
// its overflow on without having taken any.
const _: () = assert!(FACES_ON_FIRST_HOME_PAGE > 0 && PLUGINS_ON_FIRST_SETTINGS_PAGE > 0);
// And the pages have to fit what `Navigator::hide` can address and what the record can count.
const _: () = assert!(pages_for(PLUGINS_MAX, FACES_ON_FIRST_HOME_PAGE) <= MAX_PAGES);
const _: () = assert!(pages_for(PLUGINS_MAX + 1, PLUGINS_ON_FIRST_SETTINGS_PAGE) <= MAX_PAGES);
const _: () = assert!(BUNDLED.len() <= PLUGINS_MAX);

/// How many plugins the rings hold, bundled and installed together: as many as the settings
/// record can name.
const PLUGINS_MAX: usize = Settings::PLUGINS_MAX;

/// Each plugin's module, by its place in the rings; `None` past the last.
type Modules = [Option<&'static [u8]>; PLUGINS_MAX];
/// The slot each plugin came from; `None` for a bundled one.
type FromSlot = [Option<usize>; PLUGINS_MAX];

/// Each bundled plugin's id; `None` for one without a readable manifest or signature section.
fn bundled_ids() -> [Option<PluginId>; BUNDLED.len()] {
    core::array::from_fn(|n| PluginId::of(BUNDLED[n]).ok())
}

/// A plugin in a slot that waits to be accepted: which slot, and its module in external RAM.
#[derive(Clone, Copy)]
struct Waiting {
    slot: usize,
    wasm: &'static [u8],
    /// Whether a plugin with the same id is there already, bundled or from a slot.
    update: bool,
}

type WaitingSlots = [Option<Waiting>; PLUGINS_MAX];

/// The bundled plugins, then those accepted in the slots of the `plugins` partition, copied
/// into external RAM so that each is a `'static` module like the bundled ones. Also how many
/// there are, the slots that wait for the install dialog, what is left of `spare`, and how many
/// slots hold nothing -- `None` if the slots were not read.
///
/// **A slot with a bundled plugin's id takes that plugin's place**: same key and name are the
/// same plugin, so a new build of it replaces the one in the image without a firmware build. A
/// second slot with an id already taken from a slot is skipped. An accepted plugin's signature is
/// checked when it is loaded, as for the bundled ones; a waiting one's here, so that the dialog
/// never offers a plugin whose signature fails.
///
/// Runs before the heap exists, so nothing here allocates. The modules take at most half of
/// `spare`; the page, the ring and the cover come off the rest.
#[expect(
    clippy::large_stack_frames,
    reason = "runs once at boot and returns the module tables by value; the main stack has room, see the note at the top"
)]
fn gather_modules(
    region: Option<Region<'_, '_>>,
    spare: Option<&'static mut [u8]>,
) -> (
    Modules,
    usize,
    WaitingSlots,
    FromSlot,
    Option<&'static mut [u8]>,
    Option<usize>,
) {
    let mut modules: Modules = [None; PLUGINS_MAX];
    let mut from_slot: FromSlot = [None; PLUGINS_MAX];
    let mut ids = [None; PLUGINS_MAX];
    let mut waiting: WaitingSlots = [None; PLUGINS_MAX];
    let mut waits = 0;
    for (n, wasm) in BUNDLED.iter().enumerate() {
        modules[n] = Some(*wasm);
    }
    ids[..BUNDLED.len()].copy_from_slice(&bundled_ids());
    let mut count = BUNDLED.len();
    let Some(region) = region else {
        return (modules, count, waiting, from_slot, spare, None);
    };
    let Some(mut spare) = spare else {
        warn!("Plugin: no external RAM, only the bundled plugins");
        return (modules, count, waiting, from_slot, None, None);
    };
    let floor = spare.len() / 2;
    let mut slots = Slots::new(region);
    let mut free = 0;
    for n in 0..slots.count() {
        let header = match slots.header(n) {
            Ok(Some(header)) => header,
            Ok(None) => {
                free += 1;
                continue;
            }
            Err(e) => {
                error!("Plugin: slot {n} unreadable -- {e:?}");
                continue;
            }
        };
        // Where an accepted plugin goes in the rings; `None` for one that waits.
        let at = if !header.accepted {
            if waits == PLUGINS_MAX {
                warn!("Plugin: slot {n} skipped, {PLUGINS_MAX} slots wait already");
                continue;
            }
            None
        } else {
            match ids[..count].iter().position(|id| *id == Some(header.id)) {
                Some(k) if from_slot[k].is_some() => {
                    warn!("Plugin: slot {n} skipped, another slot holds the same plugin");
                    continue;
                }
                Some(k) => Some(k),
                None if count == PLUGINS_MAX => {
                    warn!("Plugin: slot {n} skipped, the rings hold {PLUGINS_MAX} plugins");
                    continue;
                }
                None => Some(count),
            }
        };
        if spare.len() < floor + header.len {
            warn!("Plugin: slot {n} skipped, no external RAM left for it");
            continue;
        }
        match slots.read(n, &mut spare[..header.len]) {
            Ok(Some(_)) => {
                let (module, rest) = core::mem::take(&mut spare).split_at_mut(header.len);
                spare = rest;
                let module: &'static [u8] = module;
                let Some(at) = at else {
                    match plugin::verify(module) {
                        Ok(()) => {
                            waiting[waits] = Some(Waiting {
                                slot: n,
                                wasm: module,
                                update: false,
                            });
                            waits += 1;
                            info!(
                                "Plugin: slot {n}, {} bytes, id {:02x?}, waits to be accepted",
                                header.len,
                                header.id.bytes()
                            );
                        }
                        Err(e) => error!("Plugin: slot {n} not offered -- {e}"),
                    }
                    continue;
                };
                modules[at] = Some(module);
                ids[at] = Some(header.id);
                from_slot[at] = Some(n);
                if at == count {
                    count += 1;
                }
                info!(
                    "Plugin: slot {n}, {} bytes, id {:02x?}, in place {at}",
                    header.len,
                    header.id.bytes()
                );
            }
            Ok(None) => {}
            Err(e) => error!("Plugin: slot {n} refused -- {e:?}"),
        }
    }
    // Only now are all plugins known that a waiting one can update.
    for waits in waiting.iter_mut().flatten() {
        waits.update = PluginId::of(waits.wasm).is_ok_and(|id| ids[..count].contains(&Some(id)));
    }
    (modules, count, waiting, from_slot, Some(spare), Some(free))
}

/// What the firmware puts into a plugin's menu for it, while faces cannot bring entries of their
/// own: its About, drawn from the manifest, and whether it is installed.
///
/// **The id says whose they are.** [`Owner::Plugin`] only says that a setting is some plugin's,
/// so bundled plugin `n` gets ids `2n` and `2n + 1`. The first plugin's About is therefore
/// `Id(0)` like the firmware's, and [`Owner`] keeps the two apart, which is what the mock plugin
/// these replaced was there to show. Once a face can bring entries of its own, these need an
/// owner of their own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PluginSetting {
    About = 0,
    Installed = 1,
}

impl PluginSetting {
    fn id(self, n: usize) -> Id {
        Id(2 * n as u16 + self as u16)
    }

    /// Which bundled plugin a setting in a plugin's menu belongs to, and which of the two it is.
    fn of(id: Id) -> Option<(usize, Self)> {
        let n = usize::from(id.0 / 2);
        let setting = if id.0.is_multiple_of(2) {
            Self::About
        } else {
            Self::Installed
        };
        (n < PLUGINS_MAX).then_some((n, setting))
    }
}

/// Bundled plugin `n`'s menu. The gear left of About leads back, as in every plugin's menu.
fn plugin_menu(name: &'static str, n: usize) -> Menu {
    Menu::plugin(
        name,
        Entry::setting(
            "About",
            &icons::ABOUT,
            PluginSetting::About.id(n),
            Buttons::Ok,
        ),
    )
    .with(
        1,
        Entry::setting(
            "Installed",
            &icons::PLUGIN,
            PluginSetting::Installed.id(n),
            Buttons::OkCancel,
        ),
    )
}

/// How many detents of one pass reach a face with the knob right. A turn is a detent or two per
/// pass; more than this is a backlog, and a face that stepped through all of it would run on
/// after the hand stopped.
const KNOB_EVENTS_MAX: u32 = 8;

/// The shortest gap between two volume keys handed to the other chip.
///
/// **`A3 03` does not go into a queue over there.** `0x400dbd90` is an `xTaskNotify` with
/// `eSetValueWithOverwrite`, so of two commands arriving before its task looks, the first is
/// overwritten and lost without a trace. The chip then spends two of its own 100 Hz ticks on
/// each key, press and release -- 20 ms.
///
/// **Neither 50 nor 120 ms was enough.** Four keys 50 ms apart moved the
/// phone by 10 of the 20 they asked for; six keys 120 ms apart moved it by 15 of 30. The same
/// half went missing at more than twice the spacing, which is a rate the chip will not exceed
/// rather than one key overwriting the next -- its volume pair is the guarded one, and the
/// guard plausibly waits for the phone to answer. Nothing is reported for a dropped key: the
/// volume the chip states afterwards is the only witness there is.
///
/// This path is not the one in use -- see [`KNOB_VOLUME_AT_CHIP`] -- and is kept because it is
/// the one a plugin borrows the knob through.
const VOLUME_KEY_PERIOD: Duration = Duration::from_millis(120);

/// How many detents may still be owed to the phone before the rest are dropped.
///
/// One key moves the phone by 5 of 127, so ten of them is most of the range. The reasoning was
/// that a control which runs on after its input is worse than one that saturates: at
/// [`VOLUME_KEY_PERIOD`] a full turn's thirty detents would keep the volume climbing for
/// another second after the hand stopped.
///
/// **It does not bind, and it was never what limited the turn.** Nothing
/// was dropped here even at a full revolution's speed -- the halving happened twice further
/// down the path, at the polled encoder and at the other chip's own guard.
const VOLUME_PENDING_MAX: i32 = 10;

/// How long after the last volume key the other chip is asked where it ended up.
///
/// The number on the status screen is the chip's own, out of `A3 08`, which is otherwise asked
/// every [`STATUS_PERIOD`]. Two seconds is not an answer to a turn.
const VOLUME_SETTLE: Duration = Duration::from_millis(150);

/// How far from the middle the player's text may reach: inside the volume arc, whose stroke
/// begins at 166.
const TEXT_RADIUS: i32 = 156;

/// What stands after a line that is cut. Three full stops rather than U+2026: the Latin-1 cuts
/// end at 255, and a character the font does not have is left out without a word.
const ELLIPSIS: &str = "...";

/// One of the phone's two lines on the player.
struct PlayerLine {
    /// How far below the middle the line stands. Both are below it, so that the top half of the
    /// cover stays whole.
    below: i32,
    font: &'static fonts::FontRenderer,
    colour: Rgb565,
}

const TITLE_LINE: PlayerLine = PlayerLine {
    below: 52,
    font: &fonts::BODY_LATIN1,
    colour: Rgb565::WHITE,
};

const ARTIST_LINE: PlayerLine = PlayerLine {
    below: 76,
    font: &fonts::SMALL_LATIN1,
    colour: Rgb565::CSS_LIGHT_GRAY,
};

impl PlayerLine {
    /// The part of the line's row inside [`TEXT_RADIUS`], as left edge and width.
    ///
    /// Measured at the edge of the text farther from the middle, where the circle is narrowest,
    /// so the two lines get different widths. Both are logged at boot.
    fn room(&self) -> (i32, i32) {
        let half = (i32::from(self.font.get_ascent()) - i32::from(self.font.get_descent())) / 2;
        let far = self.below + half;
        let across = (TEXT_RADIUS * TEXT_RADIUS - far * far).max(0).isqrt();
        (WIDTH as i32 / 2 - across, 2 * across)
    }

    /// How much wider than its room `text` is, or 0 if it fits.
    fn overflow(&self, text: &str) -> i32 {
        (menu_width(text, self.font) - self.room().1).max(0)
    }

    /// Draws `text` centred if it fits. If it does not, it is shifted left by `run` and clipped
    /// to its room while the run lasts, and cut with an [`ELLIPSIS`] once it is over.
    fn draw(&self, frame: &mut Framebuffer, text: &str, run: Option<i32>) {
        let (left, room) = self.room();
        let y = HEIGHT as i32 / 2 + self.below;
        let centred = Point::new(WIDTH as i32 / 2, y);
        let _ = match (run, shortened(text, self.font, room, ELLIPSIS)) {
            (_, None) => menu_text(frame, text, centred, self.font, self.colour),
            (Some(offset), Some(_)) => {
                let width = menu_width(text, self.font);
                let offset = offset.min(width - room);
                // Only the sides are clipped; nothing else is drawn into this line's rows.
                let window =
                    Rectangle::new(Point::new(left, 0), Size::new(room as u32, HEIGHT as u32));
                let at = Point::new(left - offset + width / 2, y);
                menu_text(
                    &mut frame.clipped(&window),
                    text,
                    at,
                    self.font,
                    self.colour,
                )
            }
            (None, Some(start)) => {
                let cut = format!("{start}{ELLIPSIS}");
                menu_text(frame, &cut, centred, self.font, self.colour)
            }
        };
    }
}

/// How often a running line moves, and by how much: 40 px a second.
///
/// **One run after each change of track, not a ticker.** `present()`
/// sends whole pictures only, and one costs 40 ms at a MADCTL angle and 65 ms between them with
/// a cover behind it. Ten a second would be 40 to 65 % of the loop, which is fine for a few
/// seconds and not for as long as a track plays.
const RUN_PERIOD: Duration = Duration::from_millis(100);
const RUN_STEP: i32 = 4;
/// How many periods a run stands still at its start, and again at its end, so that both can be
/// read: 1.5 s.
const RUN_HOLD: i32 = 15;

/// The one run of the player's long lines, after each change of track and each time the player
/// comes back on the screen.
///
/// **Time counts only while the player is on the screen and the loop gets round.** A pass that a
/// cover transfer or a decode held up counts one period at most, so the run waits for the
/// picture instead of jumping ahead of it.
#[derive(Default)]
struct Run {
    elapsed: Duration,
    /// How far the longer of the two lines overhangs its room, in pixels. 0 means no run.
    overflow: i32,
}

impl Run {
    fn new(state: &Overview) -> Self {
        let overflow = match state.title.is_empty() {
            // Without a title the player names the device instead, and that fits.
            true => 0,
            false => TITLE_LINE
                .overflow(&state.title)
                .max(ARTIST_LINE.overflow(&state.artist)),
        };
        Self {
            elapsed: Duration::from_ticks(0),
            overflow,
        }
    }

    /// How far the lines stand shifted, or `None` when there is no run or it is over. Each line
    /// stops where its own end shows, the shorter one first.
    fn offset(&self) -> Option<i32> {
        if self.overflow == 0 {
            return None;
        }
        let periods = (self.elapsed.as_millis() / RUN_PERIOD.as_millis()) as i32;
        let moved = (periods - RUN_HOLD).max(0) * RUN_STEP;
        match moved >= self.overflow + RUN_HOLD * RUN_STEP {
            true => None,
            false => Some(moved.min(self.overflow)),
        }
    }
}

#[expect(
    clippy::large_stack_frames,
    reason = "fires in the `Clone` and `PartialEq` that `derive` generates; the main stack has room, see the note at the top"
)]
mod overview {
    use super::*;

    /// What the status screen knows, gathered from all five sources.
    ///
    /// Kept apart from the drawing so that redrawing is a pure function of it: the picture is only
    /// sent to the screen when one of these fields actually moved.
    #[derive(Clone, Default, PartialEq, Eq)]
    pub(super) struct Overview {
        /// Where the user is in the menus while they are up -- home after boot -- and `None` while a
        /// face is on the screen.
        pub(super) menu: Option<Navigator>,
        /// Seconds since boot.
        pub(super) uptime: u32,
        /// Networks found in the most recent Wi-Fi scan.
        pub(super) networks: u8,
        /// Whether a BLE peer is connected right now.
        pub(super) peer: bool,
        /// The identity address of the one bonded peer, as the settings keep it.
        pub(super) bonded: Option<[u8; 6]>,
        /// Detents counted since boot, signed -- clockwise is positive.
        pub(super) detents: i32,
        /// How many quarter turns clockwise the picture stands at.
        pub(super) orientation: usize,
        /// Which colours the settings are drawn in.
        pub(super) theme: Theme,
        /// How bright the screen is.
        pub(super) brightness: Brightness,
        /// How hard the motor clicks.
        pub(super) haptics: Haptics,
        /// The other chip's volume, 0-127, or `None` while it does not answer.
        pub(super) volume: Option<u8>,
        /// Whether the other chip says its own encoder reporting is switched on.
        pub(super) companion_encoder: bool,
        /// Whether audio streams to the knob, which is when the other chip takes volume steps.
        pub(super) streaming: bool,
        /// Whether a phone is connected over BLE HID, as the other chip says.
        pub(super) hid: bool,
        /// How big the cover stands behind the player.
        pub(super) cover: CoverStyle,
        /// Whether the cloud moves.
        pub(super) motion: Motion,
        /// How the cloud looks.
        pub(super) shape: CloudShape,
        /// The track the other chip last named.
        pub(super) title: String,
        pub(super) artist: String,
        /// How far the long lines stand shifted while their one run lasts, from [`Run::offset`].
        pub(super) run: Option<i32>,
        /// Which frame of the moving cloud is up, counted in [`CLOUD_FRAME`]s since boot, or 0
        /// while it stands still or is not on the screen. A new frame is a change like any other,
        /// so it is what makes the moving cloud redraw.
        pub(super) cloud: u32,
        /// The picture currently laid behind the text, if there is one.
        pub(super) backdrop: Option<Backdrop>,
        /// What the screen shows when no menu is up. [`Face::Plugin`] only while that plugin
        /// is installed; if loading it was refused, its face says why.
        pub(super) face: Face,
        /// The plugins as the settings show them, each `None` if its manifest could not be read,
        /// and `None` past the last.
        pub(super) plugins: [Option<PluginView>; PLUGINS_MAX],
        /// Slots of the `plugins` partition that hold nothing, as read at boot. An upload fills
        /// one and restarts, so the count stays true.
        pub(super) free_slots: Option<usize>,
        /// The plugin the install dialog offers, while it is open.
        pub(super) offer: Option<Offer>,
        /// How the upload stands that the receive dialog shows.
        pub(super) received: Received,
        /// The card's size as the share entry states it, `None` without a card.
        pub(super) card: Option<String>,
        /// Whether the share dialog shows its network in words instead of as a code.
        pub(super) share_text: bool,
        /// Counted up whenever the plugin asks to be drawn again. What it shows lives in its own
        /// memory, where the comparison that decides a redraw cannot look, so this stands in for it.
        pub(super) plugin_frame: u32,
    }
}

use overview::Overview;

/// How an upload over BLE stands, as the receive dialog shows it.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Received {
    status: UploadStatus,
    slot: usize,
    received: usize,
    total: usize,
}

impl Received {
    fn of(status: UploadStatus, upload: &Upload) -> Self {
        Self {
            status,
            slot: upload.slot(),
            received: upload.received(),
            total: upload.total(),
        }
    }
}

/// The plugin list a sender reads over BLE, encoded as the device loop last filled it.
struct Listing {
    count: u8,
    entries: [[u8; upload::ENTRY]; PLUGINS_MAX],
}

/// The device loop fills it, the advertising loop answers from it and never reads flash. A
/// static, because the table is larger than a stack frame may be.
static PLUGIN_LIST: Mutex<RefCell<Listing>> = Mutex::new(RefCell::new(Listing {
    count: 0,
    entries: [[0; upload::ENTRY]; PLUGINS_MAX],
}));

impl Listing {
    /// The entry characteristic's bytes for `index`.
    fn entry(&self, index: u8) -> [u8; upload::ENTRY] {
        match self.entries.get(usize::from(index)) {
            Some(raw) if index < self.count => *raw,
            _ => upload::listing::past_end(index, self.count),
        }
    }

    /// The plugins the settings show, in their order, installed as `kept` says.
    fn fill(&mut self, plugins: &[Option<PluginView>], kept: &Settings) {
        self.count = plugins.iter().flatten().count() as u8;
        for (k, view) in plugins.iter().flatten().enumerate() {
            let entry = upload::listing::Entry {
                slot: view.slot.map(|n| n as u8),
                installed: kept.installed(view.id),
                id: view.id,
                len: view.bytes as u32,
                version: view.version,
                name: view.name,
                summary: view.summary,
            };
            self.entries[k] = entry.encode(k as u8, self.count);
        }
    }
}

/// What the settings know about the bundled plugin.
#[derive(Clone, PartialEq, Eq)]
struct PluginView {
    /// What the settings record names it by.
    id: PluginId,
    name: &'static str,
    /// The line from its manifest that home shows under its name.
    summary: &'static str,
    bytes: usize,
    version: Version,
    rights: Rights,
    /// The slot it was installed from; `None` for a bundled plugin.
    slot: Option<usize>,
    /// Installed as the dialog stands -- which becomes what is loaded only at OK.
    installed: bool,
    /// What loading cost, while it is loaded: microseconds, and bytes of internal heap.
    loaded: Option<(u64, usize)>,
    /// Why it was stopped, if it was.
    fault: Option<String>,
}

/// What the install dialog shows of a plugin waiting in a slot, all of it read from the module
/// without running any of it.
#[derive(Clone, PartialEq, Eq)]
struct Offer {
    slot: usize,
    id: PluginId,
    /// Whether it replaces a plugin with the same id once accepted.
    update: bool,
    name: &'static str,
    version: Version,
    /// The first bytes of the author's key, which tell two authors apart on the screen.
    key: [u8; 8],
    /// Whether a bundled plugin is signed with the same key.
    known: bool,
    rights: Rights,
    bytes: usize,
    /// What loading it would take of the heap, by [`plugin::heap_needed`], and what is free.
    heap: usize,
    free: usize,
}

impl Offer {
    /// `None`, with the reason logged, if the module's manifest or signature section cannot be
    /// read.
    fn of(waiting: Waiting) -> Option<Self> {
        let wasm = waiting.wasm;
        let (signed, manifest, id) = Signed::read(wasm)
            .and_then(|signed| Ok((signed, Manifest::read(wasm)?, PluginId::of(wasm)?)))
            .inspect_err(|e| error!("Plugin: slot {} not offered -- {e}", waiting.slot))
            .ok()?;
        let mut key = [0; 8];
        key.copy_from_slice(&signed.key[..8]);
        Some(Self {
            slot: waiting.slot,
            id,
            update: waiting.update,
            name: manifest.name(),
            version: manifest.version(),
            key,
            known: BUNDLED
                .iter()
                .any(|bundled| Signed::read(bundled).is_ok_and(|b| b.key == signed.key)),
            rights: manifest.rights(),
            bytes: wasm.len(),
            heap: plugin::heap_needed(wasm.len()),
            free: esp_alloc::HEAP.free(),
        })
    }
}

/// The plugins waiting in slots, offered one after another at boot, and how many were accepted.
struct Offers {
    waiting: WaitingSlots,
    next: usize,
    accepted: usize,
}

impl Offers {
    fn new(waiting: WaitingSlots) -> Self {
        Self {
            waiting,
            next: 0,
            accepted: 0,
        }
    }

    /// The next waiting plugin that can be offered.
    fn next(&mut self) -> Option<Offer> {
        while let Some(&waiting) = self.waiting.get(self.next) {
            self.next += 1;
            if let Some(offer) = waiting.and_then(Offer::of) {
                return Some(offer);
            }
        }
        None
    }
}

/// Opens the install dialog on the next plugin waiting in a slot. With none left, a plugin
/// accepted on the way takes a restart to get its place in the rings; otherwise it is home.
fn offer_next(
    state: &mut Overview,
    offers: &mut Offers,
    home_menu: &'static Menu,
    settings_menu: &'static Menu,
) {
    state.offer = offers.next();
    if let Some(offer) = &state.offer {
        info!("Plugin: slot {} offered for install", offer.slot);
        state
            .menu
            .insert(Navigator::firmware(&INSTALL_MENU))
            .open_selected();
    } else if offers.accepted > 0 {
        info!("Plugin: {} accepted -- restarting", offers.accepted);
        software_reset();
    } else {
        let _ = state.menu.insert(home(home_menu, settings_menu));
    }
}

/// Erases the other slots that hold an accepted plugin with slot `n`'s id. An update is written
/// into a slot of its own, and the plugin it replaces goes only once the update is accepted.
fn replace_older<F: NorFlash>(slots: &mut Slots<F>, n: usize) {
    let Ok(Some(new)) = slots.header(n) else {
        return;
    };
    for m in (0..slots.count()).filter(|&m| m != n) {
        if let Ok(Some(old)) = slots.header(m)
            && old.accepted
            && old.id == new.id
        {
            match slots.erase(m) {
                Ok(()) => info!("Plugin: slot {m} erased, slot {n} replaces it"),
                Err(e) => error!("Plugin: slot {m} not erased -- {e:?}"),
            }
        }
    }
}

/// Takes one command of an upload over BLE, through the flash the settings store holds, and
/// says how the upload stands after it.
#[expect(
    clippy::large_stack_frames,
    reason = "a piece and the hash under way are a few hundred bytes each; the main stack has room, see the note at the top"
)]
fn receive(
    store: Option<&mut Store<Region<'_, '_>>>,
    table: &mut [u8; TABLE_SCRATCH],
    upload: &mut Option<Upload>,
    command: Command,
) -> Received {
    let failed = |status| Received {
        status,
        ..Received::default()
    };
    let Some(store) = store else {
        error!("Plugin: upload refused, no flash to write to");
        return failed(UploadStatus::Flash);
    };
    let Ok(region) = flash::plugins(store.flash_mut().storage(), table) else {
        error!("Plugin: upload refused, no plugins partition");
        return failed(UploadStatus::Flash);
    };
    let mut slots = Slots::new(region);
    match command {
        Command::Begin(header) => {
            // A second begin replaces the upload under way, whose slot stays empty.
            *upload = None;
            let n = match slots.free() {
                Ok(Some(n)) => n,
                Ok(None) => {
                    warn!("Plugin: upload refused, every slot holds a plugin");
                    return failed(UploadStatus::NoSlot);
                }
                Err(e) => {
                    error!("Plugin: upload refused -- {e:?}");
                    return failed(UploadStatus::Flash);
                }
            };
            let len = header.len;
            match slots.begin(n, header) {
                Ok(begun) => {
                    info!("Plugin: receiving {len} bytes into slot {n}");
                    let received = Received::of(UploadStatus::Ready, &begun);
                    *upload = Some(begun);
                    received
                }
                Err(slots::Error::Module(e)) => {
                    warn!("Plugin: upload refused -- {e}");
                    failed(UploadStatus::Mismatch)
                }
                Err(e) => {
                    error!("Plugin: upload refused -- {e:?}");
                    failed(UploadStatus::Flash)
                }
            }
        }
        Command::Piece { offset, len, bytes } => {
            let Some(current) = upload.as_mut() else {
                return failed(UploadStatus::Refused);
            };
            if offset != current.received() {
                warn!(
                    "Plugin: piece at {offset} refused, {} bytes received",
                    current.received()
                );
                return Received::of(UploadStatus::OutOfOrder, current);
            }
            match slots.feed(current, &bytes[..len]) {
                Ok(()) => Received::of(UploadStatus::Ready, current),
                Err(e) => {
                    error!(
                        "Plugin: upload into slot {} failed -- {e:?}",
                        current.slot()
                    );
                    let status = match e {
                        slots::Error::Module(_) => UploadStatus::Mismatch,
                        _ => UploadStatus::Flash,
                    };
                    let received = Received::of(status, current);
                    *upload = None;
                    received
                }
            }
        }
        Command::Commit => {
            let Some(done) = upload.take() else {
                return failed(UploadStatus::Refused);
            };
            let last = Received::of(UploadStatus::Written, &done);
            match slots.finish(done) {
                Ok(header) => {
                    info!(
                        "Plugin: slot {} written, {} bytes, id {:02x?}",
                        last.slot,
                        header.len,
                        header.id.bytes()
                    );
                    last
                }
                Err(e) => {
                    warn!("Plugin: slot {} not written -- {e:?}", last.slot);
                    let status = match e {
                        slots::Error::Module(_) => UploadStatus::Mismatch,
                        _ => UploadStatus::Flash,
                    };
                    Received { status, ..last }
                }
            }
        }
        Command::Abort => {
            if let Some(dropped) = upload.take() {
                info!("Plugin: upload into slot {} aborted", dropped.slot());
            }
            Received::default()
        }
        // Deleting changes the settings, which the device loop holds; see [`delete_slot`].
        Command::Delete(_) => failed(UploadStatus::Refused),
    }
}

/// Erases the slot of a plugin in the list, through the flash the settings store holds; the id
/// of the plugin it held, or why not.
fn delete_slot(
    store: Option<&mut Store<Region<'_, '_>>>,
    table: &mut [u8; TABLE_SCRATCH],
    plugins: &[Option<PluginView>],
    slot: usize,
) -> Result<PluginId, UploadStatus> {
    let Some(view) = plugins
        .iter()
        .flatten()
        .find(|view| view.slot == Some(slot))
    else {
        warn!("Plugin: delete refused, no plugin in slot {slot}");
        return Err(UploadStatus::NoPlugin);
    };
    let Some(store) = store else {
        error!("Plugin: delete refused, no flash to write to");
        return Err(UploadStatus::Flash);
    };
    let Ok(region) = flash::plugins(store.flash_mut().storage(), table) else {
        error!("Plugin: delete refused, no plugins partition");
        return Err(UploadStatus::Flash);
    };
    match Slots::new(region).erase(slot) {
        Ok(()) => {
            info!("Plugin: slot {slot} erased, {} deleted", view.name);
            Ok(view.id)
        }
        Err(e) => {
            error!("Plugin: slot {slot} not erased -- {e:?}");
            Err(UploadStatus::Flash)
        }
    }
}

/// Marks a waiting slot accepted, through the flash the settings store holds; says whether it
/// took.
fn accept_slot(
    store: Option<&mut Store<Region<'_, '_>>>,
    table: &mut [u8; TABLE_SCRATCH],
    slot: usize,
) -> bool {
    let Some(store) = store else {
        error!("Plugin: slot {slot} not accepted, no flash to write to");
        return false;
    };
    let accepted = match flash::plugins(store.flash_mut().storage(), table) {
        Ok(region) => {
            let mut slots = Slots::new(region);
            slots
                .accept(slot)
                .inspect(|()| replace_older(&mut slots, slot))
        }
        Err(_) => Err(slots::Error::NoSlot),
    };
    match accepted {
        Ok(()) => {
            info!("Plugin: slot {slot} accepted");
            true
        }
        Err(e) => {
            error!("Plugin: slot {slot} not accepted -- {e:?}");
            false
        }
    }
}

/// What the picture behind the text is.
///
/// **One backdrop, two sources, and the newest wins.** A swipe puts up a photograph off the
/// card, a track change puts up the cover of what is playing; both are the same 253 KiB of
/// external RAM (see [`Screen::stash`]), so keeping both would cost a third screen and the
/// choice between them would still have to be made somewhere. Which one is up is named on the
/// status screen, because from the screen a photograph and an album sleeve look alike.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Backdrop {
    /// A full-screen background read off the card, by file name.
    Card(String),
    /// The cover art of the track the other chip is playing, at the size it arrived in.
    Cover { width: usize, height: usize },
}

/// The GATT server and its one service, in a module of their own so that the lints on the code
/// their macros generate are expected here rather than for the whole binary.
#[expect(
    clippy::needless_borrows_for_generic_args,
    clippy::large_stack_frames,
    reason = "both fire in the code `gatt_server` and `gatt_service` generate"
)]
mod gatt {
    use super::*;

    /// The GATT server the knob presents.
    ///
    /// Without one, a peer discovers no services, and the specification then requires it to drop the
    /// connection after the 30 s ATT transaction timeout -- which is exactly what was measured before
    /// this existed.
    #[gatt_server]
    pub(super) struct Server {
        pub(super) knob: KnobService,
        pub(super) upload: UploadService,
    }

    /// Where a plugin is uploaded into a slot, and the plugins are listed and deleted; the protocol
    /// is [`upload`]'s. Commands and pieces need an encrypted link, so only the bonded peer changes
    /// the card; the list stays readable.
    #[gatt_service(uuid = "4a729af2-063c-451a-8c73-60e5fab61ccb")]
    pub(super) struct UploadService {
        /// A command: begin with a slot header, commit, abort, or delete a slot.
        #[characteristic(
            uuid = "19792d5c-9458-40ba-b233-c82b87d3dd4e",
            write,
            permissions(write = encrypted),
            value = [0; upload::CONTROL_MAX]
        )]
        pub(super) control: [u8; upload::CONTROL_MAX],
        /// A piece of the module, after its offset.
        #[characteristic(
            uuid = "81bcd10c-d2eb-4f6a-b4db-e196026f9f7c",
            write,
            permissions(write = encrypted),
            value = [0; upload::DATA_MAX]
        )]
        pub(super) data: [u8; upload::DATA_MAX],
        /// How the upload stands, from [`upload::Status::encode`].
        #[characteristic(uuid = "1236b81e-8a6e-49bf-817a-210b74ac7990", read, notify)]
        pub(super) status: [u8; upload::STATUS_LEN],
        /// Which entry of the plugin list `entry` holds.
        #[characteristic(uuid = "8d86b41e-f676-4ba8-896f-0d3baee6bae3", write)]
        pub(super) select: u8,
        /// The plugin list's entry at the selected index, from [`upload::listing`].
        #[characteristic(
            uuid = "2e229d9b-b681-4220-85ff-eb82a309ebcf",
            read,
            value = [0; upload::ENTRY]
        )]
        pub(super) entry: [u8; upload::ENTRY],
    }

    /// A service exposing what the firmware currently knows about itself.
    #[gatt_service(uuid = "aa7154b7-6b8f-4c5b-aa39-a6cd78bad6bb")]
    pub(super) struct KnobService {
        /// Seconds since boot.
        #[characteristic(uuid = "4b22a5ab-422b-4c76-a176-d7be74ec9bb7", read, notify)]
        pub(super) uptime: u32,
        /// Wi-Fi networks found in the most recent scan.
        #[characteristic(uuid = "4ea309d6-ee6a-4be8-b753-1925723a2e15", read, notify)]
        pub(super) networks: u8,
        /// The firmware's version, as [`VERSION`] spells it.
        #[characteristic(uuid = "3a298945-67fa-444e-9739-e0698bc95ca9", read)]
        pub(super) version: HeaplessString<32>,
        /// The card's size in bytes, or zero without a card.
        #[characteristic(uuid = "5bd092f3-61c8-4fc5-b755-f19328dd0172", read)]
        pub(super) card_bytes: u64,
        /// Theme, brightness, clicks and orientation, from [`teetotum_pack::settings`]. Anyone
        /// reads them; only the bonded peer changes them.
        #[characteristic(
            uuid = "dcec6510-5a6a-41c3-b83f-a638357034ff",
            read,
            write,
            notify,
            permissions(read, write = encrypted, cccd),
            value = [0; teetotum_pack::settings::LEN]
        )]
        pub(super) settings: [u8; teetotum_pack::settings::LEN],
    }
}

use gatt::Server;

/// One advertiser seen during a scan window.
struct SeenDevice {
    addr: [u8; 6],
    rssi: i8,
    name: Option<String>,
}

/// Collects advertising reports for the scan loop to summarise.
///
/// `on_adv_reports` is called from the BLE runner rather than from the scanning task, so the
/// list behind it is shared state and needs a critical section.
struct BleScanLog {
    seen: Mutex<RefCell<Vec<SeenDevice>>>,
}

impl BleScanLog {
    const fn new() -> Self {
        Self {
            seen: Mutex::new(RefCell::new(Vec::new())),
        }
    }

    /// Empties the list and returns what was in it.
    fn take(&self) -> Vec<SeenDevice> {
        critical_section::with(|cs| core::mem::take(&mut *self.seen.borrow_ref_mut(cs)))
    }
}

impl EventHandler for BleScanLog {
    fn on_adv_reports(&self, reports: LeAdvReportsIter) {
        critical_section::with(|cs| {
            let mut seen = self.seen.borrow_ref_mut(cs);

            for report in reports.flatten() {
                let addr = report.addr.into_inner();
                let name = local_name(report.data);

                // Phones advertise many times a second; keep one entry per address and let a
                // later report fill in a name an earlier one did not carry.
                if let Some(known) = seen.iter_mut().find(|d| d.addr == addr) {
                    known.rssi = report.rssi;
                    if known.name.is_none() {
                        known.name = name;
                    }
                } else if seen.len() < BLE_MAX_DEVICES {
                    seen.push(SeenDevice {
                        addr,
                        rssi: report.rssi,
                        name,
                    });
                }
            }
        });
    }
}

/// Extracts the local name from advertising data, if it carries one.
///
/// Most advertisers do not: a name costs bytes in a 31-byte packet. Android only includes it
/// while the Bluetooth settings screen is open and the phone is discoverable.
fn local_name(data: &[u8]) -> Option<String> {
    AdStructure::decode(data).flatten().find_map(|ad| match ad {
        AdStructure::CompleteLocalName(name) | AdStructure::ShortenedLocalName(name) => {
            core::str::from_utf8(name).ok().map(String::from)
        }
        _ => None,
    })
}

/// Keep a BLE scan window open for `window`, or until the card's dialog raises its access point.
async fn hold_window(window: Duration) {
    let began = Instant::now();
    while began.elapsed() < window && !share::OPEN.load(Ordering::Relaxed) {
        Timer::after(NEARBY_POLL).await;
    }
}

/// Waits `slow`, or only `fast` while a face that listens is on the screen -- and notices such a
/// face coming up within [`NEARBY_POLL`].
async fn pause(slow: Duration, fast: Duration) {
    let began = Instant::now();
    while began.elapsed() < slow && !(nearby::wanted() && began.elapsed() >= fast) {
        Timer::after(NEARBY_POLL).await;
    }
}

/// Scans for access points, logs what is in range and hands it to `nearby`.
///
/// It began as the proof that the radio transmits and receives rather than merely
/// initialising, and became what a face with `Rights::RADIO` hears.
#[allow(
    clippy::large_stack_frames,
    reason = "many small locals in the task's future, none over 120 bytes; the main stack has room, see the note at the top"
)]
#[embassy_executor::task]
async fn wifi_scan(mut controller: WifiController<'static>, access_point: AccessPointConfig) {
    // Scanning needs station mode. `set_config` starts the driver as a side effect -- there is
    // no separate start call in esp-radio 0.18.
    if let Err(err) = controller.set_config(&WifiConfig::Station(StationConfig::default())) {
        error!("Wi-Fi station mode failed: {err:?}");
        return;
    }

    let config = ScanConfig::default()
        .with_max(SCAN_MAX_NETWORKS)
        .with_show_hidden(true)
        .with_scan_type(ScanTypeConfig::Active {
            min: SCAN_DWELL_MIN,
            max: SCAN_DWELL_MAX,
        });

    let mut sharing = false;
    loop {
        // The access point comes and goes with the share dialog. A mode change restarts the
        // driver, so it happens here, between scans, where nothing else holds the controller.
        let wanted = share::OPEN.load(Ordering::Relaxed);
        if wanted != sharing {
            let mode = if wanted {
                WifiConfig::AccessPointStation(StationConfig::default(), access_point.clone())
            } else {
                WifiConfig::Station(StationConfig::default())
            };
            match controller.set_config(&mode) {
                Ok(()) => info!("Share: access point {}", if wanted { "up" } else { "down" }),
                Err(err) => error!("Share: Wi-Fi mode change failed: {err:?}"),
            }
            sharing = wanted;
        }

        // No scans while the access point is up: a scan switches channels, which stalls a
        // download and drops a joined client for seconds.
        if !sharing {
            match controller.scan_async(&config).await {
                Ok(mut networks) => {
                    networks.sort_by_key(|ap| core::cmp::Reverse(ap.signal_strength));
                    WIFI_NETWORKS.store(networks.len() as u8, Ordering::Relaxed);
                    nearby::publish(
                        Radio::Wifi,
                        networks.iter().map(|ap| Heard {
                            address: ap.bssid,
                            strength: ap.signal_strength,
                            channel: ap.channel,
                            name: ap.ssid.as_str(),
                        }),
                    );
                    info!("Scan: {} networks", networks.len());
                    // While a face listens a round comes every few seconds, and twenty lines each
                    // would bury the log.
                    for ap in networks.iter().filter(|_| !nearby::wanted()) {
                        let b = ap.bssid;
                        info!(
                            "  {:>4} dBm  ch {:>2}  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {:?}  {}",
                            ap.signal_strength,
                            ap.channel,
                            b[0],
                            b[1],
                            b[2],
                            b[3],
                            b[4],
                            b[5],
                            ap.auth_method,
                            ap.ssid.as_str()
                        );
                    }
                }
                Err(err) => error!("Scan failed: {err:?}"),
            }
        }

        let began = Instant::now();
        while began.elapsed() < SCAN_INTERVAL
            && !(nearby::wanted() && began.elapsed() >= NEARBY_WIFI_GAP)
            && share::OPEN.load(Ordering::Relaxed) == sharing
        {
            Timer::after(NEARBY_POLL).await;
        }
    }
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "everything the firmware owns is set up here and lives for as long as it runs; the main stack has room, see the note at the top"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.3.0
    // generator parameters: --chip esp32s3 -o alloc -o embassy -o log -o esp-backtrace -o unstable-hal -o wifi -o ble-trouble

    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let delay = Delay::new();

    // The screen, in one call: external RAM, the QSPI bus at the 80 MHz the panel was measured
    // to take, the vendor initialisation sequence, and a blank 360x360 picture to draw into.
    // Which pin carries what is board knowledge and stays here; everything after this line is
    // about pictures. See `src/screen.rs`.
    let pins = ScreenPins {
        sck: peripherals.GPIO13.into(),
        sio0: peripherals.GPIO15.into(),
        sio1: peripherals.GPIO16.into(),
        sio2: peripherals.GPIO17.into(),
        sio3: peripherals.GPIO18.into(),
        cs: peripherals.GPIO14.into(),
        reset: peripherals.GPIO21.into(),
        // Not the screen's: the LED controller drives it below, so that it can be dimmed.
        backlight: None,
    };
    let mut screen = match Screen::new(
        peripherals.PSRAM,
        peripherals.SPI2,
        peripherals.DMA_CH0,
        pins,
        delay,
    ) {
        Ok(screen) => Some(screen),
        Err(err) => {
            // A firmware that cannot show anything can still scan, report and be talked to, so
            // this is not fatal -- but it is the loudest thing in the log.
            error!("Display: the screen did not come up: {err:?}");
            None
        }
    };
    // The backlight starts dark and lights with the first picture of home, at the stored
    // brightness -- see the main loop. **Nothing before it is lit**: there is no boot screen,
    // and after a reset the panel still holds the last picture of the run before, which must
    // not show either. A configuration the controller
    // refuses is refused on every boot, so the dark screen that follows shows at the first flash
    // and not in someone's hand.
    let backlight = match Backlight::new(peripherals.LEDC, peripherals.GPIO47) {
        Ok(backlight) => Some(backlight),
        Err(err) => {
            error!("Display: the backlight did not take its configuration: {err:?}");
            None
        }
    };
    if screen.is_some() {
        info!("Display: up at 80 MHz");
    }

    // The rest of the board, in the order it was measured. Every pin here has a run behind it
    // in `src/bin/`; what those runs proved individually is what this firmware now holds at
    // once.
    let mut i2c = I2c::new(
        peripherals.I2C0,
        I2cConfig::default().with_frequency(Rate::from_khz(400)),
    )
    .expect("the I2C peripheral could not be configured")
    .with_sda(peripherals.GPIO11)
    .with_scl(peripherals.GPIO12);

    // The screen. GPIO10 is not wired as a reset -- the controller answers without a pulse --
    // and the interrupt line pulses too briefly to sample, so the driver polls instead.
    let mut touch = Touch::new(
        Output::new(peripherals.GPIO10, Level::High, OutputConfig::default()),
        Input::new(
            peripherals.GPIO9,
            InputConfig::default().with_pull(Pull::Up),
        ),
        &delay,
    );
    match touch.chip_id(&mut i2c) {
        Ok(id) => info!("Touch: the controller answers {id:#04x} (a CST816D)"),
        Err(err) => error!("Touch: the controller does not answer: {err:?}"),
    }

    // The motor. GPIO38 is an enable line that appears in no pin list -- it was found by
    // raising every free pin in turn and asking the chip for its own diagnosis.
    let mut haptic = Haptic::new(
        Output::new(peripherals.GPIO38, Level::High, OutputConfig::default()),
        &delay,
    );
    let _ = haptic.wake(&mut i2c);
    let _ = haptic.set_actuator(&mut i2c, Actuator::Lra);

    // **Calibration is the only thing at start-up that the user feels**, so it happens once per
    // board rather than once per boot. What a run leaves behind goes into the `nvs` partition,
    // and every boot after the first writes it back instead of driving the motor again.
    //
    // The first boot on a fresh partition therefore buzzes, and it buzzes for the *longest*
    // calibration time: it is paid once, and the result is what every later boot inherits.
    // Erasing the two sectors -- which `src/bin/store.rs` does when it finishes -- is what asks
    // for a new one.
    //
    // The first read happens here, before `esp_rtos::start` and the radio further down. **The
    // store no longer ends with this block**, though: the settings page writes through it later,
    // with the radios up, which is why what it borrows is leaked onto the heap rather than left
    // on this stack frame. See [`save_settings`] for what that costs.
    //
    // Both of these have to outlive this frame: the store's region writes through the flash and
    // points into the raw partition table, so neither can be a local.
    //
    // **Static and not heap:** `esp_alloc::heap_allocator!` runs further down this function, so
    // up here there is no heap to allocate from, and a box panics in `handle_alloc_error`.
    static FLASH: StaticCell<FlashStorage<'static>> = StaticCell::new();
    static TABLE: StaticCell<[u8; TABLE_SCRATCH]> = StaticCell::new();
    let flash = FLASH.init(FlashStorage::new(peripherals.FLASH));
    let table = TABLE.init([0u8; TABLE_SCRATCH]);

    // The plugins in the `plugins` partition, read before the store takes the flash for good.
    // They are copied into the screen's external RAM, which it hands over once, so what is left
    // goes on to where the page, the ring and the cover come off it.
    let (modules, plugin_count, waiting, from_slot, spare, free_slots) = gather_modules(
        flash::plugins(flash, table)
            .inspect_err(|_| warn!("Plugin: no plugins partition, only the bundled plugins"))
            .ok(),
        screen.as_mut().and_then(Screen::take_spare),
    );

    let mut store = {
        flash::nvs(flash, table)
            .and_then(|region| {
                Store::new(region).map_err(|e| {
                    error!("Settings: the nvs partition will not hold a store: {e:?}");
                    flash::NoPartition
                })
            })
            .ok()
    };

    // Two copies on purpose: what the flash holds, and what the firmware is running on. The
    // settings page moves the second one freely and the way out writes the difference -- which
    // is also what makes "nothing changed" cost nothing.
    let mut stored = match store.as_mut() {
        None => {
            error!("Settings: no store -- this boot calibrates and forgets it again");
            Settings::default()
        }
        Some(store) => {
            let mut buf = [0u8; settings::LEN];
            match store.load(&mut buf) {
                Ok(Some(len)) => {
                    info!("Settings: record version {}, {len} bytes", buf[0]);
                    Settings::decode(&buf[..len], &bundled_ids())
                }
                Ok(None) => Settings::default(),
                Err(e) => {
                    error!("Settings: could not be read: {e:?}");
                    Settings::default()
                }
            }
        }
    };
    let removed = bundled_ids()
        .iter()
        .flatten()
        .filter(|id| !stored.installed(**id))
        .count();
    info!(
        "Settings: colour theme {}, {removed} bundled plugins removed",
        stored.theme.name()
    );

    // What the drive stood at before this boot wrote it. **The driver keeps it across a reset of
    // this chip** -- a boot after a flash once read back `0x17`/`0x32`, the step the previous
    // build had set -- so only a boot after a power cut reads the chip's own values, and the
    // first boot of the build that began writing them is where [`DRIVE_FULL`] comes from.
    match haptic.settings(&mut i2c) {
        Ok([rated, clamp, ..]) => info!("Haptic: found rated {rated:#04x}, clamp {clamp:#04x}"),
        Err(err) => error!("Haptic: the drive could not be read: {err:?}"),
    }
    // A calibration runs at full drive whatever the clicks are set to: it measures the actuator,
    // and every step drives through what it leaves behind.
    let _ = haptic.set_drive(&mut i2c, DRIVE_FULL.0, DRIVE_FULL.1);

    match stored.haptic {
        Some(cal) => {
            let restored = cal.into();
            match haptic.set_calibration(&mut i2c, &restored) {
                Ok(()) => info!(
                    "Haptic: restored calibration, compensation {:#04x}, back-EMF {:#04x} -- no buzz",
                    cal.compensation, cal.back_emf
                ),
                Err(err) => error!("Haptic: writing back the calibration failed: {err:?}"),
            }
        }
        None => {
            info!("Haptic: nothing stored, calibrating once -- this is the buzz");
            let time = if store.is_some() {
                CalTime::Longest
            } else {
                // Nowhere to keep it, so it is paid again next boot; the long time is only
                // worth its noise when the result outlives the run that made it.
                CalTime::Short
            };
            if let Some(cal) = calibrate(&mut haptic, &mut i2c, &delay, time) {
                stored.haptic = Some((&cal).into());
                if let Some(store) = store.as_mut() {
                    save_settings(store, &stored);
                }
            }
        }
    }

    // What the firmware runs on. It starts as what was read back and the settings page moves it.
    let mut settings = stored;
    if let Some(screen) = screen.as_mut() {
        screen.set_orientation(settings.orientation as usize);
        screen.set_path(SCREEN_PATH);
        info!("Screen: pictures go out {:?}", screen.path());
    }

    let _ = haptic.set_library(&mut i2c, Library::Lra);
    // `click` below writes a sequence and pulls GO; the mode it triggers in is set once here.
    let _ = haptic.set_mode(&mut i2c, HapticMode::InternalTrigger);
    set_haptics(&mut haptic, &mut i2c, settings.haptics);
    info!(
        "Haptic: clicks at step {} of {}",
        settings.haptics.step(),
        Haptics::MAX.step()
    );

    // The knob. GPIO8 is the clockwise direction, measured against a mark on the screen rather
    // than taken from the schematic. How many pulses make a revolution is still open: 30 when
    // polled, 37 to 41 with interrupts depending on the speed; the menu assumes 40.
    let pull_up = InputConfig::default().with_pull(Pull::Up);
    let mut io = Io::new(peripherals.IO_MUX);

    let mut encoder = Encoder::new(
        &mut io,
        Input::new(peripherals.GPIO8, pull_up),
        Input::new(peripherals.GPIO7, pull_up),
    );

    // The other chip. It owns the DAC, classic Bluetooth and a second encoder on the same
    // shaft; this link is how we read what it is playing and tell it what to play next.
    let mut companion =
        match Uart::new(peripherals.UART1, UartConfig::default().with_baudrate(BAUD)) {
            Ok(uart) => {
                let (rx, tx) = uart
                    .with_tx(peripherals.GPIO40)
                    .with_rx(peripherals.GPIO39)
                    .split();
                let mut companion = Companion::new(rx, tx);
                companion.request_status();
                Some(companion)
            }
            Err(err) => {
                error!("Companion: UART1 could not be configured: {err:?}");
                None
            }
        };

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 73744);
    // COEX needs more RAM - so we've added some more
    esp_alloc::heap_allocator!(size: 64 * 1024);

    // Cover art: the bytes as the other chip sends them, and the picture they unpack into.
    // **Neither goes through the allocator.** The internal heap is 134 KiB and the radio is
    // already in it, while a decoded 200x200 cover is 120 KiB on its own. The screen owns the
    // external RAM, so the screen is where this comes from -- and there is eight megabytes of
    // it behind the two pictures. Without a screen there is nothing to show a cover on either,
    // which is why both are missing together.
    //
    // The plugin's page comes off the end first: 64 KiB of the megabytes behind the pictures,
    // where a face's memory costs the internal heap nothing. One page, because one plugin runs
    // at a time -- see [`start_plugin`].
    let (spare, mut page): (_, Option<Page>) = match spare {
        Some(spare) if spare.len() > COVER_MAX_BYTES + plugin::PAGE => {
            let at = spare.len() - plugin::PAGE;
            let (rest, tail) = spare.split_at_mut(at);
            (Some(rest), Page::new(tail))
        }
        other => (other, None),
    };
    // Then the ring's shape, worked out once here so that a menu paints its ring from it instead
    // of working it out again for every frame -- see [`Ring`]. The clock of `embassy_time` is not
    // running yet, so esp-hal's times it.
    let (spare, ring) = match spare {
        Some(spare) if spare.len() > COVER_MAX_BYTES + RING_BYTES => {
            let at = spare.len() - RING_BYTES;
            let (rest, tail) = spare.split_at_mut(at);
            let started = esp_hal::time::Instant::now();
            let ring = Ring::new(tail);
            info!(
                "Menu: the ring's shape in {} KiB of external RAM, worked out in {} ms",
                RING_BYTES / 1024,
                started.elapsed().as_millis()
            );
            (Some(rest), ring)
        }
        other => (other, None),
    };
    // The share's socket buffers, which only the CPU copies.
    let (spare, share_area) = match spare {
        Some(spare) if spare.len() > COVER_MAX_BYTES + share::BUFFER_BYTES => {
            let at = spare.len() - share::BUFFER_BYTES;
            let (rest, tail) = spare.split_at_mut(at);
            (Some(rest), Some(tail))
        }
        other => (other, None),
    };
    let (mut cover, mut cover_pixels) = match spare {
        Some(spare) if spare.len() > COVER_MAX_BYTES => {
            let (bytes, pixels) = spare.split_at_mut(COVER_MAX_BYTES);
            info!(
                "Cover: {} KiB of external RAM for a decoded picture",
                pixels.len() / 1024
            );
            (Some(Cover::new(bytes)), Some(pixels))
        }
        _ => {
            warn!("Cover: no external RAM to spare -- cover art will not be shown");
            (None, None)
        }
    };

    // The card. It gets its own SPI peripheral: SPI2 is the panel's, and the two run at
    // different clocks and different widths. This comes after the allocator because everything
    // below it keeps file names, and a name is a `String`.
    let mut volume = match Spi::new(
        peripherals.SPI3,
        SpiConfig::default().with_frequency(sd::INIT_RATE),
    ) {
        Ok(spi) => {
            let spi = spi
                .with_sck(peripherals.GPIO4)
                .with_mosi(peripherals.GPIO3)
                .with_miso(peripherals.GPIO5);
            let cs = Output::new(peripherals.GPIO2, Level::High, OutputConfig::default());
            match SdCard::new(spi, cs, delay) {
                Ok(card) => match Volume::mount(card) {
                    Ok(volume) => {
                        let layout = *volume.layout();
                        info!(
                            "Card: {} with {} clusters of {} KiB, mounted",
                            if layout.fat32 { "FAT32" } else { "FAT16" },
                            layout.clusters,
                            layout.cluster_bytes() / 1024
                        );
                        Some(volume)
                    }
                    Err(err) => {
                        error!("Card: no filesystem on it: {err:?}");
                        None
                    }
                },
                Err(err) => {
                    // No card in the slot is the ordinary case and not a fault: everything
                    // else this firmware does works without one.
                    warn!("Card: none answered: {err:?}");
                    None
                }
            }
        }
        Err(err) => {
            error!("Card: SPI3 could not be configured: {err:?}");
            None
        }
    };

    // What the card has to show. **Read once and no longer stepped through**: the swipe that
    // used to walk this list is now the page axis, and when the list comes back it comes back as
    // an entry on the settings page. A read is 130 ms, which was never something to do per frame
    // -- that is what the backdrop in `Screen` is for.
    let backgrounds: Vec<String> = match volume.as_mut() {
        Some(volume) if CARD_BACKGROUNDS => background_names(volume),
        _ => Vec::new(),
    };
    // The first one goes up before anything else does, so the status screen has ground under
    // it from its very first frame rather than appearing on black and then jumping.
    let initial_background = match (volume.as_mut(), screen.as_mut()) {
        (Some(volume), Some(screen)) => match backgrounds.first() {
            Some(name) => load_background(volume, screen, name),
            None => None,
        },
        _ => None,
    };

    // What the share entry says about the card, worked out while the volume is still at hand.
    let card_bytes = volume.as_ref().map_or(0, |volume| volume.layout().bytes());
    let card_size = volume
        .as_ref()
        .map(|_| format!("{} card", share::card_size_text(card_bytes)));

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    let (wifi_controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, Default::default())
        .expect("Failed to initialize Wi-Fi controller");
    // find more examples https://github.com/embassy-rs/trouble/tree/main/examples/esp32
    let transport = BleConnector::new(peripherals.BT, Default::default()).unwrap();

    // The monitor's keyboard, for the screenshot key alone. It is the wire the log already
    // uses, so it costs nothing when nobody is watching.
    let (keys, _) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    let mut keys = Some(keys);
    let ble_controller = ExternalController::<_, 1>::new(transport);

    // The number the keys a face sees are made with; see `teetotum_firmware::nearby`. Drawn
    // after the radio is set up, in the hope that its noise is in the generator by now -- the
    // log says whether it was, since only then does the number differ from boot to boot.
    let mut salt = [0u8; 4];
    let mut secret = [0u8; 6];
    let trng = esp_hal::rng::Trng::try_new();
    let source = match &trng {
        Ok(trng) => {
            trng.read(&mut salt);
            trng.read(&mut secret);
            "physical"
        }
        Err(_) => {
            let rng = esp_hal::rng::Rng::new();
            rng.read(&mut salt);
            rng.read(&mut secret);
            "pseudo-random"
        }
    };
    nearby::set_salt(u32::from_le_bytes(salt));
    info!("Nearby: keys made with a {source} number");

    // The card over Wi-Fi: its network, new every boot, and the IP stack on the access point
    // interface. The stack's room is static; the access point itself only runs while the dialog
    // is open, see [`wifi_scan`].
    let credentials = share::Credentials::new(interfaces.access_point.mac_address(), secret);
    info!(
        "Share: network {} with password {}",
        credentials.ssid, credentials.password
    );
    let share_code = qr::encode(&credentials.join_text());
    let access_point = AccessPointConfig::default()
        .with_ssid(credentials.ssid.as_str())
        .with_auth_method(AuthenticationMethod::Wpa2Personal)
        .with_password(credentials.password.clone())
        .with_max_connections(4);
    static NET_RESOURCES: StaticCell<StackResources<{ share::SOCKETS }>> = StaticCell::new();
    let net_seed = {
        let mut bytes = [0u8; 8];
        esp_hal::rng::Rng::new().read(&mut bytes);
        u64::from_le_bytes(bytes)
    };
    let (net_stack, mut net_runner) = embassy_net::new(
        interfaces.access_point,
        share::ip_config(),
        NET_RESOURCES.init(StackResources::new()),
        net_seed,
    );
    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    // Pairing draws its keys from this seed, and `build` panics without one from a true
    // generator. The radio started above is what enables it.
    let mut trng = trng.expect("the radio enables the true random number generator");
    // A bond is tied to this address, so it stays the same across boots: the board's MAC, least
    // significant byte first, with the top two bits that mark a static random address.
    let mut identity = [0u8; 6];
    identity.copy_from_slice(esp_hal::efuse::base_mac_address().as_bytes());
    identity.reverse();
    identity[5] |= 0xC0;
    info!("BLE: identity address {}", AddressText(&identity));
    let stack = trouble_host::new(ble_controller, &mut resources)
        .set_random_address(Address::random(identity))
        .set_random_generator_seed(&mut trng);
    let Host {
        central,
        mut peripheral,
        mut runner,
        ..
    } = stack.build();
    if let Some(bond) = stored.bond {
        match stack.add_bond_information(bond_information(&bond)) {
            Ok(()) => info!("BLE: bonded with {}", AddressText(&bond.address)),
            Err(err) => error!("BLE: the stored bond was not taken -- {err:?}"),
        }
    }

    let scan_log = BleScanLog::new();

    // The runner drives the host and delivers the advertising reports; the scan loop opens the
    // windows that produce them. Neither returns, so they are joined rather than spawned -- the
    // stack borrows `resources` from this frame and is not 'static.
    let scanning = async {
        let mut scanner = Scanner::new(central);
        let config = BleScanConfig {
            active: true,
            ..Default::default()
        };

        loop {
            // Stand down while a peer is connected or the card is shared over Wi-Fi: scanning
            // shares the radio with both. Not only during a transfer -- a window beside the
            // access point costs a joined client three to eight seconds for a plain page, once
            // per scan cycle, measured.
            if PEER_CONNECTED.load(Ordering::Relaxed) || share::OPEN.load(Ordering::Relaxed) {
                Timer::after(CONNECTION_POLL).await;
                continue;
            }

            let window = if nearby::wanted() {
                NEARBY_BLE_WINDOW
            } else {
                BLE_SCAN_WINDOW
            };
            match scanner.scan(&config).await {
                Ok(session) => {
                    hold_window(window).await;
                    drop(session);
                }
                Err(err) => {
                    error!("BLE scan failed: {err:?}");
                    // Not straight round again: with no pause while a face listens, a scan that
                    // keeps failing would fail in a tight loop.
                    Timer::after(window).await;
                }
            }

            let mut devices = scan_log.take();
            devices.sort_by_key(|device| core::cmp::Reverse(device.rssi));
            nearby::publish(
                Radio::Bluetooth,
                devices.iter().map(|device| Heard {
                    address: device.addr,
                    strength: device.rssi,
                    channel: 0,
                    name: device.name.as_deref().unwrap_or(""),
                }),
            );
            let named = devices.iter().filter(|d| d.name.is_some()).count();
            info!("BLE: {} advertisers, {} with a name", devices.len(), named);
            // As for Wi-Fi: forty lines every two seconds would bury the log.
            for device in devices.iter().filter(|_| !nearby::wanted()) {
                let a = device.addr;
                info!(
                    "  {:>4} dBm  {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}  {}",
                    device.rssi,
                    a[0],
                    a[1],
                    a[2],
                    a[3],
                    a[4],
                    a[5],
                    device.name.as_deref().unwrap_or("-")
                );
            }

            pause(BLE_SCAN_INTERVAL, Duration::from_secs(0)).await;
        }
    };

    spawner.spawn(
        wifi_scan(wifi_controller, access_point).expect("Failed to create the Wi-Fi scan task"),
    );

    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: BLE_DEVICE_NAME,
        appearance: &KNOB_APPEARANCE,
    }))
    .expect("the GATT server does not fit its attribute table");
    let mut version = HeaplessString::<32>::new();
    let _ = version.push_str(VERSION);
    let _ = server.set(&server.knob.version, &version);
    let _ = server.set(&server.knob.card_bytes, &card_bytes);

    // Keeps the published values current for whoever is reading them.
    let publish = || {
        let _ = server.set(&server.knob.uptime, &(Instant::now().as_secs() as u32));
        let _ = server.set(
            &server.knob.networks,
            &WIFI_NETWORKS.load(Ordering::Relaxed),
        );
        let settings = critical_section::with(|cs| SHARED_SETTINGS.borrow(cs).get());
        let _ = server.set(&server.knob.settings, &settings);
        settings
    };

    // An upload's commands go from the advertising loop to the device loop, which holds the
    // flash, and its status comes back.
    let uploads: Channel<NoopRawMutex, Command, 2> = Channel::new();
    let upload_status: Signal<NoopRawMutex, [u8; upload::STATUS_LEN]> = Signal::new();
    // A new bond goes the same way, to be kept in the settings, and forgetting it the other way.
    let bonds: Signal<NoopRawMutex, Bond> = Signal::new();
    let forget: Signal<NoopRawMutex, ()> = Signal::new();
    // Settings a sender wrote go to the device loop, which applies and keeps them.
    let setting_writes: Signal<NoopRawMutex, teetotum_pack::settings::Settings> = Signal::new();

    let advertising = async {
        // Flags, appearance and the name come to 17 of the 31 bytes; the 128-bit service UUID
        // alone is 18 and has to travel in the scan response instead. Scanning here is active,
        // and so is a phone's, so both halves arrive.
        let mut adv_data = [0u8; 31];
        let appearance = KNOB_APPEARANCE.to_le_bytes();
        let len = AdStructure::encode_slice(
            &[
                AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                AdStructure::Unknown {
                    ty: AD_TYPE_APPEARANCE,
                    data: &appearance,
                },
                AdStructure::CompleteLocalName(BLE_DEVICE_NAME.as_bytes()),
            ],
            &mut adv_data,
        )
        .expect("the advertising data does not fit into 31 bytes");

        let mut scan_data = [0u8; 31];
        let scan_len = AdStructure::encode_slice(
            &[AdStructure::ServiceUuids128(&[KNOB_SERVICE_UUID_LE])],
            &mut scan_data,
        )
        .expect("the scan response does not fit into 31 bytes");

        let params = AdvertisementParameters::default();

        loop {
            let advertiser = match peripheral
                .advertise(
                    &params,
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &adv_data[..len],
                        scan_data: &scan_data[..scan_len],
                    },
                )
                .await
            {
                Ok(advertiser) => advertiser,
                Err(err) => {
                    error!("BLE advertising failed: {err:?}");
                    Timer::after(BLE_SCAN_WINDOW).await;
                    continue;
                }
            };

            info!("BLE: advertising as \"{BLE_DEVICE_NAME}\"");

            let connection = match advertiser.accept().await {
                Ok(connection) => connection,
                Err(err) => {
                    error!("BLE connection failed: {err:?}");
                    continue;
                }
            };

            let connection = match connection.with_attribute_server(&server) {
                Ok(connection) => connection,
                Err(err) => {
                    error!("BLE attribute server failed: {err:?}");
                    continue;
                }
            };

            // Forgotten while nobody was connected: the bond goes before the peer can encrypt.
            if forget.try_take().is_some() {
                forget_bonds(&stack);
            }
            if let Err(err) = connection.raw().set_bondable(true) {
                warn!("BLE: the connection will not bond -- {err:?}");
            }
            PEER_CONNECTED.store(true, Ordering::Relaxed);
            // A peer may read before the first refresh, or before it selects an entry.
            let mut told = publish();
            let first = critical_section::with(|cs| PLUGIN_LIST.borrow_ref(cs).entry(0));
            let _ = server.set(&server.upload.entry, &first);
            let since = Instant::now();
            let mut security = SecurityLevel::NoEncryption;
            info!("BLE: a device connected");

            loop {
                match select3(
                    connection.next(),
                    Timer::after(GATT_REFRESH),
                    upload_status.wait(),
                )
                .await
                {
                    Either3::First(GattConnectionEvent::Disconnected { reason }) => {
                        info!(
                            "BLE: the device disconnected after {} s, reason {:?}",
                            since.elapsed().as_secs(),
                            reason
                        );
                        break;
                    }
                    Either3::First(GattConnectionEvent::Gatt { event }) => {
                        // An upload's writes go to the device loop. The reply waits until it has
                        // room for them, which paces the sender.
                        let command = match &event {
                            GattEvent::Write(write)
                                if write.handle() == server.upload.control.handle =>
                            {
                                Some(Command::control(write.data()))
                            }
                            GattEvent::Write(write)
                                if write.handle() == server.upload.data.handle =>
                            {
                                Some(Command::data(write.data()))
                            }
                            _ => None,
                        };
                        // A select is answered here, from the list, and taken at any time.
                        let selected = match &event {
                            GattEvent::Write(write)
                                if write.handle() == server.upload.select.handle =>
                            {
                                Some(<[u8; 1]>::try_from(write.data()).ok())
                            }
                            _ => None,
                        };
                        if let Some(Some([index])) = selected {
                            let raw = critical_section::with(|cs| {
                                PLUGIN_LIST.borrow_ref(cs).entry(index)
                            });
                            let _ = server.set(&server.upload.entry, &raw);
                        }
                        let written = match &event {
                            GattEvent::Write(write)
                                if write.handle() == server.knob.settings.handle =>
                            {
                                Some(teetotum_pack::settings::Settings::decode(write.data()))
                            }
                            _ => None,
                        };
                        if let Some(Some(written)) = written {
                            setting_writes.signal(written);
                        }
                        let reply = match command {
                            None if selected == Some(None) || written == Some(None) => {
                                event.reject(AttErrorCode::VALUE_NOT_ALLOWED)
                            }
                            None => event.accept(),
                            Some(_) if !RECEIVING.load(Ordering::Relaxed) => {
                                event.reject(AttErrorCode::WRITE_NOT_PERMITTED)
                            }
                            Some(None) => event.reject(AttErrorCode::VALUE_NOT_ALLOWED),
                            Some(Some(command)) => {
                                uploads.send(command).await;
                                event.accept()
                            }
                        };
                        match reply {
                            Ok(reply) => reply.send().await,
                            Err(err) => error!("BLE could not answer a GATT request: {err:?}"),
                        }
                    }
                    Either3::First(GattConnectionEvent::PairingComplete {
                        security_level,
                        bond,
                    }) => {
                        info!("BLE: paired, {security_level:?}");
                        if let Some(info) = bond {
                            // One bond is kept, so one is known: the others go now rather than
                            // at the next boot.
                            for other in stack.get_bond_information() {
                                if other.identity != info.identity {
                                    let _ = stack.remove_bond_information(other.identity);
                                }
                            }
                            info!(
                                "BLE: bonded with {}",
                                AddressText(&address_bytes(info.identity.bd_addr))
                            );
                            bonds.signal(stored_bond(&info));
                        }
                    }
                    Either3::First(GattConnectionEvent::PairingFailed(err)) => {
                        warn!("BLE: pairing failed -- {err:?}");
                    }
                    Either3::First(_) => {}
                    Either3::Third(status) => {
                        if let Err(err) = server.upload.status.notify(&connection, &status).await {
                            warn!("BLE: upload status not sent -- {err:?}");
                        }
                    }
                    Either3::Second(()) => {
                        // A link that was encrypted with the forgotten bond stays encrypted until
                        // it ends, so it is ended.
                        if forget.try_take().is_some() {
                            forget_bonds(&stack);
                            connection.raw().disconnect();
                        }
                        // Settings changed on the glass or by the peer are told as they change.
                        let settings = publish();
                        if settings != told {
                            match server.knob.settings.notify(&connection, &settings).await {
                                Ok(()) => told = settings,
                                Err(err) => warn!("BLE: settings not sent -- {err:?}"),
                            }
                        }
                        // A known peer encrypts without pairing, and nothing reports that.
                        if let Ok(level) = connection.raw().security_level()
                            && level != security
                        {
                            info!("BLE: security {level:?}");
                            security = level;
                        }
                    }
                }
            }

            PEER_CONNECTED.store(false, Ordering::Relaxed);
        }
    };

    // The bundled plugins. Their manifests are read whether or not they are installed -- that is
    // what lets the settings show them, and install them again, without running any of them.
    // **None is loaded here**: a plugin is loaded when its face is started at home, and one runs
    // at a time -- see [`start_plugin`]. This comes after the radio: the heap is shared, and a
    // plugin that does not fit is a plugin missing, where a radio that does not fit is a device
    // missing.
    // A plugin without a signature section is treated as one without a manifest: it has no id.
    let ids: [Option<PluginId>; PLUGINS_MAX] =
        core::array::from_fn(|n| modules[n].and_then(|wasm| PluginId::of(wasm).ok()));
    let manifests: [Option<Manifest<'static>>; PLUGINS_MAX] = core::array::from_fn(|n| {
        let wasm = modules[n]?;
        PluginId::of(wasm)
            .and(Manifest::read(wasm))
            .inspect_err(|e| error!("Plugin: plugin {n} has no usable manifest -- {e}"))
            .ok()
    });
    let (settings_menu, home_menu): (&'static Menu, &'static Menu) = {
        static ICONS: StaticCell<[Icon; PLUGINS_MAX]> = StaticCell::new();
        static PLUGIN_MENUS: StaticCell<[Menu; PLUGINS_MAX]> = StaticCell::new();
        // One cell per page of either ring; the pages a build does not need stay empty.
        static MENU: [StaticCell<Menu>; MAX_PAGES] = [const { StaticCell::new() }; MAX_PAGES];
        static HOME: [StaticCell<Menu>; MAX_PAGES] = [const { StaticCell::new() }; MAX_PAGES];
        // An array is made whole, so a plugin without a manifest gets an icon and a menu too;
        // its segment stays empty, and neither is ever shown.
        let icons = ICONS.init(core::array::from_fn(|n| {
            Icon::packed(manifests[n].map_or(&[], |manifest| manifest.icon_bytes()))
        }));
        let menus = PLUGIN_MENUS.init(core::array::from_fn(|n| {
            plugin_menu(manifests[n].map_or("", |manifest| manifest.name()), n)
        }));
        // A plugin with a manifest has a place in the settings, and at home too while it is
        // installed. **Both rings are laid out without gaps**, bundled plugins first, so a
        // plugin that comes or goes takes a restart to get its place or give it up.
        //
        // **More plugins than a ring has room for spill onto a second page**, which carries only
        // the top entry and none of what the pages before it carry: the firmware's own entries
        // and the player stand on the first page alone. Each ring is
        // built backwards, its last page first, so that every page can be handed the page it
        // turns on to.
        let listed = |n: &usize| manifests[*n].is_some();
        let on_home = |n: &usize| listed(n) && ids[*n].is_some_and(|id| settings.installed(id));
        let settings_count = (0..PLUGINS_MAX).filter(listed).count();
        let home_count = (0..PLUGINS_MAX).filter(on_home).count();
        // One more entry than plugins: receiving one comes after them.
        let settings_pages = pages_for(settings_count + 1, PLUGINS_ON_FIRST_SETTINGS_PAGE);
        let mut settings_next: Option<&'static Menu> = None;
        for page in (0..settings_pages).rev() {
            let mut menu = match page {
                0 => SETTINGS,
                _ => Menu::new(
                    SETTINGS.title,
                    Entry::setting("About", &icons::ABOUT, SETTING_ABOUT, Buttons::Ok),
                ),
            };
            for (k, n) in (0..PLUGINS_MAX).filter(listed).enumerate() {
                let (at, slot) = place(k, PLUGINS_ON_FIRST_SETTINGS_PAGE, PLUGIN_SLOT);
                if let (true, Some(manifest)) = (at == page, manifests[n]) {
                    menu = menu.with(slot, Entry::plugin(manifest.name(), &icons[n], &menus[n]));
                }
            }
            let (at, slot) = place(settings_count, PLUGINS_ON_FIRST_SETTINGS_PAGE, PLUGIN_SLOT);
            if at == page {
                menu = menu.with(
                    slot,
                    Entry::setting("Receive", &icons::PLUGIN, SETTING_RECEIVE, Buttons::Ok),
                );
            }
            if let Some(next) = settings_next {
                menu = menu.then(next);
            }
            settings_next = Some(MENU[page].init(menu));
        }
        let home_pages = pages_for(home_count, FACES_ON_FIRST_HOME_PAGE);
        let mut home_next: Option<&'static Menu> = None;
        for page in (0..home_pages).rev() {
            let mut home = match page {
                0 => Menu::home(HOME_TITLE)
                    .holding(HOLD_FOR_QR)
                    .with(
                        HOME_PLAYER_SLOT,
                        Entry::screen("Music Player", &icons::MUSIC, Face::Player.id()),
                    )
                    .with(
                        HOME_SHARE_SLOT,
                        Entry::setting(SHARE_NAME, &icons::CARD_WIFI, SETTING_SHARE, Buttons::None),
                    ),
                _ => Menu::home_page(HOME_TITLE).holding(HOLD_FOR_QR),
            };
            for (k, n) in (0..PLUGINS_MAX).filter(on_home).enumerate() {
                let (at, slot) = place(k, FACES_ON_FIRST_HOME_PAGE, HOME_PLUGIN_SLOT);
                if let (true, Some(manifest)) = (at == page, manifests[n]) {
                    let id = Face::Plugin(n as u8).id();
                    home = home.with(slot, Entry::screen(manifest.name(), &icons[n], id));
                }
            }
            if let Some(next) = home_next {
                home = home.then(next);
            }
            home_next = Some(HOME[page].init(home));
        }
        if settings_pages > 1 || home_pages > 1 {
            info!(
                "Menu: {plugin_count} plugins, {home_count} at home on {home_pages} page(s), settings on {settings_pages}"
            );
        }
        let first = |menu: Option<&'static Menu>| menu.expect("a ring has at least one page");
        (first(settings_next), first(home_next))
    };
    // The plugin that is loaded, and its place among the gathered ones.
    let mut running: Option<(usize, Plugin)> = None;
    info!(
        "Plugin: none loaded yet, heap {} bytes free",
        esp_alloc::HEAP.free()
    );

    // The device itself: the knob, the screen, the motor and the other chip, polled from one
    // place. It borrows peripherals from this frame, which is why it is a future joined here
    // rather than a spawned task -- the same reason the two BLE loops are.
    let device = async {
        let mut state = Overview {
            backdrop: initial_background.map(Backdrop::Card),
            orientation: settings.orientation as usize,
            theme: settings.theme,
            brightness: settings.brightness,
            haptics: settings.haptics,
            cover: settings.cover,
            motion: settings.motion,
            shape: settings.shape,
            free_slots,
            card: card_size.clone(),
            share_text: false,
            bonded: settings.bond.map(|bond| bond.address),
            plugins: core::array::from_fn(|n| {
                manifests[n].zip(ids[n]).map(|(manifest, id)| PluginView {
                    id,
                    name: manifest.name(),
                    summary: manifest.summary(),
                    bytes: modules[n].map_or(0, <[u8]>::len),
                    version: manifest.version(),
                    rights: manifest.rights(),
                    slot: from_slot[n],
                    installed: settings.installed(id),
                    loaded: None,
                    fault: None,
                })
            }),
            ..Overview::default()
        };
        // The device starts at home, which is where a face is chosen -- after the install dialog
        // for each plugin that waits in a slot.
        let mut offers = Offers::new(waiting);
        offer_next(&mut state, &mut offers, home_menu, settings_menu);
        // Whether a finished transfer is still waiting to be decoded. The decoding does not
        // happen where the last packet arrives: it costs a few hundred milliseconds, and the
        // link is what that pass is for.
        let mut cover_waiting = false;
        // The upload under way over BLE, and when the knob restarts after one was written.
        let mut upload: Option<Upload> = None;
        let mut restart_at: Option<Instant> = None;
        // Which plugins were installed when the list for BLE was last filled, one bit each.
        let mut listed: Option<u16> = None;
        // What the screen is currently showing. The picture is sent only when the gathered
        // state differs from it, which makes the redraw rate a consequence of what changed
        // rather than a timer: `uptime` moves once a second and everything else on demand.
        let mut shown: Option<Overview> = None;
        let mut cloud_meter = CloudMeter::new();
        // The long lines' one run; see [`Run`]. `player_up` is whether the player was on the
        // screen in the last pass, `run_pass` when that pass was.
        let mut title_run = Run::default();
        let mut player_up = false;
        let mut run_pass = Instant::now();
        // The other chip's own bits of its state byte as last reported, to log each change.
        let mut companion_bits: Option<u8> = None;
        info!(
            "Player: {} px for the title, {} px for the artist",
            TITLE_LINE.room().1,
            ARTIST_LINE.room().1
        );
        // Whether the backlight is on yet. It comes on with the first picture that reached the
        // panel, so that home is the first thing on the screen.
        let mut lit = false;
        // Whether the backlight stands at full for a QR code rather than at the stored level.
        let mut qr_lit = false;
        // How many frames still get asked whether the external RAM holds them; see
        // [`EXTERNAL_PROBES`].
        let mut probes = EXTERNAL_PROBES;
        let mut next_touch = Instant::now();
        // One answer per contact; see the gesture handling below.
        let mut taps = Taps::new();
        let mut next_status = Instant::now();
        // Queries sent since the last answer; at [`STATUS_UNANSWERED`] the chip is taken as gone.
        let mut status_unanswered: u8 = 0;
        // When the click currently playing should be cut off, if one is.
        let mut click_ends: Option<Instant> = None;
        // Detents the knob has counted and the phone has not been told about yet, signed.
        // Turning is faster than the other chip will take keys, so the two are kept apart:
        // the knob adds here and the pass below pays it off.
        let mut volume_owed: i32 = 0;
        let mut next_volume_key = Instant::now();
        // The keys of one turn, and where the phone stood before the first of them. **The
        // chip's own volume is the only witness we have**: `A3 03` is answered by nothing, and
        // a key that its notify slot overwrote and a key the phone ignored look the same from
        // here. Five per key times the keys sent is what the number should have moved by.
        let mut volume_asked: i32 = 0;
        // Detents each side of the shaft saw since the last report. **The second encoder is
        // the only witness this side has**: it sits on the same shaft, so the two numbers have
        // to agree -- twelve there against six here is what put the knob on an interrupt. They
        // are still counted, because a count that agrees is the only thing that says the latch
        // works.
        let mut turn_ours: i32 = 0;
        let mut turn_theirs: i32 = 0;
        let mut volume_before: Option<u8> = None;
        let mut volume_settling = false;
        // What the other chip should be doing with its encoder, which is a question of which
        // page is up. It is resent whenever the chip reports something else, so this is both
        // the request and the thing the report is checked against.
        let mut wanted_state = companion_state(&state);
        // What the settings were when the open dialog opened, for its Cancel to go back to.
        let mut before = settings;
        // The round of the radio the face on the screen was last handed, while one that listens
        // is shown; `None` otherwise, so that it is handed the latest as soon as it comes up.
        let mut handed_round: Option<u32> = None;
        // The same for whether a phone is connected over BLE HID, told to a face that may send.
        let mut handed_link: Option<bool> = None;
        // When the face's pulse last clicked, while it wants one.
        let mut last_pulse: Option<Instant> = None;

        loop {
            // Ending the last click is the first thing done, so that its length is decided by
            // the loop period and not by how much work this pass happens to find.
            if let Some(ends) = click_ends
                && Instant::now() >= ends
            {
                let _ = haptic.stop(&mut i2c);
                click_ends = None;
            }

            // A screenshot on `s` in the monitor. The README's renders put something on the
            // screen, and the honest something is what the firmware actually drew -- so the
            // framebuffer goes down the log's own wire rather than being drawn a second time
            // in another language. Whatever is on the screen when the key arrives is what comes
            // out, so every screen is reachable: home, the player with a cover, any face.
            if let Some(keys) = keys.as_mut()
                && let Ok(key) = keys.read_byte()
                && key.eq_ignore_ascii_case(&b's')
                && let Some(screen) = screen.as_mut()
            {
                shot::dump(screen.frame().bytes(), WIDTH, HEIGHT);
            }

            // The other chip, which talks whenever it feels like it. Its receive FIFO holds
            // 128 bytes -- 1.4 ms at this baud rate -- and a cover transfer fills it a
            // thousand bytes at a time, so this comes before everything slow and not on a
            // timer.
            if let Some(companion) = companion.as_mut() {
                while let Some(event) = companion.poll() {
                    // A cover transfer answers its own frames. The pacing, the retries and the
                    // packet numbering are `cover`'s; what is left here is the log line.
                    if let Some(cover) = cover.as_mut()
                        && let Some(step) = cover.feed(event, companion)
                    {
                        report(step, &mut cover_waiting);
                        continue;
                    }
                    match event {
                        CompanionEvent::Status(status) => {
                            // What the turn asked for against what it got. A short count is
                            // not necessarily a lost key: the chip clamps at 0 and 127, so
                            // both numbers are printed rather than a verdict.
                            if volume_settling {
                                volume_settling = false;
                                if let Some(before) = volume_before.take() {
                                    info!(
                                        "Volume: {} detents here, {} there, {} keys sent -- {} -> {} is {:+}",
                                        turn_ours.abs(),
                                        turn_theirs.abs(),
                                        volume_asked.abs(),
                                        before,
                                        status.volume,
                                        i32::from(status.volume) - i32::from(before)
                                    );
                                }
                                volume_asked = 0;
                                turn_ours = 0;
                                turn_theirs = 0;
                            }
                            // Bits 4, 5 and 6 are the other chip's own. Bit 6 is a phone connected
                            // over BLE HID, as read in its image; 4 and 5 are unread. Logged on
                            // every change, so a run can line them up with the phone.
                            let own = status.state & !COMPANION_STATE_MASK;
                            if companion_bits != Some(own) {
                                info!(
                                    "Companion: own bits {own:#04x} (4: {}, 5: {}, 6: {})",
                                    own >> 4 & 1,
                                    own >> 5 & 1,
                                    own >> 6 & 1
                                );
                                companion_bits = Some(own);
                            }
                            if state.volume != Some(status.volume) {
                                info!("Companion: volume {}", status.volume);
                            }
                            status_unanswered = 0;
                            state.volume = Some(status.volume);
                            state.companion_encoder = status.encoder_enabled();
                            state.streaming = status.streaming();
                            state.hid = status.hid_connected();
                            if status.state & COMPANION_STATE_MASK != wanted_state {
                                info!(
                                    "Companion: state {:#04x} -- asking for {wanted_state:#04x}",
                                    status.state
                                );
                                companion.set_state(wanted_state);
                            }
                        }
                        CompanionEvent::Metadata => {
                            let (title, artist) =
                                (companion.metadata().title(), companion.metadata().artist());
                            if title != state.title || artist != state.artist {
                                state.title = String::from(title);
                                state.artist = String::from(artist);
                                title_run = Run::new(&state);
                            }
                            info!(
                                "Music: \"{}\" by {}",
                                companion.metadata().title(),
                                companion.metadata().artist()
                            );
                        }
                        // Both encoders sit on the same shaft, so a detent reported from over
                        // there is the same detent `encoder.poll()` already counted. It is
                        // logged and deliberately not added to anything.
                        CompanionEvent::Encoder(direction) => {
                            turn_theirs += if direction == Direction::Clockwise {
                                1
                            } else {
                                -1
                            };
                        }
                        other => info!("Companion: {other:?}"),
                    }
                }

                // Whatever the transfer still owes: the first packet after its pause, a retry
                // after "not in sending", or giving up on one nothing is answering.
                if let Some(cover) = cover.as_mut()
                    && let Some(step) = cover.tick(companion)
                {
                    report(step, &mut cover_waiting);
                }

                // The status query waits for a transfer to end. Its answer would arrive in the
                // middle of one, and this pass is being kept free for the picture.
                let busy = cover.as_ref().is_some_and(Cover::busy);
                if !busy && Instant::now() >= next_status {
                    next_status = Instant::now() + STATUS_PERIOD;
                    if status_unanswered >= STATUS_UNANSWERED && state.volume.is_some() {
                        info!("Companion: silent after {status_unanswered} status queries");
                        state.volume = None;
                    }
                    status_unanswered = status_unanswered.saturating_add(1);
                    companion.request_status();
                }
            }

            // **A transfer owns the pass it is in.** A redrawn screen is 14 ms and the receive
            // FIFO is 1.4, so drawing on the way to an answer eats the answer -- 34 packets
            // offered and none arriving is what that looked like. A whole cover is under half a
            // second, so the screen holds still for it; yielding rather than spinning keeps the
            // two Bluetooth futures alive meanwhile.
            if cover.as_ref().is_some_and(Cover::busy) {
                yield_now().await;
                continue;
            }

            // A finished cover is decoded **once**, here rather than where its last packet
            // arrived: it costs a few hundred milliseconds, and it ends up in the backdrop, so
            // every frame after this one is a 21 ms copy instead.
            //
            // Decoding holds [`cover::DECODE_HEAP`] of internal heap for a moment, and the plugin
            // that ran last may leave less. It gives way then, unless its face is on the screen:
            // the cover waits until the face is left.
            let face_running = shown_index(state.face)
                .is_some_and(|n| running.as_ref().is_some_and(|(m, _)| *m == n));
            if cover_waiting
                && !(face_running && heap_room() < cover::DECODE_HEAP)
                && let (Some(cover), Some(pixels), Some(screen)) =
                    (cover.as_ref(), cover_pixels.as_mut(), screen.as_mut())
                && let Some(bytes) = cover.image()
            {
                cover_waiting = false;
                // **The redraw has to be told, because the gathered state may not have moved.**
                // Every cover this phone sends is 200x200, so the second one leaves `Overview`
                // exactly as the first did -- and a screen that only redraws on a change would
                // keep showing the old sleeve. The same line covers a picture that failed to
                // decode, which leaves the frame cleared and wanting whatever was there before.
                shown = None;
                if heap_room() < cover::DECODE_HEAP && running.is_some() {
                    stop_plugin(&mut running, &mut page, &mut state.plugins);
                    info!("Cover: the plugin was unloaded to make room for decoding");
                }
                let room = heap_room();
                if room < cover::DECODE_HEAP {
                    warn!(
                        "Cover: {room} bytes of heap in one region, {} needed -- not decoded",
                        cover::DECODE_HEAP
                    );
                } else if let Some(art) =
                    cover::show(screen, bytes, pixels, settings.cover.size(), None)
                {
                    state.backdrop = Some(Backdrop::Cover {
                        width: art.width,
                        height: art.height,
                    });
                }
            }

            // The knob. Our own encoder counts every detent, and where the user is decides what
            // it does: in the menus it walks the ring or turns the open setting, and on a face
            // with the knob right it goes to the plugin.
            //
            // **The volume is the other chip's while [`KNOB_VOLUME_AT_CHIP`] holds**: its own
            // encoder sits on the same shaft, and in mode 1 it works the volume by itself, so
            // no key goes over the wire. In the menus and on a face with the knob right it is
            // asked for mode 2 instead, so that the phone's volume does not follow a hand that
            // is walking a menu. Without the flag every detent on the player is a key we send.
            let detents = encoder.poll();
            if detents != 0 {
                state.detents += detents;
                turn_ours += detents;
                // Whether this turn is felt. Everywhere but the ring it is, a detent at a time.
                let mut felt = true;
                if let Some(menu) = state.menu.as_mut() {
                    // In the settings the knob belongs to the menu: it walks the ring, or it
                    // turns whatever the open dialog sets. For the orientation that is the screen
                    // itself, and About at the top of the ring is the mark that shows it.
                    // Clockwise, the way the panel controller turns the picture.
                    match menu.turn(detents) {
                        Outcome::Adjust {
                            id: SETTING_ORIENTATION,
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            let step = (settings.orientation as i32 + detents)
                                .rem_euclid(ORIENTATIONS as i32)
                                as u8;
                            settings.orientation = step;
                            state.orientation = step as usize;
                            if let Some(screen) = screen.as_mut() {
                                screen.set_orientation(step as usize);
                            }
                        }
                        // The theme shows itself the same way: the ring around its dialog is
                        // drawn in whichever one the knob has reached.
                        Outcome::Adjust {
                            id: SETTING_THEME,
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            settings.theme = settings.theme.turned(detents);
                            state.theme = settings.theme;
                        }
                        // And the brightness is the screen itself again, like the orientation.
                        Outcome::Adjust {
                            id: SETTING_BRIGHTNESS,
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            let brightness = settings.brightness.turned(detents);
                            // Against a stop the level stays put, and a click would say it moved.
                            felt = brightness != settings.brightness;
                            settings.brightness = brightness;
                            state.brightness = settings.brightness;
                            if let Some(backlight) = backlight.as_ref() {
                                backlight.set(settings.brightness);
                            }
                        }
                        // And the clicks are their own reading: the one under this detent comes
                        // after the drive is set, so it is already at the step just reached.
                        Outcome::Adjust {
                            id: SETTING_HAPTICS,
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            let haptics = settings.haptics.turned(detents);
                            felt = haptics != settings.haptics;
                            settings.haptics = haptics;
                            state.haptics = haptics;
                            set_haptics(&mut haptic, &mut i2c, haptics);
                        }
                        // The size shows at OK, where the cover is drawn again.
                        Outcome::Adjust {
                            id: SETTING_COVER,
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            settings.cover = settings.cover.turned(detents);
                            state.cover = settings.cover;
                        }
                        // Shows at once: the dialog stands on the cloud it is about.
                        Outcome::Adjust {
                            id: SETTING_MOTION,
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            settings.motion = settings.motion.turned(detents);
                            state.motion = settings.motion;
                        }
                        // So does the cloud's shape. At either end of a range nothing moves,
                        // and nothing clicks.
                        Outcome::Adjust {
                            id:
                                id @ (SETTING_POINTS | SETTING_BRIGHTEST | SETTING_CENTRE
                                | SETTING_ACCENT),
                            owner: Owner::Firmware,
                            detents,
                        } => {
                            let part = match id {
                                SETTING_POINTS => Part::Points,
                                SETTING_BRIGHTEST => Part::Brightest,
                                SETTING_CENTRE => Part::Centre,
                                _ => Part::Accent,
                            };
                            let shape = settings.shape.turned(part, detents);
                            felt = shape != settings.shape;
                            settings.shape = shape;
                            state.shape = shape;
                        }
                        // Two states, so every odd number of detents flips it. What it comes to
                        // happens at OK.
                        Outcome::Adjust {
                            id,
                            owner: Owner::Plugin,
                            detents,
                        } => {
                            if let Some((n, PluginSetting::Installed)) = PluginSetting::of(id)
                                && detents % 2 != 0
                                && let Some(view) = state.plugins[n].as_mut()
                            {
                                view.installed = !settings.installed(view.id);
                                settings.set_installed(view.id, view.installed);
                            }
                        }
                        // On the ring a click means the selection moved to another segment.
                        // Every detent moves it one entry, so this is a turn with nowhere to go:
                        // a menu that shows only its top entry.
                        // The share dialog has no value either: turning flips between its code and
                        // the same in words, for a computer without a camera.
                        Outcome::Adjust {
                            id: SETTING_SHARE,
                            owner: Owner::Firmware,
                            ..
                        } => state.share_text = !state.share_text,
                        // A QR code is no value to turn: the knob goes on to the next code and
                        // shows it straight away, so the codes can be walked without tapping.
                        Outcome::Adjust {
                            id,
                            owner: Owner::Firmware,
                            detents,
                        } if qr_index(id).is_some() => {
                            menu.dismiss();
                            felt = menu.turn(detents) == Outcome::Moved;
                            menu.open_selected();
                        }
                        Outcome::Nothing => felt = false,
                        _ => {}
                    }
                } else if shown_view(&state).is_some_and(|view| view.rights.contains(Rights::KNOB))
                {
                    // A face with the knob right gets the detents as events, and the other chip
                    // has been told to keep its volume out of it -- see [`companion_state`].
                    let event = if detents > 0 {
                        FaceEvent::Clockwise
                    } else {
                        FaceEvent::Anticlockwise
                    };
                    if let Some(face) = shown_plugin(&mut running, state.face) {
                        for _ in 0..detents.unsigned_abs().min(KNOB_EVENTS_MAX) {
                            deliver(face, event, companion.as_mut(), &mut state);
                        }
                    }
                } else if KNOB_VOLUME_AT_CHIP {
                    // The detent is the other chip's to act on and ours only to watch. Ask it
                    // where the volume ended up once the hand comes to rest -- this side has
                    // no other way to know, and the screen is showing that number.
                    if !volume_settling {
                        volume_before = state.volume;
                    }
                    next_status = Instant::now() + VOLUME_SETTLE;
                    volume_settling = true;
                } else {
                    let wanted = volume_owed + detents;
                    volume_owed = wanted.clamp(-VOLUME_PENDING_MAX, VOLUME_PENDING_MAX);
                    // Saying so rather than assuming it: a hand that outruns the link is the
                    // whole reason [`VOLUME_PENDING_MAX`] exists.
                    if wanted != volume_owed {
                        info!(
                            "Knob: turned faster than the link takes keys -- {} detents dropped",
                            (wanted - volume_owed).abs()
                        );
                    }
                }
                if felt {
                    click_ends = Some(click(
                        &mut haptic,
                        &mut i2c,
                        settings.haptics,
                        CLICK_DETENT,
                        state.menu.is_some(),
                    ));
                }
            }

            // What the knob owes the phone, one key per [`VOLUME_KEY_PERIOD`].
            if !KNOB_VOLUME_AT_CHIP
                && volume_owed != 0
                && Instant::now() >= next_volume_key
                && let Some(companion) = companion.as_mut()
            {
                next_volume_key = Instant::now() + VOLUME_KEY_PERIOD;
                if volume_asked == 0 {
                    volume_before = state.volume;
                }
                volume_asked += volume_owed.signum();
                companion.queue_key(if volume_owed > 0 {
                    QueueKey::VolumeUp
                } else {
                    QueueKey::VolumeDown
                });
                volume_owed -= volume_owed.signum();
                // Ask where it ended up once the last key is gone. **The volume pair is the
                // guarded one** -- the chip drops it unless its stored volume leaves room and
                // a word of its own state agrees -- so a turn that moves nothing is a real
                // outcome, and the status line is the only place it shows.
                if volume_owed == 0 {
                    next_status = Instant::now() + VOLUME_SETTLE;
                    volume_settling = true;
                }
            }

            // The screen.
            if Instant::now() >= next_touch {
                next_touch = Instant::now() + TOUCH_PERIOD;
                match touch.read(&mut i2c) {
                    Ok(report) => {
                        // One answer per contact. The controller names a gesture *while* the
                        // finger is still down and keeps naming it, so a reading acted on
                        // directly turns an ordinary tap into a handful of commands -- which is
                        // exactly what a play/pause that flips twice looks like from the outside.
                        // `Taps` keeps the rule, and times the long press the controller never
                        // reports.
                        match taps.press(&report, Instant::now().as_millis()) {
                            None => {}
                            // In the install dialog a long press is Cancel: the plugin waits in
                            // its slot, and the next boot asks again.
                            Some(Press::Hold(_)) if state.offer.is_some() => {
                                if let Some(offer) = &state.offer {
                                    info!(
                                        "Plugin: slot {} not accepted, asked again at boot",
                                        offer.slot
                                    );
                                }
                                offer_next(&mut state, &mut offers, home_menu, settings_menu);
                                click_ends = Some(click(
                                    &mut haptic,
                                    &mut i2c,
                                    settings.haptics,
                                    CLICK_TAP,
                                    true,
                                ));
                            }
                            // **A long press goes home, whatever is on the screen.** It is
                            // answered here, before any screen sees the finger, so that no
                            // screen -- and no plugin -- can take it away.
                            // From inside the menus too, since the settings one level below home
                            // say "hold for home" where OK stood. An open
                            // dialog is taken back first, as Cancel would: leaving it by a way
                            // that is neither OK nor Cancel must not keep a value half set.
                            // At home itself a long press opens the QR codes instead.
                            Some(Press::Hold(_)) if state.menu.as_ref().is_some_and(at_home) => {
                                if let Some(nav) = state.menu.as_mut() {
                                    nav.enter(&QR_MENU);
                                }
                                click_ends = Some(click(
                                    &mut haptic,
                                    &mut i2c,
                                    settings.haptics,
                                    CLICK_TAP,
                                    true,
                                ));
                                info!("Touch: held -- QR codes");
                            }
                            Some(Press::Hold(_)) => {
                                if let Some((id, _)) =
                                    state.menu.as_ref().and_then(Navigator::opened)
                                {
                                    settings = before;
                                    take_back(
                                        &settings,
                                        &mut state,
                                        backlight.as_ref(),
                                        &mut haptic,
                                        &mut i2c,
                                        screen.as_mut(),
                                    );
                                    info!("Settings: {id:?} taken back");
                                }
                                let entering = state.menu.is_none();
                                // The menu first, so the click that opens it is already one of
                                // its own and as short as the rest.
                                state.menu = Some(home(home_menu, settings_menu));
                                click_ends = Some(click(
                                    &mut haptic,
                                    &mut i2c,
                                    settings.haptics,
                                    CLICK_TAP,
                                    true,
                                ));
                                // The knob changes hands with the settings. Asking for it here
                                // rather than waiting for the next status report is what keeps
                                // a detent from reaching the phone's volume after the menu is up.
                                if entering {
                                    wanted_state = companion_state(&state);
                                    if let Some(companion) = companion.as_mut() {
                                        companion.set_state(wanted_state);
                                    }
                                }
                                info!("Touch: held -- home");
                            }
                            Some(Press::Tap(contact)) if state.menu.is_some() => {
                                // The menu is drawn in picture coordinates, so the finger goes
                                // there first: out of the mounting, then out of however the
                                // picture happens to stand.
                                let (x, y) = contact.in_view();
                                let (x, y) = screen
                                    .as_ref()
                                    .map_or((x, y), |screen| screen.picture_point(x, y));
                                let outcome = state
                                    .menu
                                    .as_mut()
                                    .map_or(Outcome::Nothing, |menu| menu.tap(Point::new(x, y)));
                                if outcome != Outcome::Nothing {
                                    click_ends = Some(click(
                                        &mut haptic,
                                        &mut i2c,
                                        settings.haptics,
                                        CLICK_TAP,
                                        state.menu.is_some(),
                                    ));
                                }
                                match outcome {
                                    Outcome::Open { id, .. } => {
                                        before = settings;
                                        info!("Settings: {id:?} open");
                                    }
                                    // OK is where a setting is decided, so it is where it is
                                    // written -- and only when it differs from what is kept.
                                    // Accepting writes one byte into the slot; the restart that
                                    // gives the plugin its place comes after the last offer. A
                                    // plugin removed earlier under the same id is installed
                                    // again, or it would stay off home.
                                    Outcome::Ok {
                                        id: SETTING_INSTALL,
                                        owner: Owner::Firmware,
                                    } => {
                                        if let Some(offer) = &state.offer
                                            && accept_slot(store.as_mut(), table, offer.slot)
                                        {
                                            offers.accepted += 1;
                                            if !settings.installed(offer.id) {
                                                settings.set_installed(offer.id, true);
                                                if let Some(store) = store.as_mut() {
                                                    save_settings(store, &settings);
                                                }
                                                stored = settings;
                                                info!(
                                                    "Plugin: slot {} was removed, installed again",
                                                    offer.slot
                                                );
                                            }
                                        }
                                        offer_next(
                                            &mut state,
                                            &mut offers,
                                            home_menu,
                                            settings_menu,
                                        );
                                    }
                                    Outcome::Cancel {
                                        id: SETTING_INSTALL,
                                        owner: Owner::Firmware,
                                        ..
                                    } => {
                                        if let Some(offer) = &state.offer {
                                            info!(
                                                "Plugin: slot {} not accepted, asked again at boot",
                                                offer.slot
                                            );
                                        }
                                        offer_next(
                                            &mut state,
                                            &mut offers,
                                            home_menu,
                                            settings_menu,
                                        );
                                    }
                                    Outcome::Ok {
                                        id: SETTING_FORGET,
                                        owner: Owner::Firmware,
                                    } => {
                                        settings.bond = None;
                                        if settings != stored {
                                            if let Some(store) = store.as_mut() {
                                                save_settings(store, &settings);
                                            }
                                            stored = settings;
                                        }
                                        state.bonded = None;
                                        forget.signal(());
                                        info!("BLE: the bonded phone is forgotten");
                                    }
                                    Outcome::Ok { id, .. } => {
                                        // Installing and removing happen here and nowhere
                                        // earlier, so Cancel has nothing to undo. The rings are
                                        // laid out at boot without gaps, so a plugin that comes
                                        // or goes is written first and then takes a restart.
                                        let rings_change =
                                            state.plugins.iter().flatten().any(|view| {
                                                settings.installed(view.id)
                                                    != stored.installed(view.id)
                                            });
                                        // The last cover's bytes are still there, so a new size
                                        // is one more decode rather than a wait for the next track.
                                        if settings.cover != stored.cover {
                                            cover_waiting = true;
                                        }
                                        if settings != stored {
                                            if let Some(store) = store.as_mut() {
                                                save_settings(store, &settings);
                                            }
                                            stored = settings;
                                        }
                                        info!("Settings: {id:?} kept");
                                        if rings_change {
                                            info!(
                                                "Plugin: installed plugins changed -- restarting"
                                            );
                                            software_reset();
                                        }
                                    }
                                    Outcome::Cancel { id, .. } => {
                                        settings = before;
                                        take_back(
                                            &settings,
                                            &mut state,
                                            backlight.as_ref(),
                                            &mut haptic,
                                            &mut i2c,
                                            screen.as_mut(),
                                        );
                                        info!("Settings: {id:?} taken back");
                                    }
                                    Outcome::Screen { id, .. } => {
                                        state.face = Face::of(id);
                                        if let Some(n) = shown_index(state.face) {
                                            start_plugin(
                                                n,
                                                &modules,
                                                &mut running,
                                                &mut page,
                                                &mut state.plugins,
                                            );
                                        }
                                        state.menu = None;
                                        info!("Home: {:?} on the screen", state.face);
                                    }
                                    // Only a top menu with an OK closes, and the firmware's top
                                    // menu is home, which has none.
                                    Outcome::Close => state.menu = None,
                                    // A QR code has no buttons; a tap anywhere on it closes it.
                                    Outcome::Touch {
                                        id,
                                        owner: Owner::Firmware,
                                        ..
                                    } if qr_index(id).is_some() || id == SETTING_SHARE => {
                                        if let Some(nav) = state.menu.as_mut() {
                                            nav.dismiss();
                                        }
                                    }
                                    Outcome::Nothing
                                    | Outcome::Moved
                                    | Outcome::Adjust { .. }
                                    | Outcome::Touch { .. } => {}
                                }
                                // The knob changes hands with the menus, as it did when they
                                // opened.
                                if state.menu.is_none() {
                                    wanted_state = companion_state(&state);
                                    if let Some(companion) = companion.as_mut() {
                                        companion.set_state(wanted_state);
                                    }
                                }
                            }
                            // A face's screen is the face's: the tap goes to it, not to the player.
                            Some(Press::Tap(_)) if state.face != Face::Player => {
                                click_ends = Some(click(
                                    &mut haptic,
                                    &mut i2c,
                                    settings.haptics,
                                    CLICK_TAP,
                                    false,
                                ));
                                if let Some(face) = shown_plugin(&mut running, state.face) {
                                    deliver(face, FaceEvent::Tap, companion.as_mut(), &mut state);
                                }
                            }
                            Some(Press::Tap(_)) => {
                                click_ends = Some(click(
                                    &mut haptic,
                                    &mut i2c,
                                    settings.haptics,
                                    CLICK_TAP,
                                    state.menu.is_some(),
                                ));
                                if let Some(companion) = companion.as_mut() {
                                    companion.toggle_playback();
                                }
                                info!("Touch: a tap -- asking the other chip to toggle playback");
                            }
                            // And so is every wipe, in the picture's directions: which way the
                            // screen stands is the firmware's business, not the face's.
                            Some(Press::Gesture(gesture))
                                if state.menu.is_none() && state.face != Face::Player =>
                            {
                                let quarters = screen.as_ref().map_or(0, Screen::picture_quarter);
                                let event = match gesture.in_picture_mount().in_picture(quarters) {
                                    Gesture::SlideLeft => Some(FaceEvent::WipeLeft),
                                    Gesture::SlideRight => Some(FaceEvent::WipeRight),
                                    Gesture::SlideUp => Some(FaceEvent::WipeUp),
                                    Gesture::SlideDown => Some(FaceEvent::WipeDown),
                                    other => {
                                        info!("Touch: {other:?}, nothing a face hears");
                                        None
                                    }
                                };
                                if let (Some(event), Some(face)) =
                                    (event, shown_plugin(&mut running, state.face))
                                {
                                    click_ends = Some(click(
                                        &mut haptic,
                                        &mut i2c,
                                        settings.haptics,
                                        CLICK_TAP,
                                        false,
                                    ));
                                    deliver(face, event, companion.as_mut(), &mut state);
                                }
                            }
                            Some(Press::Gesture(gesture)) => {
                                // A gesture is named in the frame the panel is mounted in, and has
                                // to be turned twice before it means what the user did: once for
                                // the mounting, once for however the picture happens to stand.
                                let quarters = screen.as_ref().map_or(0, Screen::picture_quarter);
                                match gesture.in_picture_mount().in_picture(quarters) {
                                    // **Left is the previous track**, the way a timeline reads
                                    // rather than the way a carousel does; judged at the screen
                                    // with the USB socket pointing away.
                                    //
                                    // `A3 03` and not `A3 04`: the key that works is the one that
                                    // goes through the other chip's own dispatcher and out as an
                                    // AVRCP passthrough. The HID ids of `A3 04` leave over BLE
                                    // HID instead, to whoever is paired with `TAIJI_KNOB_HID` --
                                    // which is nobody.
                                    Gesture::SlideLeft if state.menu.is_none() => {
                                        click_ends = Some(click(
                                            &mut haptic,
                                            &mut i2c,
                                            settings.haptics,
                                            CLICK_TAP,
                                            state.menu.is_some(),
                                        ));
                                        if let Some(companion) = companion.as_mut() {
                                            companion.queue_key(QueueKey::Previous);
                                        }
                                        info!("Touch: swiped left -- previous track");
                                    }
                                    Gesture::SlideRight if state.menu.is_none() => {
                                        click_ends = Some(click(
                                            &mut haptic,
                                            &mut i2c,
                                            settings.haptics,
                                            CLICK_TAP,
                                            state.menu.is_some(),
                                        ));
                                        if let Some(companion) = companion.as_mut() {
                                            companion.queue_key(QueueKey::Next);
                                        }
                                        info!("Touch: swiped right -- next track");
                                    }
                                    // Up and down were the page axis until the long press took
                                    // over, and are free now. In the settings nothing slides:
                                    // the knob and the tap do the work there.
                                    other => info!("Touch: {other:?}, nothing bound to it"),
                                }
                            }
                        }
                    }
                    Err(err) => warn!("Touch: the screen did not answer: {err:?}"),
                }
            }

            // A face that listens to the radio: the loops speed up while it is on the screen, and
            // it is handed every round as it comes in.
            let listening = state.menu.is_none()
                && shown_view(&state).is_some_and(|view| view.rights.contains(Rights::RADIO));
            nearby::set_wanted(listening);
            if listening {
                let round = nearby::round();
                if handed_round != Some(round)
                    && let Some(face) = shown_plugin(&mut running, state.face)
                {
                    deliver(face, FaceEvent::Nearby, companion.as_mut(), &mut state);
                    handed_round = Some(round);
                }
            } else {
                handed_round = None;
            }

            // A face that sends to the phone is told whether one is connected, once when it comes
            // up and again on every change: until then it could only say how to pair.
            let sending = state.menu.is_none()
                && shown_view(&state).is_some_and(|view| view.rights.contains(Rights::HID));
            if sending {
                let linked = state.hid;
                if handed_link != Some(linked)
                    && let Some(face) = shown_plugin(&mut running, state.face)
                {
                    let event = if linked {
                        FaceEvent::Linked
                    } else {
                        FaceEvent::Unlinked
                    };
                    deliver(face, event, companion.as_mut(), &mut state);
                    handed_link = Some(linked);
                }
            } else {
                handed_link = None;
            }

            // A face's pulse: the firmware keeps the time, since a face has none, and only while
            // the face is on the screen. The interval is the face's latest, so a pulse that
            // speeds up does so from the last click and not from where the old interval ended.
            let pulse = match state.menu {
                None => shown_plugin(&mut running, state.face).and_then(|face| face.pulse()),
                Some(_) => None,
            };
            match pulse {
                Some(every) => {
                    let now = Instant::now();
                    let every = Duration::from_millis(u64::from(every));
                    if last_pulse.is_none_or(|last| now >= last + every) {
                        click_ends = Some(click(
                            &mut haptic,
                            &mut i2c,
                            settings.haptics,
                            CLICK_PULSE,
                            false,
                        ));
                        last_pulse = Some(now);
                    }
                }
                None => last_pulse = None,
            }

            // A run nobody could see is not spent: coming back to the player starts it again.
            let now = Instant::now();
            let on_player = state.menu.is_none() && state.face == Face::Player;
            if on_player && !player_up {
                title_run = Run::new(&state);
            }
            if on_player {
                title_run.elapsed += (now - run_pass).min(RUN_PERIOD);
            }
            (player_up, run_pass) = (on_player, now);
            state.run = title_run.offset();

            // The cloud moves wherever it is the ground: in the menus, and on the player
            // until a cover arrives. A transfer holds it still, because a pass with a transfer
            // in it ends above.
            let on_cloud =
                state.menu.is_some() || (state.face == Face::Player && state.backdrop.is_none());
            // A cloud frame is drawn synchronously for some 47 ms, which starves the network
            // stack: while a file goes out over Wi-Fi the cloud holds still.
            state.cloud = match state.motion == Motion::Moving && on_cloud && !share::busy() {
                true => (now.as_millis() / CLOUD_FRAME.as_millis()) as u32 + 1,
                false => 0,
            };

            state.uptime = Instant::now().as_secs() as u32;
            state.networks = WIFI_NETWORKS.load(Ordering::Relaxed);
            state.peer = PEER_CONNECTED.load(Ordering::Relaxed);

            // Uploads are taken only while the receive dialog is open. Closing it drops one
            // under way, and its slot stays empty.
            let receiving = matches!(
                state.menu.as_ref().and_then(Navigator::opened),
                Some((SETTING_RECEIVE, Owner::Firmware))
            );
            RECEIVING.store(receiving, Ordering::Relaxed);
            // The access point runs while the card's dialog is open, and only with a card.
            let sharing = matches!(
                state.menu.as_ref().and_then(Navigator::opened),
                Some((SETTING_SHARE, Owner::Firmware))
            );
            if !sharing {
                state.share_text = false;
            }
            if sharing {
                // The page a client fetches wears the ring's colours.
                share::set_palette(state.theme.palette());
            }
            share::OPEN.store(sharing && state.card.is_some(), Ordering::Relaxed);
            if !receiving {
                if let Some(dropped) = upload.take() {
                    info!("Plugin: upload into slot {} dropped", dropped.slot());
                }
                state.received = Received::default();
            }
            let kept = state
                .plugins
                .iter()
                .flatten()
                .enumerate()
                .fold(0u16, |bits, (k, view)| {
                    bits | u16::from(stored.installed(view.id)) << k
                });
            if listed != Some(kept) {
                critical_section::with(|cs| {
                    PLUGIN_LIST.borrow_ref_mut(cs).fill(&state.plugins, &stored);
                });
                listed = Some(kept);
            }
            // Settings a sender wrote stand at once, like an OK on the glass, and an open
            // dialog's Cancel keeps them.
            if let Some(written) = setting_writes.try_take() {
                settings.set_remote(written);
                before.set_remote(written);
                stored.set_remote(written);
                take_back(
                    &settings,
                    &mut state,
                    backlight.as_ref(),
                    &mut haptic,
                    &mut i2c,
                    screen.as_mut(),
                );
                if let Some(store) = store.as_mut() {
                    save_settings(store, &stored);
                }
                info!("Settings: written over BLE");
            }
            let shared = stored.remote().encode();
            critical_section::with(|cs| SHARED_SETTINGS.borrow(cs).set(shared));
            // Encrypting with a known bond reports it again; only a new one is written.
            if let Some(bond) = bonds.try_take()
                && stored.bond != Some(bond)
            {
                stored.bond = Some(bond);
                settings.bond = Some(bond);
                state.bonded = Some(bond.address);
                if let Some(store) = store.as_mut() {
                    save_settings(store, &stored);
                }
            }
            while let Ok(command) = uploads.try_receive() {
                if !receiving {
                    continue;
                }
                state.received = match command {
                    // Kept as removed in the settings, so a bundled plugin the slot had replaced
                    // comes back off home.
                    Command::Delete(slot) => {
                        let slot = usize::from(slot);
                        match delete_slot(store.as_mut(), table, &state.plugins, slot) {
                            Ok(id) => {
                                if let Some(dropped) = upload.take() {
                                    info!("Plugin: upload into slot {} dropped", dropped.slot());
                                }
                                settings.set_installed(id, false);
                                if let Some(store) = store.as_mut() {
                                    save_settings(store, &settings);
                                }
                                stored = settings;
                                Received {
                                    status: UploadStatus::Deleted,
                                    slot,
                                    ..Received::default()
                                }
                            }
                            Err(status) => Received {
                                status,
                                ..Received::default()
                            },
                        }
                    }
                    command => receive(store.as_mut(), table, &mut upload, command),
                };
                upload_status.signal(
                    state
                        .received
                        .status
                        .encode(state.received.slot, state.received.received),
                );
                if matches!(
                    state.received.status,
                    UploadStatus::Written | UploadStatus::Deleted
                ) {
                    restart_at = Some(Instant::now() + UPLOAD_RESTART);
                }
            }
            if restart_at.is_some_and(|at| Instant::now() >= at) {
                info!("Plugin: slots changed -- restarting");
                software_reset();
            }

            if shown.as_ref() != Some(&state) {
                if let Some(screen) = screen.as_mut() {
                    // How long the cloud took, if this frame stands on it.
                    let drawing = Instant::now();
                    let mut ground = None;
                    // Whether everything outside the ring is still the black a menu left there.
                    let ring_clean = shown.as_ref().is_some_and(|last| last.menu.is_some());
                    match state.menu.as_ref() {
                        // A plugin's face stands on black and draws the rest itself -- or, once
                        // the plugin is stopped, the firmware says so in its place.
                        None if state.face != Face::Player => {
                            // As in the menus: a click ends before the picture goes out, or it
                            // lasts as long as the send. A face's pulse is felt for its
                            // strength, so every click of it has to be the same length.
                            if let Some(ends) = click_ends.take() {
                                Timer::at(ends).await;
                                let _ = haptic.stop(&mut i2c);
                            }
                            screen.frame().clear(Rgb565::BLACK).ok();
                            let palette = state.theme.palette();
                            let face = shown_plugin(&mut running, state.face);
                            let drawn = match face {
                                Some(face) => {
                                    let drawn = face.draw(screen.frame(), palette);
                                    let fault = face.fault().map(String::from);
                                    if let Some(view) = shown_index(state.face)
                                        .and_then(|n| state.plugins[n].as_mut())
                                    {
                                        view.fault = fault;
                                    }
                                    drawn
                                }
                                None => false,
                            };
                            if !drawn {
                                stopped_screen(screen.frame(), &state);
                            }
                            home_hint(screen.frame(), &state);
                        }
                        None => {
                            // A frame starts from whatever ground it has: the cover or the
                            // picture off the card if one was read into the backdrop, the cloud
                            // otherwise. `restore` only says whether there is a second screen in
                            // the external RAM to copy, not whether anything was ever put in it
                            // -- that is what `state.backdrop` knows, and without it the first
                            // frame would show uninitialised PSRAM.
                            let over_picture = state.backdrop.is_some() && screen.restore();
                            if !over_picture {
                                ground = Some(cloud_ground(screen.frame(), &state, false, false));
                            }
                            status_screen(screen.frame(), &state, over_picture);
                            home_hint(screen.frame(), &state);
                        }
                        // Home and the settings stand on the cloud, cover or not. It is dark in
                        // the middle, where the names, the values and a dialog are read.
                        Some(nav) => {
                            // A click in here ends before the picture goes out, not after: the
                            // send blocks for longer than [`CLICK_LENGTH_MENU`], and the loop
                            // would only come back to cut it once the send is done.
                            if let Some(ends) = click_ends.take() {
                                Timer::at(ends).await;
                                let _ = haptic.stop(&mut i2c);
                            }
                            ground = Some(cloud_ground(screen.frame(), &state, true, ring_clean));
                            settings_screen(
                                screen.frame(),
                                &state,
                                nav,
                                ring.as_ref(),
                                &credentials,
                                share_code.as_ref(),
                            );
                        }
                    }
                    // Sending the picture blocks: 14 ms standing upright, 42 ms turned. At one
                    // redraw a second that is a hole the executor can live with, and it is the
                    // reason this is driven by change rather than by a frame rate. The moving
                    // cloud is the exception, and [`CloudMeter`] says what it costs.
                    if probes > 0 {
                        probes -= 1;
                        let bytes = screen.frame().bytes();
                        let at = bytes.as_ptr();
                        let len = bytes.len();
                        if let Some((cached, external)) = teetotum::display::external_probe(bytes) {
                            info!(
                                "Screen: picture at {at:p}, {len} bytes -- cached {cached:#010x}, \
                                 external {external:#010x}, the external RAM {}",
                                if cached == external {
                                    "holds it"
                                } else {
                                    "does not hold it"
                                }
                            );
                        }
                    }
                    let sending = Instant::now();
                    let sent = screen.present();
                    if let Some(ground) = ground {
                        cloud_meter.add(ground, drawing.elapsed(), sending.elapsed());
                    }
                    match sent {
                        Ok(()) if !lit => {
                            if let Some(backlight) = backlight.as_ref() {
                                backlight.set(state.brightness);
                                info!("Display: backlight at {} %", state.brightness.percent());
                            }
                            lit = true;
                            if let Some(hold) = FIRST_FRAME_HOLD {
                                info!(
                                    "Display: holding the first picture for {} s, path {:?}",
                                    hold.as_secs(),
                                    SCREEN_PATH
                                );
                                Timer::after(hold).await;
                                info!("Display: first picture held, carrying on");
                            }
                        }
                        Ok(()) => {}
                        Err(err) => error!("Display: sending the picture failed: {err:?}"),
                    }
                    // A phone camera reads a code best from a bright screen, so a shown QR code
                    // gets full brightness, and the stored level returns with the picture after it.
                    let qr_shown = state.menu.as_ref().and_then(Navigator::opened).is_some_and(
                        |(id, owner)| {
                            owner == Owner::Firmware
                                && (qr_index(id).is_some()
                                    || id == SETTING_SHARE
                                        && state.card.is_some()
                                        && share_code.is_some()
                                        && !state.share_text)
                        },
                    );
                    if lit && qr_shown != qr_lit {
                        if let Some(backlight) = backlight.as_ref() {
                            backlight.set(if qr_shown {
                                Brightness::MAX
                            } else {
                                state.brightness
                            });
                        }
                        qr_lit = qr_shown;
                    }
                }
                shown = Some(state.clone());
            }

            Timer::after(INPUT_PERIOD).await;
        }
    };

    // The card over Wi-Fi. Its sockets wait for ever and only hear something while the access
    // point runs.
    let share_area = share_area.unwrap_or_else(|| {
        warn!("Share: no external RAM to spare -- the buffers take the heap");
        alloc::vec![0u8; share::BUFFER_BYTES].leak()
    });
    let card_share = join(
        net_runner.run(),
        share::serve(net_stack, share_area, volume),
    );

    join5(
        runner.run_with_handler(&scan_log),
        scanning,
        advertising,
        device,
        card_share,
    )
    .await;

    // `scanning` never returns, so getting here means the BLE runner gave up.
    panic!("the BLE host stopped running");

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.1.0/examples
}

/// Logs what a cover transfer just did, and notes when one is ready to be decoded.
///
/// A transfer says more than a firmware needs to show -- packet by packet it is a log line and
/// nothing else. Only the end of it changes what is on the screen, and that is the flag.
fn report(step: CoverStep, waiting: &mut bool) {
    match step {
        CoverStep::Offered { id, packets } => {
            info!("Cover: offered as id {id}, {packets} packets");
        }
        CoverStep::Packet { .. } => {}
        CoverStep::Complete { bytes } => {
            info!("Cover: {bytes} bytes in");
            *waiting = true;
        }
        CoverStep::Refused { reason } => warn!("Cover: the other chip refused, reason {reason}"),
        CoverStep::Silent { bytes } => {
            warn!("Cover: no answer to the request, after {bytes} bytes");
        }
    }
}

/// Run a calibration and say what came back.
///
/// Returns `None` when the chip refused; the caller has nothing worth storing then, and a boot
/// with an uncalibrated driver still clicks -- just not as well.
fn calibrate(
    haptic: &mut Haptic<'_>,
    i2c: &mut I2c<'_, Blocking>,
    delay: &Delay,
    time: CalTime,
) -> Option<teetotum::haptic::Calibration> {
    match haptic.auto_calibrate(i2c, delay, time) {
        Ok(cal) => {
            info!(
                "Haptic: calibrated, compensation {:#04x}, back-EMF {:#04x}, passed {}",
                cal.compensation, cal.back_emf, cal.passed
            );
            Some(cal)
        }
        Err(err) => {
            error!("Haptic: calibration failed: {err:?}");
            None
        }
    }
}

/// Starts a click and says when it is to be cut off: [`CLICK_LENGTH_MENU`] in the settings,
/// [`CLICK_LENGTH`] everywhere else.
///
/// [`Haptic::play`] waits for the chip to report itself finished, which is right for a run that
/// is judging effects by hand and wrong here: a brisk turn of the knob delivers thirty detents,
/// and thirty waits would be most of a second spent inside the input loop. A click that arrives
/// while the previous one is still playing cuts it short -- which is what a fast turn ought to
/// feel like anyway.
///
/// **Off is no click at all**, not a click driven at nothing: what a zero clamp does in closed
/// loop is the datasheet's word, and not starting one needs nobody's.
fn click(
    haptic: &mut Haptic<'_>,
    i2c: &mut I2c<'_, Blocking>,
    haptics: Haptics,
    effect: u8,
    in_menu: bool,
) -> Instant {
    if haptics != Haptics::OFF {
        let _ = haptic.set_sequence(i2c, &[effect]);
        let _ = haptic.go(i2c);
    }
    Instant::now()
        + if in_menu {
            CLICK_LENGTH_MENU
        } else {
            CLICK_LENGTH
        }
}

/// Sets the drive the next clicks get, from [`DRIVE`]. Off leaves it where it was, because
/// [`click`] does not start one then.
fn set_haptics(haptic: &mut Haptic<'_>, i2c: &mut I2c<'_, Blocking>, haptics: Haptics) {
    if let Some(&(rated, clamp)) = (haptics.step() as usize)
        .checked_sub(1)
        .and_then(|n| DRIVE.get(n))
    {
        let _ = haptic.set_drive(i2c, rated, clamp);
    }
}

/// The names in [`BACKGROUND_FOLDER`] that are a whole screen, in the order the card lists them.
///
/// Anything else in the folder is passed over without a word: a card is the user's, and what is
/// on it is none of the firmware's business beyond what it can show.
#[expect(
    clippy::large_stack_frames,
    reason = "`fat::Entries` is 800 bytes; card backgrounds are switched off by `CARD_BACKGROUNDS`"
)]
fn background_names(volume: &mut Volume<'_>) -> Vec<String> {
    let mut names = Vec::new();
    let dir = match volume.dir(BACKGROUND_FOLDER) {
        Ok(dir) => dir,
        Err(err) => {
            info!("Card: no {BACKGROUND_FOLDER} to take a background from ({err:?})");
            return names;
        }
    };

    let mut entries = volume.entries(dir);
    loop {
        match entries.next(volume) {
            Ok(Some(entry)) => {
                if entry.directory || header_bytes(entry.size).is_none() {
                    continue;
                }
                names.push(String::from(entry.name()));
                if names.len() == MAX_BACKGROUNDS {
                    break;
                }
            }
            Ok(None) => break,
            Err(err) => {
                warn!("Card: {BACKGROUND_FOLDER} could not be walked to the end: {err:?}");
                break;
            }
        }
    }
    info!(
        "Card: {} background{} in {BACKGROUND_FOLDER}",
        names.len(),
        if names.len() == 1 { "" } else { "s" }
    );
    names
}

/// How many bytes come before the pixels, if this size can be a screen at all.
///
/// The demo's files are 259204 bytes for 259200 of picture: four bytes of header, which read as
/// a width and a height. A file written without one is just as welcome.
fn header_bytes(size: u32) -> Option<usize> {
    match size as usize {
        n if n == SCREEN_BYTES => Some(0),
        n if n == SCREEN_BYTES + 4 => Some(4),
        _ => None,
    }
}

/// Reads one background off the card into the backdrop, and names it if it got there.
///
/// The pixels go from the card straight into external RAM: the demo's files are RGB565 with the
/// high byte first, which is the panel's order and this firmware's, so there is no pass over
/// them. It costs about 130 ms, which is why it happens on a swipe and not on a frame.
#[expect(
    clippy::large_stack_frames,
    reason = "a `fat::File` result is 536 bytes; card backgrounds are switched off by `CARD_BACKGROUNDS`"
)]
fn load_background(volume: &mut Volume<'_>, screen: &mut Screen<'_>, name: &str) -> Option<String> {
    let backdrop = screen.backdrop_mut()?;
    let path = format!("{BACKGROUND_FOLDER}/{name}");

    let mut file = match volume.open(&path) {
        Ok(file) => file,
        Err(err) => {
            error!("Card: {path} could not be opened: {err:?}");
            return None;
        }
    };
    let header = header_bytes(file.size())?;
    if header > 0
        && let Err(err) = file.seek(volume, header as u32)
    {
        error!("Card: {path} could not be stepped past its header: {err:?}");
        return None;
    }

    let began = Instant::now();
    let mut done = 0usize;
    while done < SCREEN_BYTES {
        match file.read(volume, &mut backdrop[done..]) {
            Ok(0) => break,
            Ok(taken) => done += taken,
            Err(err) => {
                error!("Card: {path} stopped after {done} bytes: {err:?}");
                return None;
            }
        }
    }
    if done < SCREEN_BYTES {
        // Half a picture behind the text is worse than none: it would be half fresh and half
        // whatever the external RAM held.
        warn!("Card: {path} gave only {done} of {SCREEN_BYTES} bytes -- not using it");
        return None;
    }

    let millis = began.elapsed().as_millis().max(1);
    info!("Card: {path} into the backdrop, {done} bytes in {millis} ms");
    Some(String::from(name))
}

/// Starts plugin `n` for its face: loads it, after unloading whichever ran before.
///
/// **One plugin runs at a time.** Loading every installed plugin at boot left the two bundled
/// ones with only 30 768 bytes of the internal heap: the home
/// menu has nine places for faces, the heap had room for one or two more. Only the face on the
/// screen needs its plugin, so that is the one loaded. The price is a load whenever another face
/// is started, 15 to 44 ms so far, and a plugin that begins afresh after another has run.
///
/// **The one that ran last stays loaded** until another plugin is started or it is removed:
/// going home and back to the same face finds it as it was left, and starting the player does
/// not unload it. A plugin that has been stopped is loaded anew, which is the way back from a
/// trap.
#[expect(
    clippy::large_stack_frames,
    reason = "a `Plugin` is 968 bytes and moves by value, and boxing it would spend internal heap, which runs out first; the main stack has room, see the note at the top"
)]
fn start_plugin(
    n: usize,
    modules: &Modules,
    running: &mut Option<(usize, Plugin)>,
    page: &mut Option<Page>,
    views: &mut [Option<PluginView>],
) {
    if running
        .as_ref()
        .is_some_and(|(m, plugin)| *m == n && plugin.fault().is_none())
    {
        return;
    }
    let Some(wasm) = modules.get(n).copied().flatten() else {
        return;
    };
    stop_plugin(running, page, views);
    let outcome = load_plugin(wasm, page);
    let Some(view) = views[n].as_mut() else {
        return;
    };
    match outcome {
        Ok((plugin, cost)) => {
            *running = Some((n, plugin));
            view.loaded = Some(cost);
            view.fault = None;
        }
        Err(why) => {
            view.loaded = None;
            view.fault = Some(why);
        }
    }
}

/// The most internal heap free in any one region: an allocation cannot span two.
fn heap_room() -> usize {
    esp_alloc::HEAP
        .stats()
        .region_stats
        .iter()
        .flatten()
        .map(|region| region.free)
        .max()
        .unwrap_or(0)
}

/// Unloads the plugin that is running, if one is, and takes its page back for the next.
#[expect(
    clippy::large_stack_frames,
    reason = "a `Plugin` is 968 bytes and moves by value, and boxing it would spend internal heap, which runs out first; the main stack has room, see the note at the top"
)]
fn stop_plugin(
    running: &mut Option<(usize, Plugin)>,
    page: &mut Option<Page>,
    views: &mut [Option<PluginView>],
) {
    let Some((n, plugin)) = running.take() else {
        return;
    };
    *page = Some(plugin.unload());
    if let Some(view) = views[n].as_mut() {
        view.loaded = None;
        view.fault = None;
    }
    info!(
        "Plugin: {n} unloaded, heap {} bytes free",
        esp_alloc::HEAP.free()
    );
}

/// Loads a plugin into the page, if the page is free, and says what that cost:
/// microseconds, and bytes of internal heap -- or why it was refused, in words for the screen.
#[expect(
    clippy::large_stack_frames,
    reason = "a `Plugin` is 968 bytes and moves by value, and boxing it would spend internal heap, which runs out first; the main stack has room, see the note at the top"
)]
fn load_plugin(
    wasm: &'static [u8],
    page: &mut Option<Page>,
) -> Result<(Plugin, (u64, usize)), String> {
    let Some(free) = page.take() else {
        error!("Plugin: no page of external RAM to run it in");
        return Err(String::from("no memory to run in"));
    };
    let heap = esp_alloc::HEAP.used();
    let began = Instant::now();
    match Plugin::load(wasm, free) {
        Ok(plugin) => {
            let took = began.elapsed().as_micros();
            let cost = esp_alloc::HEAP.used().saturating_sub(heap);
            info!(
                "Plugin: {} loaded in {took} us, heap +{cost} bytes, {} free",
                plugin.manifest().name(),
                esp_alloc::HEAP.free()
            );
            Ok((plugin, (took, cost)))
        }
        Err((e, free)) => {
            error!("Plugin: refused -- {e}");
            *page = Some(free);
            Err(format!("{e}"))
        }
    }
}

/// The home menu, on Home, with the settings behind the gear left of it. It holds only the
/// installed plugins, laid out at boot; a plugin that is refused when its face is started keeps
/// its segment, and the face says why.
fn home(menu: &'static Menu, settings: &'static Menu) -> Navigator {
    Navigator::home(menu, settings)
}

/// Which bundled plugin's face `face` is, if it is one.
fn shown_index(face: Face) -> Option<usize> {
    match face {
        Face::Plugin(n) => Some(usize::from(n)),
        Face::Player => None,
    }
}

/// The plugin whose face is on the screen, if one is and it is the one running.
fn shown_plugin(running: &mut Option<(usize, Plugin)>, face: Face) -> Option<&mut Plugin> {
    let n = shown_index(face)?;
    match running {
        Some((m, plugin)) if *m == n => Some(plugin),
        _ => None,
    }
}

/// What the settings know about the plugin whose face is on the screen, if one is.
fn shown_view(state: &Overview) -> Option<&PluginView> {
    state.plugins.get(shown_index(state.face)?)?.as_ref()
}

/// What the other chip is asked to do with its encoder: work the volume, unless the settings are
/// open or a face with the knob right is shown -- then the detents are this side's alone.
fn companion_state(state: &Overview) -> u8 {
    let face_has_knob = shown_view(state).is_some_and(|view| view.rights.contains(Rights::KNOB));
    if state.menu.is_some() || face_has_knob {
        COMPANION_STATE_SETTINGS
    } else {
        COMPANION_STATE
    }
}

/// Hands an event to the plugin and carries out its answer: the usages go to the other chip, and
/// a wish to be drawn again goes into the state the redraw is decided on.
fn deliver(
    plugin: &mut Plugin,
    event: FaceEvent,
    companion: Option<&mut Companion<'_>>,
    state: &mut Overview,
) {
    let reply = plugin.event(event);
    if let Some(companion) = companion {
        for usage in &reply.usages {
            companion.media_key(media_key(*usage));
        }
    }
    info!("Plugin: {event:?} -> {:?}", reply.usages);
    if reply.redraw {
        state.plugin_frame = state.plugin_frame.wrapping_add(1);
    }
    // The face on the screen is the one events go to, so it is the one to report on.
    if let Some(view) = shown_index(state.face).and_then(|n| state.plugins[n].as_mut()) {
        view.fault = plugin.fault().map(String::from);
    }
}

/// A usage a face sent, as the key the other chip takes. Both lists are the ten usages its HID
/// report was read to carry, so this is a renaming and cannot miss.
fn media_key(usage: Usage) -> MediaKey {
    match usage {
        Usage::Power => MediaKey::Power,
        Usage::Play => MediaKey::Play,
        Usage::Pause => MediaKey::Pause,
        Usage::Record => MediaKey::Record,
        Usage::FastForward => MediaKey::FastForward,
        Usage::Rewind => MediaKey::Rewind,
        Usage::Next => MediaKey::Next,
        Usage::Previous => MediaKey::Previous,
        Usage::Stop => MediaKey::Stop,
        Usage::PlayPause => MediaKey::PlayPause,
    }
}

/// What a plugin's face shows once the plugin has been stopped: which one, why, and the way out.
fn stopped_screen(frame: &mut Framebuffer, state: &Overview) {
    let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
    let palette = state.theme.palette();
    let (name, why) = shown_view(state).map_or(("Plugin", ""), |view| {
        (view.name, view.fault.as_deref().unwrap_or(""))
    });
    let _ = menu_text(
        frame,
        name,
        centre + Point::new(0, -34),
        &fonts::LARGE,
        Rgb565::WHITE,
    );
    let _ = menu_text(frame, "stopped", centre, &fonts::BODY, palette.value);
    let _ = menu_text(
        frame,
        why,
        centre + Point::new(0, 28),
        &fonts::SMALL_LATIN1,
        palette.quiet,
    );
}

/// The way out, on every face: written after the face has drawn, so no face can cover it. The
/// long press belongs to the firmware, and so does saying so -- a face that had to say it itself
/// could also leave it out, and the two bundled ones did.
///
/// It stands in [`HINT`], the gap at the foot of the player's volume arc, which every face keeps
/// free.
fn home_hint(frame: &mut Framebuffer, state: &Overview) {
    let centre = Point::new((HINT.left + HINT.right) / 2, (HINT.top + HINT.bottom) / 2);
    let colour = state.theme.palette().selected;
    let _ = menu_text(
        frame,
        teetotum::menu::HOLD_FOR_HOME,
        centre,
        &fonts::SMALL,
        colour,
    );
}

/// Brings everything that follows a setting live back in line with `settings`: what Cancel does
/// after it has put the snapshot back, and a long press out of an open dialog with it.
fn take_back(
    settings: &Settings,
    state: &mut Overview,
    backlight: Option<&Backlight>,
    haptic: &mut Haptic<'_>,
    i2c: &mut I2c<'_, Blocking>,
    screen: Option<&mut Screen<'_>>,
) {
    state.orientation = settings.orientation as usize;
    state.theme = settings.theme;
    state.brightness = settings.brightness;
    if let Some(backlight) = backlight {
        backlight.set(settings.brightness);
    }
    state.haptics = settings.haptics;
    set_haptics(haptic, i2c, settings.haptics);
    state.cover = settings.cover;
    state.motion = settings.motion;
    state.shape = settings.shape;
    for view in state.plugins.iter_mut().flatten() {
        view.installed = settings.installed(view.id);
    }
    if let Some(screen) = screen {
        screen.set_orientation(state.orientation);
    }
}

/// Writes the settings back, and says what came of it.
///
/// **This erase happens with the radios up**, unlike the one at boot, and an erase stops the
/// world for milliseconds. That is why it is not done per detent: the settings page moves a
/// value as often as the hand likes, and this is called once on the way out, and only when
/// something really differs from what is down there.
fn save_settings<F: NorFlash>(store: &mut Store<F>, wanted: &Settings) {
    let mut out = [0u8; settings::LEN];
    let len = wanted.encode(&mut out);
    match store.save(&out[..len]) {
        Ok(()) => info!("Settings: written to slot {:?}", store.slot()),
        Err(e) => error!("Settings: could not be written: {e:?}"),
    }
}

/// Removes every bond the Bluetooth host holds; the settings are the caller's.
#[expect(
    clippy::large_stack_frames,
    reason = "the host hands its bonds over as a copy of its whole table, once, when a hand says so"
)]
fn forget_bonds<C, P>(stack: &Stack<'_, C, P>)
where
    C: Controller,
    P: PacketPool,
{
    for bond in stack.get_bond_information() {
        if let Err(err) = stack.remove_bond_information(bond.identity) {
            warn!("BLE: a bond was not removed -- {err:?}");
        }
    }
}

/// A bond from the settings, as the Bluetooth host takes it.
fn bond_information(bond: &Bond) -> BondInformation {
    BondInformation::new(
        Identity {
            bd_addr: BdAddr::new(bond.address),
            irk: bond
                .irk
                .map(|irk| IdentityResolvingKey::new(u128::from_le_bytes(irk))),
        },
        LongTermKey::from_le_bytes(bond.ltk),
        match bond.level {
            2 => SecurityLevel::EncryptedAuthenticated,
            _ => SecurityLevel::Encrypted,
        },
        true,
    )
}

/// A bond from the Bluetooth host, as the settings keep it.
fn stored_bond(info: &BondInformation) -> Bond {
    Bond {
        address: address_bytes(info.identity.bd_addr),
        ltk: info.ltk.to_le_bytes(),
        irk: info.identity.irk.map(|irk| irk.0.to_le_bytes()),
        level: match info.security_level {
            SecurityLevel::EncryptedAuthenticated => 2,
            _ => 1,
        },
    }
}

/// The six bytes of an address, least significant first.
fn address_bytes(address: BdAddr) -> [u8; 6] {
    <[u8; 6]>::try_from(address.raw()).unwrap_or_default()
}

/// A Bluetooth address, least significant byte first, written the usual way round.
struct AddressText<'a>(&'a [u8; 6]);

impl core::fmt::Display for AddressText<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (n, byte) in self.0.iter().rev().enumerate() {
            if n > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

/// Where the cloud's scattering starts, chosen by eye from a mockup of the backdrop.
const CLOUD_SEED: u32 = 0x7EE7_0701;

/// How long the last ground took, for the Background menu's About.
static CLOUD_GROUND_US: AtomicU32 = AtomicU32::new(0);

/// The ground of home, the settings and the player without a cover, shaped as the Background
/// menu says (the mockup's sliders, on the knob; see [`CloudShape`]). A plugin's face stays on
/// black.
fn cloud_of(shape: CloudShape) -> Cloud {
    Cloud {
        seed: CLOUD_SEED,
        points: usize::from(shape.points),
        // Percent to 256ths, rounded: 98 % is the 251 the mockup wrote.
        peak: (u16::from(shape.brightest) * 256 + 50) / 100,
        core: i32::from(shape.centre),
        accent: shape.accent,
    }
}

/// How long one frame of the moving cloud stands: 25 a second at most, and fewer if drawing
/// and sending take longer, which is what [`CloudMeter`] is there to say. At 50 ms the first
/// run made 12.9 a second: a pass that ends just before the next frame is due waits a whole
/// further pass for it.
const CLOUD_FRAME: Duration = Duration::from_millis(40);

/// How often [`CloudMeter`] reports.
const CLOUD_REPORT: Duration = Duration::from_secs(10);

/// Clears the picture to the cloud, in the theme's colours, and says how long that took.
///
/// **Under a menu only the disc inside the ring is cleared, and only its points are drawn** --
/// worth the trouble only once the cloud moves. The ring paints every pixel from
/// [`teetotum::menu::INNER`] out, its gaps included, so clearing and scattering there was work
/// nobody saw. The first run spent 13.2 ms clearing and 7.6 ms on points per frame.
///
/// `clean` says the frame before was a menu too. If it was not, whatever it left outside the
/// ring -- a cover, a face -- is cleared once with the rest: the ring stops at the pixel centres
/// 180 px out, and the edge of the screen shows a little past them (the user saw a cover there).
fn cloud_ground(frame: &mut Framebuffer, state: &Overview, ring: bool, clean: bool) -> Duration {
    let started = Instant::now();
    let reach = match ring {
        true => {
            match clean {
                true => clear_disc(frame, teetotum::menu::INNER),
                false => frame.clear(Rgb565::BLACK).unwrap_or(()),
            }
            teetotum::menu::INNER
        }
        false => {
            frame.clear(Rgb565::BLACK).ok();
            WIDTH as i32 / 2
        }
    };
    let moment =
        (state.cloud != 0).then(|| state.cloud.wrapping_mul(CLOUD_FRAME.as_millis() as u32));
    let _ = cloud_of(state.shape).draw_within(frame, state.theme.palette(), moment, reach);
    let spent = started.elapsed();
    CLOUD_GROUND_US.store(spent.as_micros() as u32, Ordering::Relaxed);
    spent
}

/// Clears every pixel nearer the centre than `radius`, a row at a time, with one pixel to spare
/// at either end of a row: whatever stands over the rim paints over the spare.
fn clear_disc(frame: &mut Framebuffer, radius: i32) {
    let centre = WIDTH as i32 / 2;
    for y in centre - radius - 1..centre + radius + 1 {
        // In half pixels, from the middle of the row to the middle of the screen.
        let dy = 2 * y + 1 - 2 * centre;
        let half = (4 * radius * radius - dy * dy).max(0).isqrt() / 2 + 1;
        let row = Rectangle::new(Point::new(centre - half, y), Size::new(2 * half as u32, 1));
        frame.fill_solid(&row, Rgb565::BLACK).ok();
    }
}

/// What the cloud costs, summed over [`CLOUD_REPORT`] and logged as one line.
///
/// **A moving cloud is the first frame rate in this firmware.** Until it, a redraw happened on
/// a change and blocked the executor for one send, 14 ms upright and 42 ms turned. Now it is
/// every pass, so the line also carries the worst frame: that is the hole the Bluetooth futures
/// and the receive FIFO of the other chip's UART have to live through.
struct CloudMeter {
    since: Instant,
    frames: u32,
    /// Clearing and the points.
    ground: Duration,
    /// Everything from the first pixel to the last byte sent: the ground, the ring or the
    /// player over it, and the send.
    whole: Duration,
    send: Duration,
    worst: Duration,
}

impl CloudMeter {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            ground: Duration::from_ticks(0),
            whole: Duration::from_ticks(0),
            send: Duration::from_ticks(0),
            worst: Duration::from_ticks(0),
        }
    }

    fn add(&mut self, ground: Duration, whole: Duration, send: Duration) {
        self.frames += 1;
        self.ground += ground;
        self.whole += whole;
        self.send += send;
        self.worst = self.worst.max(whole);
        let window = self.since.elapsed();
        if window >= CLOUD_REPORT {
            let frames = u64::from(self.frames);
            let tenths = frames * 10_000 / window.as_millis().max(1);
            let (ground, whole, send) = (
                self.ground.as_micros() / frames,
                self.whole.as_micros() / frames,
                self.send.as_micros() / frames,
            );
            info!(
                "Cloud: {} frames in {} ms, {}.{} per s, ground {} us, over it {} us, send {} us, worst {} us",
                frames,
                window.as_millis(),
                tenths / 10,
                tenths % 10,
                ground,
                whole.saturating_sub(ground + send),
                send,
                self.worst.as_micros()
            );
            *self = Self::new();
        }
    }
}

/// What the device shows once it is up: a player.
///
/// **The screen after boot is the player, not a list of what the device knows about itself.**
/// The cover, once the other chip has sent one, is the ground; title
/// and artist stand on a dimmed band below the middle, so the top of the cover stays whole; and
/// the volume runs round the rim as an arc in the theme's colours -- the ring's language, and the
/// one shape a round screen gives away for free. What this screen used to list moved into About.
///
/// The picture is drawn upright and turned on its way to the panel, so nothing in here has to
/// know which way the knob has left it standing.
fn status_screen(frame: &mut Framebuffer, state: &Overview, over_picture: bool) {
    let centre = Point::new(WIDTH as i32 / 2, HEIGHT as i32 / 2);
    let palette = state.theme.palette();

    // Helvetica like the settings. What the other chip names -- a title, an artist -- is set in
    // the Latin-1 cuts, because a phone's metadata is not ASCII.
    type Style = (&'static fonts::FontRenderer, Rgb565);
    let heading: Style = (&fonts::LARGE, Rgb565::WHITE);
    let quiet: Style = (&fonts::SMALL, Rgb565::CSS_GRAY);

    // The lines are laid out before any of them is drawn, because with a photograph behind
    // them the band they stand in has to be dimmed first -- and the height of that band is
    // whatever this list turns out to be. Each `y` is the middle of its line, which is where
    // [`menu_text`] centres it. The phone's two lines are not in it: they are measured against
    // their room and drawn by [`PlayerLine::draw`].
    let mut lines: Vec<(i32, &str, Style)> = Vec::new();
    let playing = !state.title.is_empty();
    if !playing {
        // Nothing to show but the device, so it names itself and its gestures: a stranger has
        // nothing else to go by.
        lines.push((centre.y - 24, "TeeToTum", heading));
        lines.push((centre.y + 8, "nothing playing", quiet));
        lines.push((centre.y + 40, "tap to play  swipe to skip", quiet));
        lines.push((centre.y + 58, "turn for volume", quiet));
    }

    // The band, from the top of the first line to the foot of the last. Full width on purpose:
    // the screen is round, so a band leaves its edges off the picture where a box would put two
    // more corners into it. The margins are half the largest line and a little.
    if over_picture {
        let (first, last) = match playing {
            true => (centre.y + TITLE_LINE.below, centre.y + ARTIST_LINE.below),
            false => (
                lines.first().map_or(0, |line| line.0),
                lines.last().map_or(0, |line| line.0),
            ),
        };
        let (top, foot) = (first - 16, last + 10);
        frame.dim_rows(top.max(0) as usize, foot.max(0) as usize);
        // And a band of its own for the hint home, which [`home_hint`] writes once this is done.
        frame.dim_rows(HINT.top as usize, HINT.bottom as usize);
    }

    // The volume, 0-127, like the scale of a knob: 270 degrees from lower left over the top to
    // lower right, with the gap at the bottom where the hint stands. Angles in embedded-graphics
    // start at three o'clock and run clockwise on the screen. The track is drawn even before the
    // other chip has named a volume, so the face does not change shape when it does.
    const START: f32 = 135.0;
    const SWEEP: f32 = 270.0;
    // Out to the rim of the screen, as the menu ring is: 8 px wide round a circle of 352, so its
    // outer edge is the screen's own. **Placed by its corner, not its centre**: `with_center`
    // puts the middle of an even diameter half a pixel below and right of the screen's, which
    // sits between pixels 179 and 180 -- off by half a pixel, a full-screen cover showed past
    // the arc at the top left.
    const CORNER: i32 = (WIDTH as i32 - 352) / 2;
    // A full cover stands inside the arc, so the two have to agree: the stroke is centred on
    // the circle, so its inside is the diameter less one width.
    const _: () = assert!(352 - 8 == settings::COVER_DISC as i32);
    let rim = |sweep: f32, colour: Rgb565| {
        Arc::new(Point::new(CORNER, CORNER), 352, START.deg(), sweep.deg())
            .into_styled(PrimitiveStyle::with_stroke(colour, 8))
    };
    let _ = rim(SWEEP, palette.ring).draw(frame);
    // Dimmed while nothing streams to the knob: the other chip then takes no volume step, and
    // the hand should see that turning does nothing rather than find out.
    if let Some(volume) = state.volume {
        let share = f32::from(volume.min(127)) / 127.0;
        let colour = match state.streaming {
            true => palette.selected,
            false => shade(palette.selected, -144),
        };
        if share > 0.0 {
            let _ = rim(SWEEP * share, colour).draw(frame);
        }
    }

    for (y, text, (font, colour)) in &lines {
        let _ = menu_text(frame, text, Point::new(centre.x, *y), font, *colour);
    }
    if playing {
        TITLE_LINE.draw(frame, &state.title, state.run);
        ARTIST_LINE.draw(frame, &state.artist, state.run);
    }
}

/// What stands under the name of the selected entry at home: **a state, not an explanation**.
/// The settings say where they stand, the player what plays, a plugin the summary from its
/// manifest -- or that it stopped,
/// which its face says too. Home says where this firmware comes from: the device is the only
/// place a stranger who picks it up can read that, and home is where they will look.
///
/// **"nothing playing", not "phone not connected"**: the other
/// chip answers us whether or not it has a phone, and bit 4 of its state, which looks like "A2DP
/// connected", is unread. An empty title is all that is known.
fn home_state(state: &Overview, entry: &Entry) -> Option<String> {
    match entry.kind {
        Kind::Firmware => {
            let mut line = format!("{} · {} %", state.theme.name(), state.brightness.percent());
            // Only what differs from how the device comes: an upright picture and clicks are
            // the ordinary case and would only make the line longer.
            if state.orientation != 0 {
                line += &format!(" · {} deg", state.orientation * 90);
            }
            if state.haptics.step() == 0 {
                line += " · silent";
            }
            if state.motion == Motion::Moving {
                line += " · moving";
            }
            Some(line)
        }
        Kind::Screen(id) => match Face::of(id) {
            Face::Player if state.title.is_empty() => Some(String::from("nothing playing")),
            Face::Player => Some(state.title.clone()),
            Face::Plugin(n) => {
                let view = state.plugins.get(usize::from(n))?.as_ref()?;
                match &view.fault {
                    Some(_) => Some(String::from("stopped")),
                    None => (!view.summary.is_empty()).then(|| String::from(view.summary)),
                }
            }
        },
        Kind::Home => Some(String::from(REPO)),
        Kind::Setting {
            id: SETTING_SHARE, ..
        } => Some(
            state
                .card
                .clone()
                .unwrap_or_else(|| String::from("no card")),
        ),
        _ => None,
    }
}

/// Where the source of this firmware lives. It stands under the home entry, so a device on a
/// desk carries its own provenance.
///
/// **Without the host name**, which does not fit: at 31 characters the full URL needs about
/// 289 px and the ring's chord leaves 268 on that line, so even the small face cut it. The
/// owner-and-repository form is what GitHub itself prints, and half a URL says less than a
/// whole short one.
const REPO: &str = "look at github.com:\nteetotum-rs/firmware";

/// The menus, home and the settings: the ring and its frame from `teetotum::menu`, and the two
/// things only the firmware knows -- what each setting currently stands at, and the body of
/// whichever dialog is open.
///
/// **The orientation needs no mark of its own any more.** Until the ring, a green dot at twelve
/// o'clock was what made 180 degrees distinguishable from 0 on a round screen. About is the top
/// segment of every menu and turns with the picture, so it is that mark now.
#[expect(
    clippy::large_stack_frames,
    reason = "many small `format!` values and draw calls, none over 36 bytes; the main stack has room, see the note at the top"
)]
fn settings_screen(
    frame: &mut Framebuffer,
    state: &Overview,
    nav: &Navigator,
    ring: Option<&Ring>,
    credentials: &share::Credentials,
    code: Option<&qr::Encoded>,
) {
    let orientation = format!("{} deg", state.orientation * 90);
    let brightness = format!("{} %", state.brightness.percent());
    let free = state.free_slots.map(|n| match n {
        0 => String::from("no free slot"),
        1 => String::from("1 free slot"),
        n => format!("{n} free slots"),
    });
    let haptics = match state.haptics.step() {
        0 => String::from("Off"),
        step => format!("{step} / {}", Haptics::MAX.step()),
    };
    let installed = |n: usize| {
        if state.plugins[n].as_ref().is_some_and(|view| view.installed) {
            "Yes"
        } else {
            "No"
        }
    };
    let paired = if state.bonded.is_some() {
        "paired"
    } else {
        "no phone"
    };
    // An id means something only together with its owner: a plugin's About is `Id(0)` too.
    let value = match (nav.owner(), nav.selected().and_then(Entry::id)) {
        (Owner::Firmware, Some(SETTING_ABOUT)) => Some(VERSION),
        (Owner::Firmware, Some(SETTING_ORIENTATION)) => Some(orientation.as_str()),
        (Owner::Firmware, Some(SETTING_THEME)) => Some(state.theme.name()),
        (Owner::Firmware, Some(SETTING_BRIGHTNESS)) => Some(brightness.as_str()),
        (Owner::Firmware, Some(SETTING_HAPTICS)) => Some(haptics.as_str()),
        (Owner::Firmware, Some(SETTING_COVER)) => Some(state.cover.name()),
        (Owner::Firmware, Some(SETTING_MOTION)) => Some(state.motion.name()),
        (Owner::Firmware, Some(SETTING_RECEIVE)) => free.as_deref(),
        (Owner::Firmware, Some(SETTING_SHARE)) => Some(state.card.as_deref().unwrap_or("no card")),
        (Owner::Firmware, Some(SETTING_FORGET)) => Some(paired),
        (Owner::Plugin, Some(id)) => match PluginSetting::of(id) {
            Some((n, PluginSetting::Installed)) => Some(installed(n)),
            _ => None,
        },
        _ => None,
    };
    // The cloud's four, which are numbers set in text.
    let shape = match (nav.owner(), nav.selected().and_then(Entry::id)) {
        (Owner::Firmware, Some(SETTING_POINTS)) => Some(format!("{}", state.shape.points)),
        (Owner::Firmware, Some(SETTING_BRIGHTEST)) => Some(format!("{} %", state.shape.brightest)),
        (Owner::Firmware, Some(SETTING_CENTRE)) => Some(format!("{} px", state.shape.centre)),
        (Owner::Firmware, Some(SETTING_ACCENT)) => Some(format!("{} %", state.shape.accent)),
        _ => None,
    };
    // Entries that lead deeper have no id; they are told apart by kind and name. The submenus are
    // `const`s, so their addresses say nothing.
    let deeper = match (nav.owner(), nav.selected()) {
        (Owner::Firmware, Some(entry)) => match entry.kind {
            Kind::Menu(_) if entry.name == PLAYER.title => Some(match state.cover {
                CoverStyle::Sharp => "sharp",
                CoverStyle::Full => "full screen",
            }),
            Kind::Menu(_) if entry.name == APP.title => Some(paired),
            Kind::Menu(_) if entry.name == BACKGROUND.title => Some(match state.motion {
                Motion::Still => "still",
                Motion::Moving => "moving",
            }),
            Kind::Plugin(_) => state
                .plugins
                .iter()
                .flatten()
                .find(|view| view.name == entry.name)
                .map(|view| match (&view.fault, view.loaded) {
                    (Some(_), _) => "stopped",
                    (None, Some(_)) => "loaded",
                    (None, None) => "not loaded",
                }),
            _ => None,
        },
        _ => None,
    };
    let home = if nav.menu().is_home() {
        nav.selected().and_then(|entry| home_state(state, entry))
    } else {
        None
    };
    let value = value.or(shape.as_deref()).or(deeper).or(home.as_deref());
    let palette = state.theme.palette();
    match ring {
        Some(ring) => nav.draw_on(frame, ring, palette, value),
        None => {
            let _ = nav.draw(frame, palette, value);
        }
    }

    let centre = BODY.center();
    // Green and large, like every other value on this device that is there to be read off the
    // screen rather than merely displayed.
    let (heading, reading, detail, quiet) = (
        (&fonts::BODY, palette.name),
        (&fonts::LARGE, palette.value),
        (&fonts::SMALL, Rgb565::CSS_LIGHT_GRAY),
        (&fonts::SMALL, palette.quiet),
    );
    let mut line = |dy: i32, text: &str, (font, colour)| {
        let _ = menu_text(frame, text, centre + Point::new(0, dy), font, colour);
    };

    match nav.opened() {
        // About is also where the device's own state went when the screen after boot became a
        // player: everything that screen used to list, in the one place a curious user -- or a
        // developer -- looks first. The version is the ring's value above, so it is not repeated.
        // What the other chip reports lives in the player's About.
        Some((SETTING_ABOUT, Owner::Firmware)) => {
            line(-38, "TeeToTum", (&fonts::BODY, palette.name));
            line(-18, "MIT OR Apache-2.0", quiet);
            line(
                0,
                &format!("up {} s  wi-fi {} nets", state.uptime, state.networks),
                detail,
            );
            line(
                16,
                &format!(
                    "ble {}  knob {:+}",
                    if state.peer {
                        "connected"
                    } else {
                        "advertising"
                    },
                    state.detents
                ),
                detail,
            );
        }
        // The address is the phone's identity, which is what its own Bluetooth settings show.
        Some((SETTING_APP_ABOUT, Owner::Firmware)) => {
            let bonded = match &state.bonded {
                Some(address) => format!("paired {}", AddressText(address)),
                None => String::from("no phone paired"),
            };
            let link = if state.peer {
                "connected"
            } else {
                "not connected"
            };
            line(-38, "App", (&fonts::BODY, palette.name));
            line(-18, "over Bluetooth LE", quiet);
            line(0, &bonded, detail);
            line(16, link, detail);
        }
        Some((SETTING_FORGET, Owner::Firmware)) => {
            let bonded = state.bonded.as_ref().map_or_else(
                || String::from("no phone"),
                |address| format!("{}", AddressText(address)),
            );
            line(-14, &bonded, (&fonts::BODY, palette.value));
            line(16, "OK forgets it", quiet);
            line(34, "the app pairs anew", quiet);
        }
        // The player's state is the other chip's: the cover it fetched, the volume it holds, and
        // the two bits it sets itself -- whether music streams here, and whether a phone is
        // connected over HID.
        Some((SETTING_PLAYER_ABOUT, Owner::Firmware)) => {
            let backdrop = match state.backdrop.as_ref() {
                Some(Backdrop::Card(name)) => format!("card  {name}"),
                // The picture's own size, whichever way it stands on the screen.
                Some(Backdrop::Cover { width, height }) => format!("cover  {width}x{height}"),
                None => String::from("no cover"),
            };
            let companion = match state.volume {
                Some(volume) => format!(
                    "vol {volume}  encoder {}",
                    if state.companion_encoder { "on" } else { "off" }
                ),
                None => String::from("other chip silent"),
            };
            let stream = if state.streaming {
                "audio streaming"
            } else {
                "no audio stream"
            };
            let hid = if state.hid {
                "hid connected"
            } else {
                "hid not connected"
            };
            line(-48, "Music Player", (&fonts::BODY, palette.name));
            line(-28, "through the other chip", quiet);
            line(-10, &backdrop, detail);
            line(8, &companion, detail);
            line(26, stream, detail);
            line(44, hid, detail);
        }
        Some((SETTING_BACKGROUND_ABOUT, Owner::Firmware)) => {
            let shape = state.shape;
            let ground = CLOUD_GROUND_US.load(Ordering::Relaxed);
            let motion = match state.motion {
                Motion::Still => "still",
                Motion::Moving => "moving",
            };
            line(-38, "Background", (&fonts::BODY, palette.name));
            line(-18, "a cloud of points", quiet);
            line(
                0,
                &format!("{} points, {} % bright", shape.points, shape.brightest),
                detail,
            );
            line(
                16,
                &format!("centre {} px, {} % icon colour", shape.centre, shape.accent),
                detail,
            );
            line(
                32,
                &format!(
                    "{motion}, ground {}.{} ms",
                    ground / 1000,
                    ground % 1000 / 100
                ),
                detail,
            );
        }
        Some((SETTING_MOTION, Owner::Firmware)) => {
            line(-14, state.motion.name(), reading);
            line(16, "turn the knob", quiet);
            line(34, "the cloud shows it", quiet);
        }
        Some((SETTING_POINTS, Owner::Firmware)) => {
            line(-14, &format!("{}", state.shape.points), reading);
            line(16, "turn the knob", quiet);
            line(34, "50 a detent", quiet);
        }
        Some((SETTING_BRIGHTEST, Owner::Firmware)) => {
            line(-14, &format!("{} %", state.shape.brightest), reading);
            line(16, "turn the knob", quiet);
            line(34, "at the rim", quiet);
        }
        Some((SETTING_CENTRE, Owner::Firmware)) => {
            line(-14, &format!("{} px", state.shape.centre), reading);
            line(16, "turn the knob", quiet);
            line(34, "no points inside it", quiet);
        }
        Some((SETTING_ACCENT, Owner::Firmware)) => {
            line(-14, &format!("{} %", state.shape.accent), reading);
            line(16, "turn the knob", quiet);
            line(34, "points in the icon colour", quiet);
        }
        Some((SETTING_COVER, Owner::Firmware)) => {
            line(-14, state.cover.name(), reading);
            line(16, "turn the knob", quiet);
            line(34, "sharp keeps 200 px", quiet);
        }
        Some((SETTING_ORIENTATION, Owner::Firmware)) => {
            line(-14, &orientation, reading);
            line(16, "turn the knob", quiet);
            line(34, "About marks the top", quiet);
        }
        Some((SETTING_THEME, Owner::Firmware)) => {
            line(-14, state.theme.name(), reading);
            line(16, "turn the knob", quiet);
            line(34, "the ring shows it", quiet);
        }
        Some((SETTING_BRIGHTNESS, Owner::Firmware)) => {
            line(-14, &brightness, reading);
            line(16, "turn the knob", quiet);
            line(34, "the screen shows it", quiet);
        }
        Some((SETTING_HAPTICS, Owner::Firmware)) => {
            line(-14, &haptics, reading);
            line(16, "turn the knob", quiet);
            line(34, "the clicks show it", quiet);
        }
        // A waiting plugin as its module describes it. An unknown key is said as such: it is
        // what tells a stranger's plugin from the project's.
        Some((SETTING_RECEIVE, Owner::Firmware)) => {
            let received = &state.received;
            line(-42, "a plugin over BLE", quiet);
            match received.status {
                UploadStatus::Idle if state.peer => line(-4, "connected", detail),
                UploadStatus::Idle => {
                    line(-4, "waiting for a sender", detail);
                    line(16, &format!("visible as {BLE_DEVICE_NAME}"), quiet);
                }
                UploadStatus::Ready => {
                    let percent = received.received * 100 / received.total.max(1);
                    line(-14, &format!("{percent} %"), reading);
                    line(
                        16,
                        &format!("{} of {} bytes", received.received, received.total),
                        detail,
                    );
                    line(34, &format!("into slot {}", received.slot), quiet);
                }
                UploadStatus::Written => {
                    line(-14, "written", reading);
                    line(16, "restarting", quiet);
                }
                UploadStatus::Deleted => {
                    line(-14, "deleted", reading);
                    line(16, "restarting", quiet);
                }
                failed => {
                    line(-14, failed.reason(), (&fonts::SMALL, Rgb565::CSS_ORANGE));
                    let hint = match failed {
                        UploadStatus::NoPlugin => "nothing deleted",
                        _ => "send it again",
                    };
                    line(16, hint, quiet);
                }
            }
        }
        Some((SETTING_INSTALL, Owner::Firmware)) => {
            if let Some(offer) = state.offer.as_ref() {
                let kb = |bytes: usize| format!("{}.{}", bytes / 1000, bytes % 1000 / 100);
                let key: String = offer.key.iter().map(|b| format!("{b:02x}")).collect();
                let (owner, owner_style) = if offer.known {
                    ("project key", quiet)
                } else {
                    ("unknown key", (&fonts::SMALL, Rgb565::CSS_ORANGE))
                };
                let cost = if offer.heap <= offer.free {
                    format!("v{}  {} KB heap", offer.version, kb(offer.heap))
                } else {
                    format!("too large: {} KB heap", kb(offer.heap))
                };
                line(-38, offer.name, heading);
                if offer.update {
                    line(-17, &format!("update, {owner}"), owner_style);
                } else {
                    line(-17, owner, owner_style);
                }
                line(1, &format!("key {key}"), detail);
                line(19, &format!("rights {}", offer.rights), detail);
                line(37, &cost, detail);
            }
        }
        // What the manifest says, and what loading cost -- read without running the plugin,
        // which is what the manifest is for.
        Some((id, Owner::Plugin)) => {
            if let Some((n, PluginSetting::Installed)) = PluginSetting::of(id) {
                line(-14, installed(n), reading);
                line(16, "turn the knob", quiet);
                line(34, "OK restarts", quiet);
            } else if let Some((n, PluginSetting::About)) = PluginSetting::of(id)
                && let Some(view) = state.plugins[n].as_ref()
            {
                // One fact a line: a shared line did not fit a load time of three digits.
                // Small lines are 18 px tall, so six of them under a BODY heading end on the lower
                // edge of the body, clear of the buttons.
                let (run, cost) = match (&view.fault, view.loaded) {
                    (Some(fault), _) => (String::from("stopped"), fault.clone()),
                    (None, Some((us, heap))) => (
                        format!("loaded in {}.{} ms", us / 1000, us % 1000 / 100),
                        format!("{}.{} KB heap", heap / 1000, heap % 1000 / 100),
                    ),
                    (None, None) => (String::from("not loaded"), String::new()),
                };
                line(-48, view.name, heading);
                match view.slot {
                    Some(slot) => line(-28, &format!("from slot {slot}"), quiet),
                    None => line(-28, "bundled with the firmware", quiet),
                }
                line(-10, &format!("{} bytes", view.bytes), detail);
                line(8, &format!("rights {}", view.rights), detail);
                line(26, &run, detail);
                line(44, &cost, detail);
            }
        }
        // The network to join, as a code or, once the knob turns, in words.
        Some((SETTING_SHARE, Owner::Firmware)) => match (&state.card, code) {
            (None, _) => {
                line(-14, "no card", reading);
                line(16, "none was found at boot", quiet);
            }
            (Some(_), Some(code)) if !state.share_text => {
                code.draw(frame, share::HOST, &credentials.ssid);
            }
            // Typed in by hand on another device, so the three values are green and as large as
            // the longest of them allows.
            (Some(_), _) => {
                let value = (&fonts::VALUE, palette.value);
                line(-43, "join the network", quiet);
                line(-19, &credentials.ssid, value);
                line(11, "with the password", quiet);
                line(35, &credentials.password, value);
                line(65, "then open", quiet);
                line(89, share::HOST, value);
            }
        },
        Some((id, Owner::Firmware)) => {
            if let Some(n) = qr_index(id) {
                qr::draw(frame, n);
            }
        }
        _ => {}
    }
}
