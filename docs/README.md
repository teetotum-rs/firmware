# TeeToTum documentation

Five documents, one for each kind of reader. All of them are plain Markdown with no images to
fetch and no links you need to follow to understand them, so they read the same in a text
editor, offline, as they do on a code host.

| Document | For | What it covers |
|---|---|---|
| [User guide](user-guide.md) | anyone with a Knob running TeeToTum | the controls, Home, the Music Player, Card over Wi-Fi, every setting, troubleshooting, building and flashing |
| [Using plugins](plugins.md) | anyone who starts, removes or wonders about plugins | the bundled plugins, plugin settings, rights, what "stopped" means, memory, installing more |
| [Writing plugins](plugin-development.md) | Rust developers | the `teetotum-face` SDK, the manifest, events, drawing, host calls, limits, building and bundling |
| [Code quality](code-quality.md) | anyone who changes the firmware or writes a plugin | the checks a push must pass: rustfmt, Clippy with every warning an error, the stack-frame lint, exceptions with a reason, the pre-push hook |
| [The hardware, as measured](hardware/README.md) | anyone curious about the board | an index plus eight topic files: pins, display, input, haptics, audio, the companion link, the card, the radios |

The hardware documentation is what the board answered when it was asked — which chip does what,
which pin goes where, and what was measured on the device rather than taken from the datasheet.
