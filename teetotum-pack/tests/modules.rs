//! The checks against the faces the firmware ships, run on the host:
//!
//! ```sh
//! RUSTFLAGS= cargo +stable test -p teetotum-pack --target x86_64-unknown-linux-gnu
//! ```
//!
//! `RUSTFLAGS=` and `+stable` step around the Xtensa settings in `.cargo/config.toml`.

use teetotum_pack::{Error, PluginId, manifest, slot, verify};

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
/// Computed outside this crate, from SHA-512 over key and name.
#[test]
fn ids_are_pinned() {
    let id = |hex| PluginId::from_bytes(from_hex(hex));
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

fn from_hex<const N: usize>(hex: &str) -> [u8; N] {
    let mut bytes = [0; N];
    for (n, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * n..2 * n + 2], 16).unwrap();
    }
    bytes
}

/// The header for nearby.wasm, computed outside this crate; flash already holds slots in this format.
const NEARBY_HEADER: &str = "5454505301ff0000a699c0784d62a9df621c00008c538adce20e63b9\
                             43090f8f26f25d875280eebbf6191e3ef28dcedd8f000e49000000000000000000000000";

#[test]
fn a_slot_header_encodes_as_written_before() {
    let golden: [u8; slot::HEADER] = from_hex(NEARBY_HEADER);
    let header = slot::Header::of(NEARBY).unwrap();
    assert_eq!(header.encode(), golden);
    assert_eq!(slot::Header::decode(&golden), Some(header));
    assert!(!header.accepted);
    assert_eq!(header.len, NEARBY.len());
}

#[test]
fn an_accepted_slot_header_round_trips() {
    let mut raw: [u8; slot::HEADER] = from_hex(NEARBY_HEADER);
    raw[slot::STATE] = slot::ACCEPTED;
    let header = slot::Header::decode(&raw).unwrap();
    assert!(header.accepted);
    assert_eq!(header.encode(), raw);
}

#[test]
fn erased_or_foreign_bytes_hold_no_slot_header() {
    assert_eq!(slot::Header::decode(&[0xff; slot::HEADER]), None);
    let mut format: [u8; slot::HEADER] = from_hex(NEARBY_HEADER);
    format[4] = 2;
    assert_eq!(slot::Header::decode(&format), None);
    let mut long: [u8; slot::HEADER] = from_hex(NEARBY_HEADER);
    long[16..20].copy_from_slice(&(slot::MODULE_MAX as u32 + 1).to_le_bytes());
    assert_eq!(slot::Header::decode(&long), None);
}

#[test]
fn a_slot_header_matches_only_its_module() {
    let header = slot::Header::of(NEARBY).unwrap();
    assert_eq!(header.matches(NEARBY), Ok(()));
    assert_eq!(
        header.matches(&flipped(NEARBY, NEARBY.len() / 3)),
        Err(slot::Error::Hash)
    );
    assert_eq!(header.matches(TEETOTUM), Err(slot::Error::Hash));

    let mut raw = header.encode();
    raw[8..16].copy_from_slice(&PluginId::of(HID_REMOTE).unwrap().bytes());
    let other_id = slot::Header::decode(&raw).unwrap();
    assert_eq!(other_id.matches(NEARBY), Err(slot::Error::Id));
}

#[test]
fn a_module_too_long_for_a_slot_gets_no_header() {
    assert_eq!(
        slot::Header::of(&vec![0; slot::MODULE_MAX + 1]),
        Err(slot::Error::TooLong)
    );
    assert_eq!(slot::Header::of(UNSIGNED), Err(slot::Error::Id));
}
