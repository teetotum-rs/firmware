#!/bin/sh
# Builds one placeholder face per name into firmware/assets/plugins/, which the firmware embeds.
#
# The name reaches the sources through the environment, so the same crate becomes as many
# modules as there are names here. `build.rs` makes cargo notice the change; `cargo build`
# alone would hand back the module it built last.
#
# Everything `plugins/hid-remote/build.sh` says about the toolchain holds here too.
set -eu
cd "$(dirname "$0")"
out=../../firmware/assets/plugins
mkdir -p "$out"
for n in 1 2 3 4 5 6 7 8 9; do
  TEETOTUM_DUMMY_NAME="Dummy $n" cargo build --release -q
  cp target/wasm32v1-none/release/dummy.wasm "$out/dummy-$n.wasm"
  ../../tools/teetotum-pack sign "$out/dummy-$n.wasm"
done
