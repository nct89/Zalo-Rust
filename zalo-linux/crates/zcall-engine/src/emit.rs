//! Writes encrypted engine -> Zalo frames, splitting over 4000 hex chars.

use serde_json::Value;
use std::io::{self, Write};
use zalo_protocol::call_v2_encrypt;

/// The longest hex run Zalo's transport takes in one message.
pub const MAX_HEX: usize = 4000;

/// Encrypts and writes one frame (with `$` terminator), splitting long frames.
pub fn emit_frame(
    socket: &mut impl Write,
    frame: &Value,
    key: &[u8],
    verbose: bool,
) -> io::Result<()> {
    let hex = call_v2_encrypt(frame, key).map_err(|e| io::Error::other(e.to_string()))?;
    if verbose {
        eprintln!("-> {} hex chars", hex.len());
    }
    if hex.len() <= MAX_HEX {
        return socket.write_all(format!("{hex}$").as_bytes());
    }
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let bytes = hex.as_bytes();
    let total = bytes.len().div_ceil(MAX_HEX);
    for (index, part) in bytes.chunks(MAX_HEX).enumerate() {
        let part = std::str::from_utf8(part).expect("hex is ascii");
        socket.write_all(format!("{part}#{id}#{total}#{index}#$").as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zalo_protocol::{CallV2Frame, CallV2FrameParser};

    fn key() -> Vec<u8> {
        (0..16).collect()
    }

    #[test]
    fn emits_a_short_frame() {
        let mut out = Vec::new();
        emit_frame(&mut out, &json!({"a": 1}), &key(), false).expect("emit");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.ends_with('$'));
        // Round-trip through the parser.
        let mut parser = CallV2FrameParser::new(key(), false);
        let mut frames = Vec::new();
        parser.push(text.as_bytes(), &mut |f| frames.push(f));
        assert_eq!(frames[0], CallV2Frame::Json(json!({"a": 1})));
    }

    #[test]
    fn splits_a_long_frame_and_reassembles() {
        let mut out = Vec::new();
        let frame = json!({"data": "x".repeat(3000)});
        emit_frame(&mut out, &frame, &key(), false).expect("emit");
        let text = String::from_utf8(out).expect("utf8");
        assert!(text.contains('#'), "long frame should be split");
        let mut parser = CallV2FrameParser::new(key(), false);
        let mut frames = Vec::new();
        parser.push(text.as_bytes(), &mut |f| frames.push(f));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], CallV2Frame::Json(frame));
    }
}
