//! Zalo Web/PC protocol primitives.
//!
//! The module implements the parts needed before any authenticated request:
//! AES-256-CBC with Zalo's zero IV, MD5 request signing and ParamCipher v2.
//! It has no network code and no knowledge of accounts; `zalo-client`
//! builds the HTTP layer on top.

#![forbid(unsafe_code)]

pub mod callv2;
pub mod config;
pub mod crypto;
pub mod param_cipher;

pub use callv2::{
    call_v2_decrypt, call_v2_encrypt, load_call_v2_key, CallV2Frame, CallV2FrameParser,
};
pub use config::{
    active_config, api_type, apply_env_overrides, apply_overrides, client_version, init_from_dir,
    load_overrides_from_file, param_cipher_key, reset_defaults, user_agent, ProtocolConfig,
    ProtocolConfigError, ProtocolOverrides, DEFAULT_USER_AGENT, DEFAULT_ZALO_DESKTOP_VERSION,
    PROTOCOL_CONFIG_FILE,
};
pub use crypto::{
    aes256_cbc_decrypt, aes256_cbc_encrypt, aes_cbc_decrypt, aes_cbc_encrypt, base64_decode,
    base64_encode, md5_hex, sign_key, sign_key_map, CryptoError,
};
pub use param_cipher::{ParamCipher, API_TYPE, CLIENT_VERSION, PARAM_CIPHER_KEY};
