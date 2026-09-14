//! The checks against the faces the firmware ships, run on the host:
//!
//! ```sh
//! RUSTFLAGS= cargo +stable test -p teetotum-pack --target x86_64-unknown-linux-gnu
//! ```
//!
//! `RUSTFLAGS=` and `+stable` step around the Xtensa settings in `.cargo/config.toml`.

use teetotum_pack::{Error, PluginId, manifest, verify};

const HID_REMOTE: &[u8] = include_bytes!("../../firmware/assets/plugins/hid-remote.wasm");
const NEARBY: &[u8] = include_bytes!("../../firmware/assets/plugins/nearby.wasm");
const TEETOTUM: &[u8] = include_bytes!("../../firmware/assets/plugins/teetotum-plugin.wasm");
const UNSIGNED: &[u8] = include_bytes!("../../firmware/assets/plugins/hid-own.wasm");

const BUNDLED: [&[u8]; 3] = [HID_REMOTE, NEARBY, TEETOTUM];

fn flipped(wasm: &[u8], at: usize) -> Vec<u8> {
    let mut copy = wasm.to_vec();
    copy[at] ^= 0x01;
    copy
}

#[test]
fn bundled_faces_verify() {
    for wasm in BUNDLED {
        assert_eq!(verify(wasm), Ok(()));
    }
}

/// The settings record names faces by these ids; a changed derivation would orphan them.
/// Computed independently by `tools/pack-slot.py`.
#[test]
fn ids_are_pinned() {
    let id = |hex: &str| {
        let mut bytes = [0; PluginId::LEN];
        for (n, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * n..2 * n + 2], 16).unwrap();
        }
        PluginId::from_bytes(bytes)
    };
    assert_eq!(PluginId::of(HID_REMOTE), Ok(id("41a9ad2d2290788d")));
    assert_eq!(PluginId::of(NEARBY), Ok(id("a699c0784d62a9df")));
    assert_eq!(PluginId::of(TEETOTUM), Ok(id("6799220a4230b177")));
}

#[test]
fn a_flipped_byte_breaks_the_signature() {
    for wasm in BUNDLED {
        let name = teetotum_pack::Manifest::read(wasm)
            .unwrap()
            .name()
            .to_owned();
        let in_manifest = wasm
            .windows(name.len())
            .position(|w| w == name.as_bytes())
            .unwrap();
        let places = [
            ("code", wasm.len() / 3),
            ("manifest", in_manifest),
            (
                "key",
                wasm.len() - manifest::KEY_LEN - manifest::SIGNATURE_LEN,
            ),
            ("signature", wasm.len() - 1),
        ];
        for (what, at) in places {
            assert_eq!(verify(&flipped(wasm, at)), Err(Error::Signature), "{what}");
        }
    }
}

#[test]
fn the_signature_section_has_to_be_last_and_single() {
    let mut behind = NEARBY.to_vec();
    behind.extend_from_slice(&[0, 2, 1, b'x']);
    assert_eq!(
        verify(&behind),
        Err(Error::Manifest(manifest::Error::Signature))
    );

    let name = manifest::SIGNATURE.as_bytes();
    let size = 1 + name.len() + manifest::KEY_LEN + manifest::SIGNATURE_LEN;
    assert!(size < 0x80, "one LEB128 byte");
    let mut twice = NEARBY.to_vec();
    twice.extend_from_slice(&[0, size as u8, name.len() as u8]);
    twice.extend_from_slice(name);
    twice.extend_from_slice(&NEARBY[NEARBY.len() - manifest::KEY_LEN - manifest::SIGNATURE_LEN..]);
    assert_eq!(
        verify(&twice),
        Err(Error::Manifest(manifest::Error::Signature))
    );
}

#[test]
fn an_unsigned_face_is_refused() {
    assert_eq!(
        verify(UNSIGNED),
        Err(Error::Manifest(manifest::Error::Unsigned))
    );
}
