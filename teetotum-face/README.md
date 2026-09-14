# teetotum-face

Write a face -- a plugin -- for [teetotum](https://github.com/teetotum-rs/firmware), the
firmware for the Waveshare ESP32-S3 Knob Touch LCD 1.8.

A face is a WebAssembly module. It gets the taps and wipes on the glass, the knob if it asks for
it, and names what should be drawn; the firmware does the drawing. The crate has no dependencies
and needs no allocator.

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
teetotum-face = "0.1"
```

A face builds for `wasm32v1-none`, is signed with
[`teetotum-pack`](https://crates.io/crates/teetotum-pack) and is accepted on the glass before any
of its code runs. The whole way from an empty crate to a face on the device is in the
[plugin guide](https://github.com/teetotum-rs/firmware/blob/main/docs/plugin-development.md).

## Versions

The crate version follows the host ABI. A face records `abi::VERSION` in its manifest, and a
firmware refuses a face built against a newer ABI than its own. **Every raise of the ABI is a
breaking release of this crate** -- a new minor version while it is `0.x` -- so a face that
depends on `0.1` never picks up a newer ABI through `cargo update`.

| `teetotum-face` | host ABI |
|---|---|
| 0.1 | 1 |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
