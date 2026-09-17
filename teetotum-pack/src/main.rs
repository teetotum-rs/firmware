//! Checks faces on the host with the code the firmware checks them with.
//!
//! ```sh
//! teetotum-pack check <wasm>...              # manifest and signature, as the firmware reads them
//! teetotum-pack sign [--key <pem>] <wasm>...  # sign in place, replacing any signature
//! teetotum-pack id <wasm>                     # the eight bytes the settings record knows the face by
//! teetotum-pack pack <wasm> --slot <n> [--write] [--partitions <csv>]
//! teetotum-pack firmware [--key <pem>] <image> [<signed>]  # sign an image for an update over BLE
//! ```
//!
//! The key is `--key`, else `$TEETOTUM_KEY`, else `~/.config/teetotum/face-key.pem`, an Ed25519
//! private key in PEM, as `openssl genpkey -algorithm ed25519` writes it. It is created on first
//! use. Keep it: key and name are a face's identity, and an update signed with another key is
//! another face.
//!
//! `firmware` signs with a key of its own: `--key`, else `$TEETOTUM_FIRMWARE_KEY`, else
//! `~/.config/teetotum/firmware-key.pem`. It writes `<image>.tfw` unless told where, and prints
//! the public key the firmware has to hold.
//!
//! `pack` writes `<wasm>.slot` next to the module, the slot header and then the module, and with
//! `--write` hands it to `espflash write-bin` at the slot's address in the partition table:
//! `--partitions`, else `$TEETOTUM_PARTITIONS`, else `partitions.csv` here. The firmware asks on
//! the screen at the next boot whether the face gets a place; one with a bundled face's id takes
//! that face's place.
//!
//! Exits 0 when every module passes, 1 when one does not, 2 on a usage error.
//! From this repository, `tools/teetotum-pack` runs it under stable Rust.

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::{
    env,
    fmt::Write as _,
    fs::{self, DirBuilder, OpenOptions},
    io::{ErrorKind, Write as _},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use ed25519_compact::KeyPair;
use teetotum_pack::{Error, Manifest, PluginId, Signed, slot, verify};

const USAGE: &str = "usage: teetotum-pack check <wasm>...
       teetotum-pack sign [--key <pem>] <wasm>...
       teetotum-pack id <wasm>
       teetotum-pack pack <wasm> --slot <n> [--write] [--partitions <csv>]
       teetotum-pack firmware [--key <pem>] <image> [<signed>]";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.split_first() {
        Some((command, modules)) if command == "check" && !modules.is_empty() => {
            let mut passed = true;
            for module in modules {
                passed &= check(Path::new(module));
            }
            exit(passed)
        }
        Some((command, rest)) if command == "sign" => {
            let (key, modules) = match rest {
                [flag, key, modules @ ..] if flag == "--key" => (Some(key.as_str()), modules),
                _ => (None, rest),
            };
            if modules.is_empty() || modules.iter().any(|m| m.starts_with('-')) {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            sign(key, modules)
        }
        Some((command, rest)) if command == "firmware" => {
            let (key, files) = match rest {
                [flag, key, files @ ..] if flag == "--key" => (Some(key.as_str()), files),
                _ => (None, rest),
            };
            match files {
                [image] => firmware(key, Path::new(image), None),
                [image, signed] => firmware(key, Path::new(image), Some(Path::new(signed))),
                _ => {
                    eprintln!("{USAGE}");
                    ExitCode::from(2)
                }
            }
        }
        Some((command, [module])) if command == "id" => id(Path::new(module)),
        Some((command, rest)) if command == "pack" => match Pack::parse(rest) {
            Some(pack) => pack.run(),
            None => {
                eprintln!("{USAGE}");
                ExitCode::from(2)
            }
        },
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

fn sign(key: Option<&str>, modules: &[String]) -> ExitCode {
    let key = match key_path(key, FACE_KEY).and_then(|path| load_key(&path, &FACE_KEY)) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("teetotum-pack: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut passed = true;
    for module in modules {
        let path = Path::new(module);
        let result = fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|wasm| teetotum_pack::sign(&wasm, &key).map_err(|e| e.to_string()))
            .and_then(|signed| {
                fs::write(path, &signed)
                    .map(|()| signed.len())
                    .map_err(|e| e.to_string())
            });
        match result {
            Ok(len) => println!(
                "{}: {len} bytes, signed by {}",
                path.display(),
                hex(&key.pk[..PluginId::LEN])
            ),
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                passed = false;
            }
        }
    }
    exit(passed)
}

/// Where a kind of key is looked for: its variable, and its file under `~/.config/teetotum`.
struct KeyKind {
    var: &'static str,
    file: &'static str,
    identity: &'static str,
}

const FACE_KEY: KeyKind = KeyKind {
    var: "TEETOTUM_KEY",
    file: "face-key.pem",
    identity: "the faces' identity",
};

const FIRMWARE_KEY: KeyKind = KeyKind {
    var: "TEETOTUM_FIRMWARE_KEY",
    file: "firmware-key.pem",
    identity: "what the firmware trusts",
};

fn firmware(key: Option<&str>, image: &Path, signed: Option<&Path>) -> ExitCode {
    let key = match key_path(key, FIRMWARE_KEY).and_then(|path| load_key(&path, &FIRMWARE_KEY)) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("teetotum-pack: {e}");
            return ExitCode::FAILURE;
        }
    };
    let out = signed.map_or_else(|| image.with_extension("tfw"), Path::to_path_buf);
    let result = fs::read(image)
        .map_err(|e| e.to_string())
        .and_then(|bytes| teetotum_pack::firmware::sign(&bytes, &key).map_err(|e| format!("{e:?}")))
        .and_then(|bytes| {
            fs::write(&out, &bytes)
                .map(|()| bytes.len())
                .map_err(|e| e.to_string())
        });
    match result {
        Ok(len) => {
            println!(
                "{}: {len} bytes, signed by {}",
                out.display(),
                hex(&key.pk[..])
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: {e}", image.display());
            ExitCode::FAILURE
        }
    }
}

fn key_path(given: Option<&str>, kind: KeyKind) -> Result<PathBuf, String> {
    if let Some(path) = given {
        return Ok(path.into());
    }
    if let Some(path) = env::var_os(kind.var).filter(|p| !p.is_empty()) {
        return Ok(path.into());
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| format!("no --key, no ${} and no home directory", kind.var))?;
    Ok(Path::new(&home).join(".config/teetotum").join(kind.file))
}

fn load_key(path: &Path, kind: &KeyKind) -> Result<KeyPair, String> {
    match fs::read_to_string(path) {
        Ok(pem) => KeyPair::from_pem(&pem)
            .map_err(|_| format!("{}: not an Ed25519 private key in PEM", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => create_key(path, kind),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// A new key from the system's randomness, never over another file. On Unix only its owner may
/// read it; elsewhere it takes the permissions of the directory it lands in.
fn create_key(path: &Path, kind: &KeyKind) -> Result<KeyPair, String> {
    let key = KeyPair::generate();
    let failed = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        let mut dirs = DirBuilder::new();
        dirs.recursive(true);
        #[cfg(unix)]
        dirs.mode(0o700);
        dirs.create(dir).map_err(failed)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(key.sk.to_pem().as_bytes()))
        .map_err(failed)?;
    eprintln!(
        "created a new signing key at {} -- back it up, it is {}",
        path.display(),
        kind.identity
    );
    Ok(key)
}

struct Pack<'a> {
    module: &'a Path,
    slot: usize,
    write: bool,
    partitions: Option<&'a str>,
}

impl<'a> Pack<'a> {
    fn parse(args: &'a [String]) -> Option<Self> {
        let (mut module, mut slot, mut write, mut partitions) = (None, None, false, None);
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--slot" => slot = Some(args.next()?.parse().ok()?),
                "--write" => write = true,
                "--partitions" => partitions = Some(args.next()?.as_str()),
                flag if flag.starts_with('-') => return None,
                _ if module.is_some() => return None,
                path => module = Some(Path::new(path)),
            }
        }
        Some(Self {
            module: module?,
            slot: slot?,
            write,
            partitions,
        })
    }

    fn run(&self) -> ExitCode {
        match self.pack() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("teetotum-pack: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn pack(&self) -> Result<(), String> {
        let table = self
            .partitions
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("TEETOTUM_PARTITIONS")
                    .filter(|p| !p.is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| "partitions.csv".into());
        let (offset, size) = plugins_partition(&table)?;
        if self.slot >= size / slot::SLOT {
            return Err(format!(
                "slot {}, the partition has {}",
                self.slot,
                size / slot::SLOT
            ));
        }
        let wasm = fs::read(self.module).map_err(|e| io_failed(self.module, e))?;
        describe(&wasm).map_err(|e| format!("{}: {e}", self.module.display()))?;
        let header =
            slot::Header::of(&wasm).map_err(|e| format!("{}: {e}", self.module.display()))?;

        let out = self.module.with_extension("slot");
        let mut image = header.encode().to_vec();
        image.extend_from_slice(&wasm);
        fs::write(&out, &image).map_err(|e| io_failed(&out, e))?;
        let address = offset + self.slot * slot::SLOT;
        println!(
            "{}: {} bytes, id {}, slot {} at {address:#x}",
            out.display(),
            image.len(),
            hex(&header.id.bytes()),
            self.slot
        );

        let mut espflash = Command::new("espflash");
        espflash
            .args(["write-bin", "-B", "921600", &format!("{address:#x}")])
            .arg(&out);
        if !self.write {
            println!(
                "espflash write-bin -B 921600 {address:#x} {}",
                out.display()
            );
            return Ok(());
        }
        match espflash.status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("espflash {status}")),
            Err(e) => Err(format!("espflash: {e}")),
        }
    }
}

fn io_failed(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

/// Offset and size of the `plugins` partition in an ESP-IDF partition table.
fn plugins_partition(table: &Path) -> Result<(usize, usize), String> {
    let text = fs::read_to_string(table).map_err(|e| format!("{}: {e}", table.display()))?;
    let number = |field: &str| {
        let field = field.trim();
        match field.strip_prefix("0x") {
            Some(hex) => usize::from_str_radix(hex, 16).ok(),
            None => field.parse().ok(),
        }
    };
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| line.split(',').collect::<Vec<_>>())
        .find(|fields| fields.len() >= 5 && fields[0].trim() == "plugins")
        .and_then(|fields| Some((number(fields[3])?, number(fields[4])?)))
        .ok_or_else(|| {
            format!(
                "{}: no plugins partition with offset and size",
                table.display()
            )
        })
}

fn exit(passed: bool) -> ExitCode {
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
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
