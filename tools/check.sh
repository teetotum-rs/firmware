#!/bin/sh
# Runs every check a push must pass, stopping at the first failure:
# rustfmt and Clippy over the firmware workspace and each plugin, the release build, the
# signatures of the bundled plugins and the third-party notice.
#
#     tools/check.sh
#
# The Xtensa toolchain is found on PATH; failing that, espup's `$HOME/export-esp.sh` is sourced.
set -eu
cd "$(dirname "$0")/.."

if ! command -v xtensa-esp32s3-elf-gcc >/dev/null 2>&1 && [ -f "$HOME/export-esp.sh" ]; then
    . "$HOME/export-esp.sh"
fi

# Faces the firmware embeds; hid-own, hid-import and hid-default are unsigned measurement inputs.
BUNDLED="hid-remote nearby teetotum-plugin"

# The dummy face takes its name at compile time (plugins/dummy/build.sh); any name lints the same.
export TEETOTUM_DUMMY_NAME="${TEETOTUM_DUMMY_NAME:-Dummy 1}"

step() { printf '\n==> %s\n' "$*"; }

step "rustfmt: workspace"
cargo fmt --all --check

for p in plugins/*/; do
    step "rustfmt: $p"
    (cd "$p" && cargo fmt --check)
done

step "clippy: workspace"
cargo clippy --release --workspace -- -D warnings

for p in plugins/*/; do
    step "clippy: $p"
    (cd "$p" && cargo clippy --release -- -D warnings)
done

step "build: firmware"
cargo build --release

step "signatures: bundled plugins"
set --
for m in $BUNDLED; do set -- "$@" "firmware/assets/plugins/$m.wasm"; done
tools/sign-face.py --check "$@"

step "third-party notice"
tools/third-party.py --check

step "all checks passed"
