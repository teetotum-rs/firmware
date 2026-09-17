#![cfg(feature = "std")]

use ed25519_compact::{KeyPair, Seed};
use teetotum_pack::firmware::{self, Error, MAGIC, Verifier};

fn key() -> KeyPair {
    KeyPair::from_seed(Seed::new([7; 32]))
}

fn image() -> Vec<u8> {
    let mut image = vec![MAGIC];
    image.extend((0..5000u32).map(|n| n as u8));
    image
}

fn check_in_pieces(signed: &[u8], key: &KeyPair, piece: usize) -> Result<(), Error> {
    let (image, signature) = firmware::split(signed)?;
    let mut verifier = Verifier::new(&key.pk, &signature)?;
    for bytes in image.chunks(piece) {
        verifier.absorb(bytes)?;
    }
    verifier.verify()
}

#[test]
fn a_signed_image_passes_in_any_pieces() {
    let key = key();
    let signed = firmware::sign(&image(), &key).unwrap();
    for piece in [1, 240, 4096, 10_000] {
        assert_eq!(check_in_pieces(&signed, &key, piece), Ok(()));
    }
}

#[test]
fn one_changed_byte_fails() {
    let key = key();
    let mut signed = firmware::sign(&image(), &key).unwrap();
    signed[1234] ^= 1;
    assert_eq!(check_in_pieces(&signed, &key, 240), Err(Error::Signature));
}

#[test]
fn a_missing_last_piece_fails() {
    let key = key();
    let signed = firmware::sign(&image(), &key).unwrap();
    let (image, signature) = firmware::split(&signed).unwrap();
    let mut verifier = Verifier::new(&key.pk, &signature).unwrap();
    verifier.absorb(&image[..image.len() - 1]).unwrap();
    assert_eq!(verifier.verify(), Err(Error::Signature));
}

#[test]
fn another_key_fails() {
    let signed = firmware::sign(&image(), &key()).unwrap();
    let other = KeyPair::from_seed(Seed::new([8; 32]));
    assert_eq!(check_in_pieces(&signed, &other, 240), Err(Error::Signature));
}

#[test]
fn only_esp_images_are_taken() {
    let key = key();
    assert_eq!(firmware::sign(&[0; 100], &key), Err(Error::NotAnImage));
    let mut signed = firmware::sign(&image(), &key).unwrap();
    signed[0] = 0;
    assert_eq!(check_in_pieces(&signed, &key, 240), Err(Error::NotAnImage));
    assert_eq!(firmware::split(&[MAGIC; 64]), Err(Error::NotAnImage));
}
