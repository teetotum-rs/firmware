# Code quality

What TeeToTum asks of its code, and the script that checks it. For anyone who forks the
firmware, changes it, or writes a plugin that should meet the same bar as the bundled ones.

The bar is mechanical: **what `tools/check.sh` passes is good enough**, and nothing below asks
for more than that script enforces. Everything else — names, comments, how a module is cut —
follows the code around the change.

## Contents

1. [The bar](#1-the-bar)
2. [Running the checks](#2-running-the-checks)
3. [The pre-push hook](#3-the-pre-push-hook)
4. [rustfmt](#4-rustfmt)
5. [Clippy](#5-clippy)
   - [Which lints](#which-lints)
   - [Stack frames](#stack-frames)
   - [Exceptions with a reason](#exceptions-with-a-reason)
   - [Where a macro is in the way](#where-a-macro-is-in-the-way)
   - [Fixing what Clippy finds](#fixing-what-clippy-finds)
6. [Plugins](#6-plugins)
7. [Signatures and the third-party list](#7-signatures-and-the-third-party-list)
8. [When a check fails](#8-when-a-check-fails)

## 1. The bar

One bar for everything in the repository: the firmware, the SDK crates `teetotum` and
`teetotum-face`, and the plugins under `plugins/`.

| Check | Passes when |
|---|---|
| rustfmt | `cargo fmt --check` would change nothing, with rustfmt's default settings |
| Clippy | there is no warning at all: `-D warnings` turns every one into an error |
| Build | the firmware builds with `cargo build --release` |
| Signatures | every bundled plugin in `firmware/assets/plugins/` carries a signature that holds |
| Third-party list | `THIRD-PARTY.md` names exactly the crates compiled into the firmware |

There is no hosted CI. `tools/check.sh` is the whole gate, and the pre-push hook runs it.

## 2. Running the checks

```sh
tools/check.sh
```

It needs what building the firmware needs: the `esp` toolchain from `espup` (the channel is set
in `rust-toolchain.toml`, so Clippy and rustfmt come from the same toolchain as the compiler).
If `xtensa-esp32s3-elf-gcc` is not on `PATH`, the script sources `$HOME/export-esp.sh`, the file
`espup` writes by default. It also needs `python3` and `openssl` 3.0 or later for the last two
steps.

The steps run in this order, each announced by a `==>` line, and the first failure stops the
script with a non-zero exit code:

| # | Step | Command |
|---|---|---|
| 1 | format, workspace | `cargo fmt --all --check` |
| 2 | format, each plugin | `cargo fmt --check` in every `plugins/*/` |
| 3 | lint, workspace | `cargo clippy --release --workspace -- -D warnings` |
| 4 | lint, each plugin | `cargo clippy --release -- -D warnings` in every `plugins/*/` |
| 5 | build | `cargo build --release` |
| 6 | signatures | `tools/sign-face.py --check` over the bundled plugins |
| 7 | third-party list | `tools/third-party.py --check` |

With a warm build cache the whole run takes a little over a minute. From an empty `target/` the
full firmware build comes on top.

## 3. The pre-push hook

```sh
git config core.hooksPath tools/hooks
```

From then on every `git push` runs `tools/hooks/pre-push`. It first looks through the commits
about to be pushed for material that should not be published, then runs `tools/check.sh`. If
either fails, nothing is pushed.

The scan covers only the outgoing commits — for a new branch, every commit no remote has yet:

- **File names** anywhere in a path: `CLAUDE.md`, `transcripts/`, `scratch-findings/`,
  `.obsidian/`. These are working notes, not source.
- **Added lines** that contain a path into a home directory under `/home`.
- **Added lines** that match a pattern from
  `${TEETOTUM_PRIVATE_PATTERNS:-$HOME/.config/teetotum/private-patterns}`: one extended regular
  expression per line, matched without regard to case. A line that starts with `!` is a fixed
  string that is allowed even where a pattern matches it.

The pattern file never lives in the repository — it would publish exactly what it looks for.
Put in it what you do not want in public history: your name, an employer, the names of your
machines. **Without the file the hook refuses to push**, rather than scanning for nothing. An
empty file is valid and leaves only the generic checks above.

`LICENSE*` and `CITATION.cff` are not scanned, and neither are lines starting with `authors`:
they carry the author's name on purpose.

The hook takes as long as `tools/check.sh`. `git push --no-verify` skips it, the checks
included.

## 4. rustfmt

Default settings; the repository has no `rustfmt.toml`.

```sh
cargo fmt --all                  # the workspace
(cd plugins/nearby && cargo fmt) # a plugin, which is a workspace of its own
```

`cargo fmt --all` does not reach the plugins, because the root `Cargo.toml` excludes `plugins/`.

Where rustfmt would take apart an alignment that carries meaning — a table of pins or
registers — `#[rustfmt::skip]` on that one item keeps it. The repository does not use it at
the moment: every aligned table survived formatting.

## 5. Clippy

### Which lints

**Clippy's default set, with every warning an error.** That is the groups `correctness` (deny
by default), `suspicious`, `style`, `complexity` and `perf`. The groups `pedantic`, `nursery`,
`restriction` and `cargo` are not enabled, and no `Cargo.toml` has a `[lints]` table.

Two lints outside the default set are switched on, both at the top of the firmware binary,
`firmware/src/bin/main.rs`, and nowhere else:

| Lint | Group | Level | Why |
|---|---|---|---|
| `clippy::mem_forget` | restriction | deny | `mem::forget` is generally not safe with esp-hal types, especially those holding buffers for the duration of a data transfer |
| `clippy::large_stack_frames` | nursery | deny | see [Stack frames](#stack-frames) |

The toolchain is pinned by the `esp` channel, and with it the Clippy version. A newer Clippy
brings new lints and sharper old ones; what it finds is fixed like anything else.

### Stack frames

`large_stack_frames` estimates the stack frame of each function and fires above the threshold
in `.clippy.toml`:

```toml
stack-size-threshold = 1024
```

The threshold is low because the stack is small and shared. The main stack on the first core is
88 KiB (90 324 bytes between the linker symbols `_stack_end_cpu0` and `_stack_start_cpu0`),
and the firmware's `esp_rtos::main` runs on it. Measured on the device — the stack filled
with a pattern at boot, then scanned for the deepest overwritten byte — it peaks at about
60 KiB, while a cover image decodes. A frame of a few kilobytes is
therefore a real share of what is left, and each one is worth a look.

When the lint fires:

1. **Read Clippy's note.** It lists the largest locals of the frame.
2. **If a large value only moves**, borrow it, take it out of an `Option` in place, or return it
   through `&mut` instead of by value.
3. **Boxing is not the default fix.** It moves the value to the internal heap, and on this
   board the internal heap runs out before the stack does (about 57 KB free after boot).
4. **If the frame is what the design needs**, write an exception with the size in its reason
   (next section).

The lint is only on in `firmware/src/bin/main.rs`. In the library crates, the other binaries
under `firmware/src/bin/` and the plugins it is off, like every `nursery` lint.

### Exceptions with a reason

A lint that is wrong in one place is silenced in that place, with `#[expect]` and a `reason`:

```rust
#[expect(
    clippy::large_stack_frames,
    reason = "a `Plugin` is 968 bytes and moves by value, and boxing it would spend internal heap, which runs out first; the main stack has room, see the note at the top"
)]
```

- **`expect`, not `allow`.** When the code changes and the lint no longer fires, `expect` warns
  with `this lint expectation is unfulfilled` — and under `-D warnings` that is an error. The
  exception cannot outlive its cause.
- **The reason says why the lint is wrong here**, not what the lint is. Where the lint is about
  a size, the reason gives the size.
- **As narrow as it goes**: on the function, not the module; on the module, not the crate.

Nothing enforces `expect` over `allow` — Clippy's `allow_attributes` is a `restriction` lint
and off — and a few older `#[allow]`s without a reason remain. New exceptions are written as
above.

### Where a macro is in the way

Three cases where the attribute cannot go where it belongs:

| Macro | What happens | What to write |
|---|---|---|
| `#[esp_rtos::main]` | rejects `expect` on the entry point: "not allowed on a xtensa-lx-rt entry point" | `#[allow(..., reason = "...")]` above the macro |
| `#[embassy_executor::task]` | copies the attribute onto the two functions it generates; the lint fires in one, so the `expect` on the other is unfulfilled | `#[allow(..., reason = "...")]` above the macro |
| `derive` and item-generating macros such as trouble-host's `gatt_server` and `gatt_service` | the lint fires in the generated `impl`s, which an `expect` on the type does not reach | put the type in a small module and the `expect` on the module |

The last pattern stands twice in `firmware/src/bin/main.rs`, as `mod overview` and `mod gatt`:

```rust
#[expect(
    clippy::needless_borrows_for_generic_args,
    clippy::large_stack_frames,
    reason = "both fire in the code `gatt_server` and `gatt_service` generate"
)]
mod gatt {
    // the types, with their derives and macros
}
```

### Fixing what Clippy finds

```sh
cargo clippy --release --workspace --fix --allow-dirty
```

applies the suggestions Clippy marks as machine-applicable. Read the diff before keeping it:
some rewrites differ at the edges. `manual_clamp` turns `x.max(lo).min(hi)` into
`x.clamp(lo, hi)`, which panics when `lo > hi`. Run `cargo fmt --all` afterwards: a fix can
leave code that rustfmt would lay out differently.

## 6. Plugins

Each plugin under `plugins/` is a workspace of its own and builds for `wasm32v1-none` (the
target comes from its `.cargo/config.toml`). Check one from its directory:

```sh
cd plugins/my-face
cargo fmt --check
cargo clippy --release -- -D warnings
```

`tools/check.sh` finds every directory under `plugins/` by itself, so a new plugin there is
checked without touching the script. `plugins/dummy` takes its name at compile time from
`TEETOTUM_DUMMY_NAME`; the script sets `Dummy 1` if the variable is unset.

A plugin outside this repository meets the same bar with the same two commands. How to build,
sign and bundle one is in [Writing plugins](plugin-development.md).

## 7. Signatures and the third-party list

**Signatures.** `tools/sign-face.py --check` verifies the last section, `teetotum.signature`, of
each bundled plugin: `hid-remote`, `nearby` and `teetotum-plugin`, listed as `BUNDLED` in
`tools/check.sh`. Ed25519 signatures are deterministic, so rebuilding an unchanged plugin gives
the same file; a changed one needs signing again, which its `build.sh` does. A plugin that
becomes bundled goes into `BUNDLED` as well. The other modules in `firmware/assets/plugins/` are
inputs for measurement runs and unsigned on purpose. What the signature means for a plugin's
identity is in [Writing plugins](plugin-development.md#the-signature).

**Third-party list.** `THIRD-PARTY.md` lists every crate compiled into the firmware with its
licence, generated from `cargo metadata` for the device target. A change to the dependencies
changes it: run `tools/third-party.py` and commit the result with the change.

## 8. When a check fails

| Output | What to do |
|---|---|
| `Diff in <file>` from rustfmt | `cargo fmt --all`, or `cargo fmt` in the plugin's directory |
| a Clippy error with `-D warnings` in its note | fix it; [Fixing what Clippy finds](#fixing-what-clippy-finds) |
| `this lint expectation is unfulfilled` | the exception is no longer needed: delete it |
| `this function may allocate N bytes on the stack` | [Stack frames](#stack-frames) |
| `<file>: not signed` or `does NOT hold` | `./plugins/<name>/build.sh` rebuilds and signs the plugin |
| `THIRD-PARTY.md is out of date; run tools/third-party.py` | run it and commit `THIRD-PARTY.md` |
| `pre-push: ... is missing; refusing rather than scanning for nothing` | create the pattern file ([The pre-push hook](#3-the-pre-push-hook)) |
| `pre-push: private files in outgoing commits` or `private text in outgoing commits` | take the file or line out of those commits before pushing — a later commit that deletes it leaves it in history |
