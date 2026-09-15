fn main() {
    linker_be_nice();
    // The linker calls this binary again to explain an error, and then there is no OUT_DIR.
    if std::env::var_os("OUT_DIR").is_some() {
        qr_codes();
        commit();
    }
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}

/// Sets `TEETOTUM_COMMIT` for About: the short hash of the commit built from, with `+` when
/// tracked files differ from it, or `unknown` outside a git checkout.
fn commit() {
    use std::process::Command;

    let git = |args: &[&str]| {
        // Without it `git status` refreshes the index, which the next build takes for a change.
        Command::new("git")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    // HEAD moves with a checkout, the branch with a commit, the index with a staged edit; the
    // source trees catch the rest. A path that does not exist would rerun every build.
    let branch = git(&["symbolic-ref", "-q", "HEAD"]);
    let mut watched = vec![
        String::from("HEAD"),
        String::from("index"),
        String::from("packed-refs"),
    ];
    watched.extend(branch);
    for name in &watched {
        let path = git(&["rev-parse", "--git-path", name]);
        if let Some(path) = path.filter(|path| std::path::Path::new(path).exists()) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    for tree in [
        "src",
        "../teetotum/src",
        "../teetotum-face/src",
        "../teetotum-pack/src",
    ] {
        println!("cargo:rerun-if-changed={tree}");
    }

    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|changes| !changes.is_empty());
    let commit = match git(&["rev-parse", "--short=7", "HEAD"]) {
        Some(hash) if dirty => format!("{hash}+"),
        Some(hash) => hash,
        None => String::from("unknown"),
    };
    println!("cargo:rustc-env=TEETOTUM_COMMIT={commit}");
}

#[allow(dead_code)]
mod links {
    include!("src/links.rs");
}

/// Encodes every link in `src/links.rs` into `$OUT_DIR/qr_codes.rs`, one bit a module.
///
/// At build time rather than on the device: a code is 80 to 180 bytes of flash this way, and
/// the image carries no encoder and needs no work buffer for one.
fn qr_codes() {
    use qrcodegen::{QrCode, QrCodeEcc};
    use std::fmt::Write as _;

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/links.rs");
    let mut out = String::from("// Generated by build.rs from src/links.rs.\n");
    writeln!(out, "pub static CODES: [Code; {}] = [", links::LINKS.len()).unwrap();
    for link in &links::LINKS {
        // Medium survives a little glare on the glass; qrcodegen raises the level for free
        // wherever the version it needs has room.
        let code = QrCode::encode_text(link.url, QrCodeEcc::Medium).expect("a URL fits a QR code");
        let size = code.size();
        let mut bits = vec![0u8; ((size * size) as usize).div_ceil(8)];
        for y in 0..size {
            for x in 0..size {
                if code.get_module(x, y) {
                    let i = (y * size + x) as usize;
                    bits[i / 8] |= 0x80 >> (i % 8);
                }
            }
        }
        writeln!(out, "    Code {{ size: {size}, modules: &{bits:?} }},").unwrap();
    }
    out.push_str("];\n");
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("qr_codes.rs");
    std::fs::write(path, out).unwrap();
}
