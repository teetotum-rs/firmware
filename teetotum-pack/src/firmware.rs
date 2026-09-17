//! A firmware image signed for an update over BLE.
//!
//! ```text
//! image       an ESP application image, as `espflash save-image` writes it
//! signature   64 bytes, Ed25519 over the image
//! ```
//!
//! The signature travels in the update command, not into the flash; the knob checks it over the
//! bytes as they arrive and selects the new partition only when it holds. Firmware is signed
//! with a key of its own, so a key that signs faces cannot install firmware.

use ed25519_compact::{PublicKey, Signature, VerifyingState};

/// Bytes of the signature at the end of a signed image.
pub const SIGNATURE: usize = Signature::BYTES;
/// Bytes of the public key an image is checked against.
pub const KEY: usize = PublicKey::BYTES;
/// The first byte of every ESP application image.
pub const MAGIC: u8 = 0xE9;

/// Why an image is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shorter than a signature and one byte, or not starting with [`MAGIC`].
    NotAnImage,
    /// The key or the signature cannot be one.
    Malformed,
    /// The signature does not hold for these bytes and this key.
    Signature,
}

/// A signed image, split into the image and its signature.
pub fn split(signed: &[u8]) -> Result<(&[u8], [u8; SIGNATURE]), Error> {
    if signed.len() <= SIGNATURE {
        return Err(Error::NotAnImage);
    }
    let (image, signature) = signed.split_at(signed.len() - SIGNATURE);
    if image[0] != MAGIC {
        return Err(Error::NotAnImage);
    }
    let mut raw = [0; SIGNATURE];
    raw.copy_from_slice(signature);
    Ok((image, raw))
}

/// Checks an image piece by piece, so it never has to be held whole.
#[derive(Clone)]
pub struct Verifier {
    state: VerifyingState,
    started: bool,
}

impl Verifier {
    pub fn new(key: &[u8; KEY], signature: &[u8; SIGNATURE]) -> Result<Self, Error> {
        PublicKey::new(*key)
            .verify_incremental(&Signature::new(*signature))
            .map(|state| Self {
                state,
                started: false,
            })
            .map_err(|_| Error::Malformed)
    }

    /// Takes the next bytes of the image; the first must be [`MAGIC`].
    pub fn absorb(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if !self.started {
            match bytes.first() {
                Some(&MAGIC) => self.started = true,
                Some(_) => return Err(Error::NotAnImage),
                None => return Ok(()),
            }
        }
        self.state.absorb(bytes);
        Ok(())
    }

    /// Whether the signature holds for every byte taken.
    pub fn verify(&self) -> Result<(), Error> {
        if !self.started {
            return Err(Error::NotAnImage);
        }
        self.state.verify().map_err(|_| Error::Signature)
    }
}

/// The image with its signature appended.
#[cfg(feature = "std")]
pub fn sign(image: &[u8], key: &ed25519_compact::KeyPair) -> Result<std::vec::Vec<u8>, Error> {
    if image.first() != Some(&MAGIC) {
        return Err(Error::NotAnImage);
    }
    let mut signed = image.to_vec();
    signed.extend_from_slice(key.sk.sign(image, None).as_ref());
    Ok(signed)
}
