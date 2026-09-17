//! Checks a face before any of its code runs: whether its signature holds, and who it is.
//!
//! The manifest and the signature section are read by [`teetotum_face::manifest`], with the code
//! a face writes them with. What takes Ed25519 and SHA-512 is here, so a face does not carry it.
//! Checking allocates nothing, so the firmware and a tool on the host run the same checks;
//! signing, with the `std` feature, is for the host.
#![cfg_attr(not(feature = "std"), no_std)]

use core::fmt;

use ed25519_compact::{PublicKey, Signature, sha512};
pub use teetotum_face::manifest::{self, Manifest, Signed};

pub mod listing;
pub mod settings;
pub mod slot;

/// Who a face is, in the eight bytes the settings record keeps of it.
///
/// **The author's key and the face's name, hashed together**, so a face of the same name signed
/// by another key is another face. Eight bytes of SHA-512 keep the faces of one device apart;
/// anything that guards a face's secrets has to use the whole key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PluginId([u8; PluginId::LEN]);

impl PluginId {
    pub const LEN: usize = 8;

    pub fn new(key: &[u8; manifest::KEY_LEN], name: &str) -> Self {
        let mut hash = sha512::Hash::new();
        hash.update(key);
        hash.update(name.as_bytes());
        let digest = hash.finalize();
        let mut id = [0; Self::LEN];
        id.copy_from_slice(&digest[..Self::LEN]);
        Self(id)
    }

    /// The id a module claims. The signature is not checked here; [`verify`] does that.
    pub fn of(wasm: &[u8]) -> Result<Self, manifest::Error> {
        let manifest = Manifest::read(wasm)?;
        Ok(Self::new(Signed::read(wasm)?.key, manifest.name()))
    }

    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; Self::LEN] {
        self.0
    }
}

/// Why a module's signature was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// No signature section to check, or one in the wrong place.
    Manifest(manifest::Error),
    /// The signature does not hold: the bytes are not what the key's holder signed.
    Signature,
}

impl From<manifest::Error> for Error {
    fn from(e: manifest::Error) -> Self {
        Self::Manifest(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(e) => write!(f, "{e}"),
            Self::Signature => f.write_str("signature does not match its bytes and key"),
        }
    }
}

/// Whether a module's signature holds for its bytes and the key it names.
pub fn verify(wasm: &[u8]) -> Result<(), Error> {
    let signed = Signed::read(wasm)?;
    PublicKey::new(*signed.key)
        .verify(signed.message, &Signature::new(*signed.signature))
        .map_err(|_| Error::Signature)
}

/// The module without its signature section, if it has one: the bytes a signature is made over.
pub fn unsigned(wasm: &[u8]) -> Result<&[u8], Error> {
    match Signed::read(wasm) {
        Ok(signed) => Ok(signed.message),
        Err(manifest::Error::Unsigned) => Ok(wasm),
        Err(e) => Err(e.into()),
    }
}

/// The module signed by `key`, with its key and signature as the last section.
///
/// A signature the module already has is replaced, not added to. Ed25519 signs without chance,
/// so the same bytes and key always give the same file.
#[cfg(feature = "std")]
pub fn sign(wasm: &[u8], key: &ed25519_compact::KeyPair) -> Result<Vec<u8>, Error> {
    const NAME: &[u8] = manifest::SIGNATURE.as_bytes();
    const BODY: usize = 1 + NAME.len() + manifest::KEY_LEN + manifest::SIGNATURE_LEN;
    const {
        assert!(
            BODY < 0x80,
            "section and name lengths fit one LEB128 byte each"
        )
    };
    let message = unsigned(wasm)?;
    let mut signed = Vec::with_capacity(message.len() + 2 + BODY);
    signed.extend_from_slice(message);
    signed.extend_from_slice(&[0, BODY as u8, NAME.len() as u8]);
    signed.extend_from_slice(NAME);
    signed.extend_from_slice(&key.pk[..]);
    signed.extend_from_slice(&key.sk.sign(message, None)[..]);
    Ok(signed)
}
