//! Checks a face before any of its code runs: whether its signature holds, and who it is.
//!
//! The manifest and the signature section are read by [`teetotum_face::manifest`], with the code
//! a face writes them with. What takes Ed25519 and SHA-512 is here, so a face does not carry it.
//! Nothing allocates: the firmware and a tool on the host run the same checks.
#![no_std]

use core::fmt;

use ed25519_compact::{PublicKey, Signature, sha512};
pub use teetotum_face::manifest::{self, Manifest, Signed};

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
