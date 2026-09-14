//! Checks faces on the host with the code the firmware checks them with.
//!
//! ```sh
//! teetotum-pack check <wasm>...   # manifest and signature, as the firmware reads them before loading
//! teetotum-pack id <wasm>         # the eight bytes the settings record knows the face by
//! ```
//!
//! Exits 0 when every module passes, 1 when one does not, 2 on a usage error.
//! From this repository, `tools/teetotum-pack` runs it under stable Rust.

use std::{env, fmt::Write, fs, path::Path, process::ExitCode};

use teetotum_pack::{Error, Manifest, PluginId, Signed, verify};

const USAGE: &str = "usage: teetotum-pack check <wasm>...\n       teetotum-pack id <wasm>";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.split_first() {
        Some((command, modules)) if command == "check" && !modules.is_empty() => {
            let mut passed = true;
            for module in modules {
                passed &= check(Path::new(module));
            }
            if passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some((command, [module])) if command == "id" => id(Path::new(module)),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn check(path: &Path) -> bool {
    let result = fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|wasm| describe(&wasm).map_err(|e| e.to_string()));
    let (line, passed) = match result {
        Ok(line) => (line, true),
        Err(e) => (e, false),
    };
    println!("{}: {line}", path.display());
    passed
}

/// The firmware's order: the manifest first, so a face built for a newer ABI is refused as that.
fn describe(wasm: &[u8]) -> Result<String, Error> {
    let manifest = Manifest::read(wasm)?;
    verify(wasm)?;
    let key = Signed::read(wasm)?.key;
    Ok(format!(
        "{} {}, id {}, signed by {}, holds",
        manifest.name(),
        manifest.version(),
        hex(&PluginId::new(key, manifest.name()).bytes()),
        hex(&key[..PluginId::LEN]),
    ))
}

fn id(path: &Path) -> ExitCode {
    let result = fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|wasm| PluginId::of(&wasm).map_err(|e| e.to_string()));
    match result {
        Ok(id) => {
            println!("{}", hex(&id.bytes()));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
