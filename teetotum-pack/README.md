# teetotum-pack

Sign, check and install faces -- plugins -- for
[teetotum](https://github.com/teetotum-rs/firmware), the firmware for the Waveshare ESP32-S3 Knob
Touch LCD 1.8.

The library checks a face before any of its code runs: whether its signature holds, and who it
is. It is `no_std` and allocates nothing, so the firmware runs the same checks as a tool on the
host. Signing needs the `std` feature.

## The tool

```sh
cargo install teetotum-pack --features cli

teetotum-pack sign my_face.wasm              # sign, creating a key on first use
teetotum-pack check my_face.wasm             # does the signature hold?
teetotum-pack id my_face.wasm                # the id the device keeps
teetotum-pack pack my_face.wasm --slot 0 --write
```

The key is `--key`, else `$TEETOTUM_KEY`, else `~/.config/teetotum/face-key.pem`. **Back it up:**
key and name are a face's identity, and an update signed with another key is another face.
`pack --write` needs [`espflash`](https://crates.io/crates/espflash) and the board on USB.

Runs on Linux, macOS and Windows. On Unix a new key is readable by its owner only.

The [plugin guide](https://github.com/teetotum-rs/firmware/blob/main/docs/plugin-development.md)
covers building, signing and installing a face.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
