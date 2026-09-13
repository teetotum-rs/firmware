#!/bin/sh
# Builds the face into firmware/assets/plugins/hid-remote.wasm, which the firmware embeds.
#
# `.cargo/config.toml` next to this sets the target and the four linker flags a face needs, so
# this is a plain `cargo build` and a copy. Inside this repository it runs on the firmware's
# toolchain (`channel = "esp"`), whose `[unstable] build-std` builds `core` for the target as
# well; a face outside it needs only a stock `rustup target add wasm32v1-none`.
#
# The three modules `firmware/src/bin/wasm` measures -- hid-own, hid-import, hid-default -- are
# not made here any more. They are frozen inputs, built from an earlier version of this plugin,
# and stay as they are so that the run keeps measuring what it did.
set -eu
cd "$(dirname "$0")"
out=../../firmware/assets/plugins
mkdir -p "$out"
cargo build --release -q
cp target/wasm32v1-none/release/hid_remote.wasm "$out/hid-remote.wasm"
echo "$out/hid-remote.wasm: $(wc -c < "$out/hid-remote.wasm") bytes"
