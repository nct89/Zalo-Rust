//! AES-CBC, MD5 and request signing.

use aes::{Aes128, Aes192, Aes256};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use cbc::cipher::{
    block_padding::Pkcs7, BlockCipher, BlockDecryptMut, BlockEncryptMut, KeyInit, KeyIvInit,
};
use md5::{Digest, Md5};
use std::collections::BTreeMap;
use thiserror::Error;

const ZERO_IV: [u8; 16] = [0; 16];

/// A protocol-level failure while encrypting, decrypting or parsing.
#[derive(Debug, Error)]
pub enum CryptoError {
    /// AES needs a 16, 24 or 32-byte key.
    #[error("AES key must be 16, 24 or 32 bytes, got {0}")]
    KeyLength(usize),
    /// The cipher rejected the key or IV length.
    #[error("AES initialization failed")]
    AesInit,
    /// The ciphertext has invalid PKCS#7 padding.
    #[error("PKCS#7 padding is invalid")]
    Padding,
    /// A base64 field is malformed.
    #[error("invalid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// A JSON payload is malformed.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// ParamCipher received an empty zcid_ext.
    #[error("zcid_ext is empty")]
    EmptyZcidExt,
    /// ParamCipher's zcid is too short for key derivation.
    #[error("zcid is too short for key derivation")]
    ShortZcid,
}

/// Encrypts with AES-CBC, a zero IV and PKCS#7 padding, choosing AES-128,
/// AES-192 or AES-256 from the key length.
///
/// Zalo uses AES-256 for ParamCipher and AES-128 for the session data key
/// (¤zpw_enk¤).
///
/// # Errors
///
/// Returns [`CryptoError`] when the key length is invalid or padding fails.
pub fn aes_cbc_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    match key.len() {
        16 => encrypt_with::<Aes128>(key, plaintext),
        24 => encrypt_with::<Aes192>(key, plaintext),
        32 => encrypt_with::<Aes256>(key, plaintext),
        other => Err(CryptoError::KeyLength(other)),
    }
}

/// Decrypts AES-CBC with a zero IV and PKCS#7 padding, choosing AES-128,
/// AES-192 or AES-256 from the key length.
///
/// # Errors
///
/// Returns [`CryptoError`] when the key length is invalid, the ciphertext is
/// not a multiple of 16 bytes or the padding is invalid.
pub fn aes_cbc_decrypt(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    match key.len() {
        16 => decrypt_with::<Aes128>(key, ciphertext),
        24 => decrypt_with::<Aes192>(key, ciphertext),
        32 => decrypt_with::<Aes256>(key, ciphertext),
        other => Err(CryptoError::KeyLength(other)),
    }
}

/// Encrypts with AES-256-CBC, a zero IV and PKCS#7 padding.
///
/// # Errors
///
/// Returns [`CryptoError`] when the key is not 32 bytes or padding fails.
pub fn aes256_cbc_encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    aes_cbc_encrypt(key, plaintext)
}

/// Decrypts AES-256-CBC with a zero IV and PKCS#7 padding.
///
/// # Errors
///
/// Returns [`CryptoError`] when the key is wrong, the ciphertext is not a
/// multiple of 16 bytes or the padding is invalid.
pub fn aes256_cbc_decrypt(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    aes_cbc_decrypt(key, ciphertext)
}

fn encrypt_with<C>(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError>
where
    C: BlockCipher + BlockEncryptMut + KeyInit,
{
    let mut buffer = vec![0_u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let cipher =
        cbc::Encryptor::<C>::new_from_slices(key, &ZERO_IV).map_err(|_| CryptoError::AesInit)?;
    let encrypted = cipher
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len())
        .map_err(|_| CryptoError::Padding)?;
    Ok(encrypted.to_vec())
}

fn decrypt_with<C>(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError>
where
    C: BlockCipher + BlockDecryptMut + KeyInit,
{
    let mut buffer = ciphertext.to_vec();
    let cipher =
        cbc::Decryptor::<C>::new_from_slices(key, &ZERO_IV).map_err(|_| CryptoError::AesInit)?;
    let plaintext = cipher
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|_| CryptoError::Padding)?;
    Ok(plaintext.to_vec())
}

/// Base64-encodes bytes with the standard alphabet.
pub fn base64_encode(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Decodes a standard base64 string.
///
/// # Errors
///
/// Returns [`CryptoError::Base64`] when the input is not valid base64.
pub fn base64_decode(text: &str) -> Result<Vec<u8>, CryptoError> {
    Ok(BASE64.decode(text)?)
}

/// Lowercase MD5 hex digest.
pub fn md5_hex(input: &[u8]) -> String {
    hex::encode(Md5::digest(input))
}

/// Computes Zalo's request signature.
///
/// The input is `"zsecure" + tag + concat(values sorted by key)`. Values must
/// be passed in sorted-key order; [`sign_key_map`] does that for a map.
pub fn sign_key<'a, I>(tag: &str, values: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let mut input = String::from("zsecure");
    input.push_str(tag);
    for value in values {
        input.push_str(value);
    }
    md5_hex(input.as_bytes())
}

/// Computes Zalo's request signature over a map sorted by key.
pub fn sign_key_map(tag: &str, params: &BTreeMap<String, String>) -> String {
    sign_key(tag, params.values().map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARAM_KEY: &[u8; 32] = b"3FC4F0D2AB50057BCE0D90D9187A22B1";

    #[test]
    fn sign_key_matches_the_bundle_formula() {
        // md5("zsecure" + "reqqr" + "1" + "2")
        assert_eq!(
            sign_key("reqqr", ["1", "2"]),
            "34d0942f32413d0f943fe0a8247ecd74"
        );
    }

    #[test]
    fn aes_encrypt_matches_the_reference_vector() {
        let encrypted = aes256_cbc_encrypt(PARAM_KEY, br#"{"x":1}"#).expect("encrypt");
        assert_eq!(base64_encode(&encrypted), "JFWw0Z1IArzLKF8UujnXMw==");
    }

    #[test]
    fn aes128_encrypt_matches_the_reference_vector() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let encrypted = aes_cbc_encrypt(&key, br#"{"x":1}"#).expect("encrypt");
        assert_eq!(base64_encode(&encrypted), "CoPAru1Snrbw1bezhk2xQg==");
        assert_eq!(
            aes_cbc_decrypt(&key, &encrypted).expect("decrypt"),
            br#"{"x":1}"#
        );
    }

    #[test]
    fn aes_round_trips_and_rejects_a_bad_key() {
        let encrypted = aes256_cbc_encrypt(PARAM_KEY, b"hello zalo").expect("encrypt");
        assert_eq!(
            aes256_cbc_decrypt(PARAM_KEY, &encrypted).expect("decrypt"),
            b"hello zalo"
        );
        assert!(matches!(
            aes256_cbc_encrypt(b"short", b"x"),
            Err(CryptoError::KeyLength(5))
        ));
    }

    #[test]
    fn base64_helpers_round_trip() {
        let encoded = base64_encode(b"zalo");
        assert_eq!(encoded, "emFsbw==");
        assert_eq!(base64_decode(&encoded).expect("decode"), b"zalo");
    }
}
