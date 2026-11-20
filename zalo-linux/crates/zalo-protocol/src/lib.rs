//! Zalo Web/PC protocol primitives.
//!
//! The module implements the parts needed before any authenticated request:
//! AES-256-CBC with Zalo's zero IV, MD5 request signing and ParamCipher v2.
//! It has no network code and no knowledge of accounts; `zalo-client`
//! builds the HTTP layer on top.

#![forbid(unsafe_code)]

pub mod callv2;
pub mod crypto;
pub mod param_cipher;

pub use callv2::{
    call_v2_decrypt, call_v2_encrypt, load_call_v2_key, CallV2Frame, CallV2FrameParser,
};
pub use crypto::{
    aes256_cbc_decrypt, aes256_cbc_encrypt, aes_cbc_decrypt, aes_cbc_encrypt, base64_decode,
    base64_encode, md5_hex, sign_key, sign_key_map, CryptoError,
};
pub use param_cipher::{ParamCipher, API_TYPE, CLIENT_VERSION, PARAM_CIPHER_KEY};
