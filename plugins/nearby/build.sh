#!/bin/sh
# Builds the face into firmware/assets/plugins/nearby.wasm, which the firmware embeds.
# Everything `plugins/hid-remote/build.sh` says about the toolchain holds here too.
set -eu
cd "$(dirname "$0")"
out=../../firmware/assets/plugins
mkdir -p "$out"
cargo build --release -q
cp target/wasm32v1-none/release/nearby.wasm "$out/nearby.wasm"
echo "$out/nearby.wasm: $(wc -c < "$out/nearby.wasm") bytes"
