//! ParamCipher v2, the per-request encryption used before login.
//!
//! The derivation follows the Zalo PC bundle:
//!
//! 1. `zcid = uppercase_hex(AES256(static_key, zero_iv, "{type},{imei},{first_launch_time}"))`
//! 2. `n = uppercase(MD5(zcid_ext))`
//! 3. `key = n_even[0..8] + zcid_even[0..12] + reverse(zcid_odd)[0..12]`
//! 4. `params = base64(AES256(key, zero_iv, JSON(payload)))`
//!
//! The signed parameter set also carries `type`, `client_version`,
//! `enc_ver` and `signkey`.

use crate::crypto::{
    aes256_cbc_decrypt, aes256_cbc_encrypt, base64_decode, base64_encode, md5_hex, sign_key_map,
    CryptoError,
};
use rand::Rng;
use serde_json::Value;
use std::collections::BTreeMap;

/// Static AES-256 key from Zalo's ZPC_SECRET_VALUES.
pub const PARAM_CIPHER_KEY: &[u8; 32] = b"3FC4F0D2AB50057BCE0D90D9187A22B1";

/// Client version sent as `client_version`.
pub const CLIENT_VERSION: &str = "697";

/// Zalo PC/Web API type sent as `type`.
pub const API_TYPE: u32 = 30;

/// A ParamCipher v2 instance for one device identity.
#[derive(Clone, Debug)]
pub struct ParamCipher {
    api_type: u32,
    zcid: String,
    zcid_ext: String,
    encrypt_key: String,
}

impl ParamCipher {
    /// Builds the cipher with a caller-supplied `zcid_ext`.
    ///
    /// Supplying the extension makes tests and captures reproducible. Use
    /// [`ParamCipher::random`] for a real request.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] when zcid encryption fails or the derived key
    /// would be shorter than 32 characters.
    pub fn new(
        imei: &str,
        first_launch_time: u64,
        api_type: u32,
        zcid_ext: impl Into<String>,
    ) -> Result<Self, CryptoError> {
        let zcid_ext = zcid_ext.into();
        if zcid_ext.is_empty() {
            return Err(CryptoError::EmptyZcidExt);
        }
        let plain = format!("{api_type},{imei},{first_launch_time}");
        let key = crate::config::param_cipher_key();
        let zcid = hex::encode_upper(aes256_cbc_encrypt(&key, plain.as_bytes())?);
        let encrypt_key = derive_encrypt_key(&zcid, &zcid_ext)?;
        Ok(Self {
            api_type,
            zcid,
            zcid_ext,
            encrypt_key,
        })
    }

    /// Builds the cipher with a fresh random hex extension.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] when zcid encryption fails.
    pub fn random(imei: &str, first_launch_time: u64, api_type: u32) -> Result<Self, CryptoError> {
        let mut rng = rand::thread_rng();
        let length = rng.gen_range(6..=12);
        let zcid_ext: String = (0..length)
            .map(|_| {
                std::char::from_digit(rng.gen_range(0..16), 16).expect("hex digit is always valid")
            })
            .collect();
        Self::new(imei, first_launch_time, api_type, zcid_ext)
    }

    /// The uppercase AES hex value identifying this device.
    pub fn zcid(&self) -> &str {
        &self.zcid
    }

    /// The random extension included in the signed parameters.
    pub fn zcid_ext(&self) -> &str {
        &self.zcid_ext
    }

    /// The 32-character ASCII AES-256 key used for `params`.
    pub fn encrypt_key(&self) -> &str {
        &self.encrypt_key
    }

    /// Encrypts a JSON payload into a base64 `params` value.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] when JSON serialization or AES encryption fails.
    pub fn encrypt_json(&self, payload: &Value) -> Result<String, CryptoError> {
        let plaintext = serde_json::to_vec(payload)?;
        let ciphertext = aes256_cbc_encrypt(self.encrypt_key.as_bytes(), &plaintext)?;
        Ok(base64_encode(&ciphertext))
    }

    /// Decrypts a base64 `params`/`data` value with this instance's key.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] when base64, AES or JSON decoding fails.
    pub fn decrypt_json(&self, encoded: &str) -> Result<Value, CryptoError> {
        let ciphertext = base64_decode(encoded)?;
        let plaintext = aes256_cbc_decrypt(self.encrypt_key.as_bytes(), &ciphertext)?;
        Ok(serde_json::from_slice(&plaintext)?)
    }

    /// Builds the signed parameter map for `tag`.
    ///
    /// The signature is computed before adding `signkey` itself, matching
    /// the bundle's `encryptParam`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] when payload encryption fails.
    pub fn encrypt_params(
        &self,
        payload: &Value,
        tag: &str,
    ) -> Result<BTreeMap<String, String>, CryptoError> {
        let mut params = BTreeMap::new();
        params.insert("zcid".to_owned(), self.zcid.clone());
        params.insert("zcid_ext".to_owned(), self.zcid_ext.clone());
        params.insert("enc_ver".to_owned(), "v2".to_owned());
        params.insert("params".to_owned(), self.encrypt_json(payload)?);
        params.insert("type".to_owned(), self.api_type.to_string());
        params.insert("client_version".to_owned(), crate::config::client_version());
        let signature = sign_key_map(tag, &params);
        params.insert("signkey".to_owned(), signature);
        Ok(params)
    }
}

fn derive_encrypt_key(zcid: &str, zcid_ext: &str) -> Result<String, CryptoError> {
    let digest = md5_hex(zcid_ext.as_bytes()).to_uppercase();
    let n_even: Vec<char> = digest.chars().step_by(2).collect();
    let zcid_even: Vec<char> = zcid.chars().step_by(2).collect();
    let mut zcid_odd_reversed: Vec<char> = zcid.chars().skip(1).step_by(2).collect();
    zcid_odd_reversed.reverse();
    if n_even.len() < 8 || zcid_even.len() < 12 || zcid_odd_reversed.len() < 12 {
        return Err(CryptoError::ShortZcid);
    }
    Ok(n_even[..8]
        .iter()
        .chain(&zcid_even[..12])
        .chain(&zcid_odd_reversed[..12])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMEI: &str = "0123456789abcdef01234567";
    const FIRST_LAUNCH: u64 = 1_700_000_000;
    const ZCID_EXT: &str = "a1b2c3d4e5";
    const REFERENCE_ZCID: &str = "DC4660601A27E55E0629DF577C0410D5015889B84CA0FB4B2F59C7172D3A2BEB991F2D14CD3929E9C2A10F838E90BC14";
    const REFERENCE_KEY: &str = "C3A3B143D46612E502D54C0E3F12999D";
    const REFERENCE_PARAMS: &str = "eSjfyIG1VfRw2wMC2M4VNm+bMRwykfJtCbGHzZR/a50=";

    fn cipher() -> ParamCipher {
        ParamCipher::new(IMEI, FIRST_LAUNCH, API_TYPE, ZCID_EXT).expect("build cipher")
    }

    #[test]
    fn derives_the_reference_zcid_and_key() {
        let cipher = cipher();
        assert_eq!(cipher.zcid(), REFERENCE_ZCID);
        assert_eq!(cipher.encrypt_key(), REFERENCE_KEY);
        assert_eq!(cipher.zcid_ext(), ZCID_EXT);
    }

    #[test]
    fn encrypts_the_reference_payload() {
        let payload = serde_json::json!({"phone": "0900000000"});
        let encrypted = cipher().encrypt_json(&payload).expect("encrypt");
        assert_eq!(encrypted, REFERENCE_PARAMS);
        assert_eq!(cipher().decrypt_json(&encrypted).expect("decrypt"), payload);
    }

    #[test]
    fn signed_parameters_are_complete_and_verifiable() {
        let params = cipher()
            .encrypt_params(&serde_json::json!({"a": 1}), "reqqr")
            .expect("params");
        assert_eq!(params.get("enc_ver").map(String::as_str), Some("v2"));
        assert_eq!(params.get("type").map(String::as_str), Some("30"));
        assert_eq!(
            params.get("client_version").map(String::as_str),
            Some(CLIENT_VERSION)
        );
        let signature = params.get("signkey").expect("signkey").clone();
        let mut unsigned = params;
        unsigned.remove("signkey");
        assert_eq!(signature, sign_key_map("reqqr", &unsigned));
    }
}
