//! The call-v2 TCP transport framing, ported from the reference call
//! implementation (`callv2-crypto.js`).
//!
//! Frames are `<hex>` terminated by `$`, where `hex = AES-128-CBC(JSON, zero IV,
//! PKCS7)` with Zalo's `callV2TransportAesKey`. An optional `<token>\n` line comes
//! first. Messages over 4000 hex chars are split as
//! `<hex>#<msgId>#<total>#<index>#`.

use crate::crypto::{aes_cbc_decrypt, aes_cbc_encrypt, base64_decode, CryptoError};
use serde_json::Value;
use std::collections::BTreeMap;

/// One frame delivered by the parser.
#[derive(Clone, Debug, PartialEq)]
pub enum CallV2Frame {
    /// The first-line token.
    Token(String),
    /// A parsed JSON frame.
    Json(Value),
    /// A decrypted frame that is not JSON.
    Text(String),
    /// A frame that failed to decrypt or parse.
    Error {
        /// Human-readable reason.
        message: String,
        /// The first bytes of the raw hex, for diagnosis.
        raw: String,
    },
}

/// Reads Zalo's `callV2TransportAesKey` (base64) from a copy of main.js.
///
/// # Errors
///
/// Returns a string when the file is missing or the key is not 16 bytes.
pub fn load_call_v2_key(main_js_path: &std::path::Path) -> Result<Vec<u8>, String> {
    let text = std::fs::read_to_string(main_js_path).map_err(|e| e.to_string())?;
    let marker = "callV2TransportAesKey:\"";
    let Some(start) = text.find(marker) else {
        return Err("callV2TransportAesKey not found".to_owned());
    };
    let after = &text[start + marker.len()..];
    let Some(end) = after.find('"') else {
        return Err("unterminated callV2TransportAesKey".to_owned());
    };
    let b64 = &after[..end];
    let key = base64_decode(b64).map_err(|e| e.to_string())?;
    if key.len() != 16 {
        return Err(format!("unexpected key length {}", key.len()));
    }
    Ok(key)
}

/// Encrypts a JSON frame into lowercase hex.
///
/// # Errors
///
/// Returns [`CryptoError`] when serialization or encryption fails.
pub fn call_v2_encrypt(json: &Value, key: &[u8]) -> Result<String, CryptoError> {
    let plaintext = serde_json::to_vec(json)?;
    let ciphertext = aes_cbc_encrypt(key, &plaintext)?;
    Ok(hex::encode(ciphertext))
}

/// Decrypts a hex frame back to UTF-8 text.
///
/// # Errors
///
/// Returns a string when the hex or padding is invalid.
pub fn call_v2_decrypt(hex: &str, key: &[u8]) -> Result<String, String> {
    let bytes = hex::decode(hex).map_err(|e| e.to_string())?;
    let plaintext = aes_cbc_decrypt(key, &bytes).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&plaintext).into_owned())
}

/// Incremental parser for one direction of one socket.
pub struct CallV2FrameParser {
    key: Vec<u8>,
    buf: String,
    need_token: bool,
    chunks: BTreeMap<u64, Vec<Option<String>>>,
}

impl CallV2FrameParser {
    /// Builds a parser. `expect_token` mirrors the JS `FrameParser` option.
    pub fn new(key: Vec<u8>, expect_token: bool) -> Self {
        Self {
            key,
            buf: String::new(),
            need_token: expect_token,
            chunks: BTreeMap::new(),
        }
    }

    /// Feeds a raw TCP chunk and delivers parsed frames to `on_frame`.
    pub fn push(&mut self, chunk: &[u8], on_frame: &mut dyn FnMut(CallV2Frame)) {
        for &byte in chunk {
            self.buf.push(byte as char); // latin1, matching the JS transport
        }
        if self.need_token {
            if let Some(nl) = self.buf.find('\n') {
                let first: String = self.buf.chars().take(nl).collect();
                if !looks_like_hex_frame(&self.buf) {
                    on_frame(CallV2Frame::Token(first));
                }
                self.buf = self.buf.chars().skip(nl + 1).collect();
                self.need_token = false;
            } else {
                return;
            }
        }
        while let Some(i) = self.buf.find('$') {
            let raw: String = self.buf.chars().take(i).collect();
            self.buf = self.buf.chars().skip(i + 1).collect();
            let raw = raw.trim().to_string();
            if raw.is_empty() {
                continue;
            }
            let Some(raw) = self.reassemble(&raw) else {
                continue; // chunk not complete yet
            };
            match call_v2_decrypt(&raw, &self.key) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(json) => on_frame(CallV2Frame::Json(json)),
                    Err(_) => on_frame(CallV2Frame::Text(text)),
                },
                Err(message) => on_frame(CallV2Frame::Error {
                    message,
                    raw: raw.chars().take(80).collect(),
                }),
            }
        }
    }

    fn reassemble(&mut self, raw: &str) -> Option<String> {
        let Some((part, id, total, index)) = parse_chunk(raw) else {
            return Some(raw.to_string()); // not a split frame
        };
        let entry = self.chunks.entry(id).or_insert_with(|| vec![None; total]);
        if index < total {
            entry[index] = Some(part);
        }
        if entry.iter().any(Option::is_none) {
            return None;
        }
        let joined: String = entry.iter().filter_map(Option::as_deref).collect();
        self.chunks.remove(&id);
        Some(joined)
    }
}

fn parse_chunk(raw: &str) -> Option<(String, u64, usize, usize)> {
    let parts: Vec<&str> = raw.split('#').collect();
    if parts.len() != 5 || !parts[4].is_empty() {
        return None;
    }
    let hex = parts[0];
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let id = parts[1].parse::<u64>().ok()?;
    let total = parts[2].parse::<usize>().ok()?;
    let index = parts[3].parse::<usize>().ok()?;
    Some((hex.to_string(), id, total, index))
}

fn looks_like_hex_frame(buf: &str) -> bool {
    let trimmed = buf.trim_start();
    trimmed.starts_with(|c: char| c.is_ascii_hexdigit() || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key() -> Vec<u8> {
        (0..16).collect()
    }

    #[test]
    fn encrypt_and_decrypt_round_trip() {
        let payload = json!({"type": "update", "data": 1});
        let ciphertext = call_v2_encrypt(&payload, &key()).expect("encrypt");
        let text = call_v2_decrypt(&ciphertext, &key()).expect("decrypt");
        let parsed: Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(parsed, payload);
    }

    #[test]
    fn loads_the_key_from_a_main_js() {
        let dir = std::env::temp_dir().join(format!("zcall-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("main.js");
        let b64 = crate::base64_encode(&(0..16).collect::<Vec<u8>>());
        std::fs::write(&path, format!("x callV2TransportAesKey:\"{b64}\" y")).expect("write");
        let key = load_call_v2_key(&path).expect("load key");
        assert_eq!(key, (0..16).collect::<Vec<u8>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_token_and_single_frame() {
        let mut parser = CallV2FrameParser::new(key(), true);
        let hex = call_v2_encrypt(&json!({"a": 1}), &key()).expect("encrypt");
        let mut frames = Vec::new();
        parser.push(format!("mytoken\n{hex}$").as_bytes(), &mut |f| {
            frames.push(f)
        });
        assert!(matches!(frames[0], CallV2Frame::Token(ref t) if t == "mytoken"));
        assert_eq!(frames[1], CallV2Frame::Json(json!({"a": 1})));
    }

    #[test]
    fn reassembles_a_split_frame() {
        let mut parser = CallV2FrameParser::new(key(), false);
        let value = json!({"data": "x".repeat(2000)});
        let hex = call_v2_encrypt(&value, &key()).expect("encrypt");
        let half = hex.len() / 2;
        let mut frames = Vec::new();
        parser.push(format!("{}#123#2#0#$", &hex[..half]).as_bytes(), &mut |f| {
            frames.push(f)
        });
        assert!(frames.is_empty());
        parser.push(format!("{}#123#2#1#$", &hex[half..]).as_bytes(), &mut |f| {
            frames.push(f)
        });
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], CallV2Frame::Json(value));
    }
}
