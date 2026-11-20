//! Batch files: `[u32 little-endian length][frame]…`.

use crate::Sync2Error;

/// Splits a downloaded batch file into its frames.
///
/// # Errors
///
/// Returns [`Sync2Error::Format`] when a length runs past the end of the file.
pub fn frames(file: &[u8]) -> Result<Vec<&[u8]>, Sync2Error> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < file.len() {
        let head = file
            .get(at..at + 4)
            .ok_or_else(|| Sync2Error::Format(format!("truncated frame length at {at}")))?;
        let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let start = at + 4;
        let frame = file
            .get(start..start + len)
            .ok_or_else(|| Sync2Error::Format(format!("frame at {at} needs {len} bytes")))?;
        out.push(frame);
        at = start + len;
    }
    Ok(out)
}

/// Builds a batch file from frames (the phone's side; used in tests).
pub fn join_frames<'a>(frames: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for frame in frames {
        out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        out.extend_from_slice(frame);
    }
    out
}

/// Unpacks a decrypted frame.
///
/// # Errors
///
/// Returns [`Sync2Error::Zstd`] when the frame is not zstd data.
pub fn decompress(frame: &[u8]) -> Result<Vec<u8>, Sync2Error> {
    zstd::stream::decode_all(frame).map_err(|e| Sync2Error::Zstd(e.to_string()))
}
