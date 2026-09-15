# Changelog

Changes that users of the firmware or authors of plugins can notice. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). `teetotum-face` and `teetotum-pack`
follow [Semantic Versioning](https://semver.org/); a raise of the host ABI is a breaking release
of `teetotum-face`.

## [Unreleased]

### Added

- Card over Wi-Fi, at ten o'clock on Home and eleven in the Settings: while its dialog is open, the
  Knob runs an access point, shows a QR code to join it and serves the TF card read-only to a
  browser. The Music Player moves to nine o'clock on Home and ten in the Settings, and the first
  page of each ring holds one plugin fewer.
- `teetotum-pack`: `slot::Digest` and `Header::matches_digest`, for a module that arrives in pieces.
- Plugin page: each plugin shows its tags from the catalogue, and buttons filter the list by tag
  ([#2](https://github.com/teetotum-rs/firmware/issues/2)).

### Changed

- The centre of a ring leaves more room: the hint to tap sits lower, and a two-line state on Home
  keeps clear of the entry name above it.

### Fixed

- A long entry name on Home or a long dialog title steps down to a smaller face instead of
  running into the ring.

## [0.2.1] - 2026-09-15

A firmware release; `teetotum-face` and `teetotum-pack` stay at 0.1.0.

### Fixed

- A plugin removed with Installed: No comes back on Home when the same plugin is installed again
  through the install dialog ([#1](https://github.com/teetotum-rs/firmware/issues/1)).

## [0.2.0] - 2026-09-15

A firmware release; `teetotum-face` and `teetotum-pack` stay at 0.1.0.

### Added

- [teetotum-rs/plugins](https://github.com/teetotum-rs/plugins): a template for `cargo generate`, an
  example face and `index.json`, the list of known faces with where to fetch them, checked by CI.
- Firmware files attached to each GitHub release — bootloader, partition table, blank `otadata`,
  app, checksums — and a [web installer](https://teetotum-rs.github.io/firmware/) that writes them
  from the browser without touching settings or plugins.
- Plugins uploaded over BLE while Settings > Receive is open: the knob writes the module
  into a free flash slot, restarts and offers it in the install dialog, which marks an update of an
  installed plugin. A [plugin page](https://teetotum-rs.github.io/firmware/plugins.html) sends one
  from the browser, from the catalogue of teetotum-rs/plugins or a file of your own;
  `tools/ble-upload.py` sends one from a computer.
- Settings > About shows the commit a firmware was built from after its version, with `+` for a
  build from changed files.
- Every entry in the settings ring shows where it stands: Background whether the cloud moves,
  Music Player the cover style, each plugin whether it is loaded, and Receive how many slots are
  free.

## [0.1.0] - 2026-09-14

The first release.

### Added

- Firmware for the Waveshare ESP32-S3 Knob Touch LCD 1.8: Home as a ring of twelve segments, a
  ring of QR codes, settings that survive a power cut, and a music player for the phone paired
  with the board's second chip.
- Drivers in the `teetotum` crate: ST77916 panel over QSPI, CST816D touch, the knob on a GPIO
  interrupt, DRV2605L haptics, a read-only FAT reader for the TF card, and the UART link to the
  second chip.
- Plugins ("faces") as signed WebAssembly modules under wasmi, with a manifest of rights the
  firmware enforces, host ABI 1. Bundled: HID remote, Teetotum, Nearby.
- Sixteen flash slots for plugins written over USB and accepted on the glass.
- `teetotum-face` 0.1.0, the crate a plugin is written against.
- `teetotum-pack` 0.1.0, which checks, signs and packs plugins, as a library and a command-line
  tool for Linux, macOS and Windows.

[Unreleased]: https://github.com/teetotum-rs/firmware/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/teetotum-rs/firmware/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/teetotum-rs/firmware/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/teetotum-rs/firmware/releases/tag/v0.1.0
