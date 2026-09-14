# Changelog

Changes that users of the firmware or authors of plugins can notice. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). `teetotum-face` and `teetotum-pack`
follow [Semantic Versioning](https://semver.org/); a raise of the host ABI is a breaking release
of `teetotum-face`.

## [Unreleased]

### Added

- [teetotum-rs/plugins](https://github.com/teetotum-rs/plugins): a template for `cargo generate`, an
  example face and `index.json`, the list of known faces with where to fetch them, checked by CI.

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

[Unreleased]: https://github.com/teetotum-rs/firmware/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/teetotum-rs/firmware/releases/tag/v0.1.0
