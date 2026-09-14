//! Signing, checked against the faces the firmware ships. Needs the `std` feature:
//!
//! ```sh
//! RUSTFLAGS= cargo +stable test -p teetotum-pack --features std --target x86_64-unknown-linux-gnu
//! ```
#![cfg(feature = "std")]

use ed25519_compact::{KeyPair, Seed};
use teetotum_pack::{Error, Manifest, Signed, manifest, sign, unsigned, verify};

const BUNDLED: [&[u8]; 3] = [
    include_bytes!("../../firmware/assets/plugins/hid-remote.wasm"),
    include_bytes!("../../firmware/assets/plugins/nearby.wasm"),
    include_bytes!("../../firmware/assets/plugins/teetotum-plugin.wasm"),
];

/// A key from a fixed seed, so no private key has to live in the repository.
fn test_key() -> KeyPair {
    KeyPair::from_seed(Seed::new([7; Seed::BYTES]))
}

#[test]
fn signing_again_replaces_the_signature() {
    let key = test_key();
    for wasm in BUNDLED {
        let resigned = sign(wasm, &key).unwrap();
        assert_eq!(verify(&resigned), Ok(()));
        assert_eq!(resigned.len(), wasm.len());
        let signed = Signed::read(&resigned).unwrap();
        assert_eq!(signed.message, Signed::read(wasm).unwrap().message);
        assert_eq!(signed.key, &*key.pk);
        assert_eq!(sign(unsigned(wasm).unwrap(), &key).unwrap(), resigned);
    }
}

/// A face built for a newer host ABI, signed properly, is refused by its manifest and not by
/// its signature.
#[test]
fn a_newer_abi_is_refused_even_when_signed() {
    let key = test_key();
    for wasm in BUNDLED {
        let name = manifest::SECTION.as_bytes();
        let contents = wasm
            .windows(name.len() + 1)
            .position(|w| w[0] as usize == name.len() && &w[1..] == name)
            .unwrap()
            + 1
            + name.len();
        let abi = contents + manifest::LEN - 8;
        let current = Manifest::read(wasm).unwrap().abi();
        assert_eq!(wasm[abi..abi + 2], current.to_le_bytes(), "ABI offset");

        let mut newer = unsigned(wasm).unwrap().to_vec();
        newer[abi..abi + 2].copy_from_slice(&(current + 1).to_le_bytes());
        let newer = sign(&newer, &key).unwrap();
        assert_eq!(verify(&newer), Ok(()));
        assert_eq!(
            Manifest::read(&newer).map(|_| ()),
            Err(manifest::Error::Abi(current + 1))
        );
    }
}

#[test]
fn a_malformed_signature_section_is_not_signed_over() {
    let mut behind = BUNDLED[0].to_vec();
    behind.extend_from_slice(&[0, 2, 1, b'x']);
    assert_eq!(
        sign(&behind, &test_key()),
        Err(Error::Manifest(manifest::Error::Signature))
    );
}
