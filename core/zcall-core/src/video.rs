//! Video: RTP payloads -> whole H.264 access units (Annex-B), and the
//! reverse (one access unit -> RTP payloads). Ported directly from the
//! reference call implementation (`video.js`).
//!
//! The phone sends PT 98 for key frames and PT 97 for delta frames on the same
//! RTP sequence; each payload is either a whole Annex-B chunk or a fragment
//! with a 2-byte header: `1c 80` (start), `1c 00` (middle), `1c 40` (end).
//! The marker bit terminates a frame.

use std::collections::BTreeMap;

/// Payload type for key frames.
pub const KEY_PT: u8 = 98;
/// Payload type for delta frames.
pub const DELTA_PT: u8 = 97;
/// Both payload types accepted by the assembler in 1-1 calls.
pub const VIDEO_PTS: [u8; 2] = [DELTA_PT, KEY_PT];
/// Maximum number of complete frames held while waiting for an earlier missing frame.
pub const REORDER_FRAMES: usize = 3;
/// Maximum wait time for a missing frame, in ms.
pub const REORDER_MS: u64 = 200;
/// Maximum number of incomplete frames retained in the pending map.
pub const MAX_PENDING: usize = 30;
/// Maximum fragment size when packetizing an access unit.
pub const MAX_CHUNK: usize = 1068;
/// SVC NAL unit types stripped so the base layer decodes as plain AVC.
pub const SVC_NALS: [u8; 3] = [14, 15, 20];
/// 4-byte Annex-B start code.
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// A completely reassembled access unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoFrame {
    /// Whether this frame is a key frame (IDR/SPS).
    pub key: bool,
    /// Annex-B byte stream of the access unit.
    pub data: Vec<u8>,
    /// 90 kHz RTP timestamp.
    pub ts: u32,
    /// SSRC of the sender. 0 when the packet had none. A group call has one
    /// stream per member; 1-1 is a single SSRC.
    pub src: u32,
}

/// Diagnostic counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoStats {
    /// Number of frames emitted.
    pub frames: u64,
    /// Number of frames dropped.
    pub dropped: u64,
    /// Number of key frames emitted.
    pub keys: u64,
    /// Number of times a frame was emitted across a sequence gap.
    pub gaps: u64,
}

#[derive(Default)]
struct Pending {
    parts: BTreeMap<u64, Vec<u8>>,
    key: bool,
    end: Option<u64>,
}

struct Ready {
    key: bool,
    data: Vec<u8>,
    ts: u32,
    last: u64,
    since: u64,
}

/// Reassembles RTP payloads into H.264 access units.
pub struct VideoAssembler {
    group: bool,
    pending: BTreeMap<u32, Pending>,
    ready: BTreeMap<u64, Ready>,
    last_idx: i64,
    started: bool,
    need_key: bool,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    stats: VideoStats,
}

impl VideoAssembler {
    /// Creates an assembler. Setting `group` relaxes PT filtering and enables SVC/SPS handling.
    pub fn new(group: bool) -> Self {
        Self {
            group,
            pending: BTreeMap::new(),
            ready: BTreeMap::new(),
            last_idx: -1,
            started: false,
            need_key: false,
            sps: None,
            pps: None,
            stats: VideoStats::default(),
        }
    }

    /// Returns assembly statistics.
    pub fn stats(&self) -> &VideoStats {
        &self.stats
    }

    /// Feeds one RTP packet and returns any frames ready to emit.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        ts: u32,
        pt: u8,
        marker: bool,
        payload: &[u8],
        seq: u16,
        roc: u32,
        now_ms: u64,
    ) -> Vec<VideoFrame> {
        if (!self.group && !VIDEO_PTS.contains(&pt)) || payload.is_empty() {
            return Vec::new();
        }
        let idx = u64::from(roc) * 0x10000 + u64::from(seq);
        if self.last_idx >= 0 && idx <= self.last_idx as u64 {
            return Vec::new(); // late or duplicate packet (relay + P2P)
        }
        if let std::collections::btree_map::Entry::Vacant(slot) = self.pending.entry(ts) {
            slot.insert(Pending::default());
            if self.pending.len() > MAX_PENDING {
                if let Some(first) = self.pending.keys().next().copied() {
                    self.pending.remove(&first);
                    self.stats.dropped += 1;
                }
            }
        }
        let frame = self.pending.get_mut(&ts).expect("vừa chèn");
        frame.parts.insert(idx, payload.to_vec());
        if pt == KEY_PT {
            frame.key = true;
        }
        if marker {
            frame.end = Some(idx);
        }
        let complete = frame.end.is_some();
        if complete {
            self.complete(ts, now_ms);
        }
        self.flush(now_ms)
    }

    fn complete(&mut self, ts: u32, now_ms: u64) {
        let Some(frame) = self.pending.get(&ts) else {
            return;
        };
        let idxs: Vec<u64> = frame.parts.keys().copied().collect();
        let Some(&first_idx) = idxs.first() else {
            return;
        };
        let first = &frame.parts[&first_idx];
        if is_fragment(first) && first.get(1) != Some(&0x80) {
            return; // leading fragment still missing
        }
        for w in idxs.windows(2) {
            if w[1] != w[0] + 1 {
                return; // sequence gap
            }
        }
        let end = frame.end.expect("đã kiểm tra");
        if *idxs.last().expect("không rỗng") != end {
            return;
        }
        let key_flag = frame.key;
        let mut data: Vec<u8> = Vec::new();
        for idx in &idxs {
            let part = &frame.parts[idx];
            if is_fragment(part) {
                data.extend_from_slice(&part[2..]);
            } else {
                data.extend_from_slice(part);
            }
        }
        self.pending.remove(&ts);

        let mut key = key_flag;
        if self.group {
            let units = nal_units(&data);
            let mut kept: Vec<Vec<u8>> = Vec::new();
            for unit in &units {
                let kind = unit[0] & 0x1f;
                if !SVC_NALS.contains(&kind) {
                    kept.push(unit.to_vec());
                }
            }
            key = units.iter().any(|u| matches!(u[0] & 0x1f, 7 | 5));
            if kept.is_empty() {
                self.stats.dropped += 1;
                return;
            }
            if kept.len() != units.len() {
                let mut rebuilt = Vec::new();
                for unit in &kept {
                    rebuilt.extend_from_slice(&START_CODE);
                    rebuilt.extend_from_slice(unit);
                }
                data = rebuilt;
            }
            for unit in &kept {
                match unit[0] & 0x1f {
                    7 => self.sps = Some(unit.clone()),
                    8 => self.pps = Some(unit.clone()),
                    _ => {}
                }
            }
            if key {
                if let Some(sps) = &self.sps {
                    let has_sps = kept.iter().any(|u| (u[0] & 0x1f) == 7);
                    let has_pps = kept.iter().any(|u| (u[0] & 0x1f) == 8);
                    if !has_sps || (!has_pps && self.pps.is_some()) {
                        let mut parts = Vec::new();
                        if !has_sps {
                            parts.extend_from_slice(&START_CODE);
                            parts.extend_from_slice(sps);
                        }
                        if !has_pps {
                            if let Some(pps) = &self.pps {
                                parts.extend_from_slice(&START_CODE);
                                parts.extend_from_slice(pps);
                            }
                        }
                        parts.extend_from_slice(&data);
                        data = parts;
                    }
                }
            }
        }
        self.ready.insert(
            first_idx,
            Ready {
                key,
                data,
                ts,
                last: end,
                since: now_ms,
            },
        );
    }

    fn flush(&mut self, now_ms: u64) -> Vec<VideoFrame> {
        let mut out = Vec::new();
        loop {
            if self.ready.is_empty() {
                return out;
            }
            let next_idx = if self.last_idx >= 0 {
                (self.last_idx as u64) + 1
            } else {
                0
            };
            if self.last_idx >= 0 && self.ready.contains_key(&next_idx) {
                out.extend(self.emit(next_idx));
                continue;
            }
            let first_idx = *self.ready.keys().next().expect("không rỗng");
            let waited = now_ms.saturating_sub(self.ready[&first_idx].since);
            if self.last_idx >= 0 && self.ready.len() <= REORDER_FRAMES && waited < REORDER_MS {
                return out;
            }
            if !self.started && !self.ready[&first_idx].key {
                self.ready.remove(&first_idx);
                self.stats.dropped += 1;
                continue;
            }
            if self.last_idx >= 0 {
                self.stats.gaps += 1;
                if self.group {
                    self.need_key = true;
                }
            }
            out.extend(self.emit(first_idx));
        }
    }

    fn emit(&mut self, idx: u64) -> Option<VideoFrame> {
        let frame = self.ready.remove(&idx)?;
        if self.group && self.need_key && !frame.key {
            self.last_idx = frame.last as i64;
            self.stats.dropped += 1;
            return None;
        }
        self.last_idx = frame.last as i64;
        if frame.key {
            self.need_key = false;
        }
        let last = self.last_idx as u64;
        let dead: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, f)| f.parts.keys().next_back().is_some_and(|m| *m <= last))
            .map(|(ts, _)| *ts)
            .collect();
        for ts in dead {
            self.pending.remove(&ts);
            self.stats.dropped += 1;
        }
        self.started = self.started || frame.key;
        self.stats.frames += 1;
        if frame.key {
            self.stats.keys += 1;
        }
        Some(VideoFrame {
            key: frame.key,
            data: frame.data,
            ts: frame.ts,
            src: 0,
        })
    }
}

/// Returns whether a payload carries the `1c xx` fragment header.
pub fn is_fragment(payload: &[u8]) -> bool {
    payload.len() >= 2 && payload[0] == 0x1c && (payload[1] & 0x3f) == 0
}

/// Splits an Annex-B buffer into individual NAL units (without start codes).
pub fn nal_units(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 2 < annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &start) in starts.iter().enumerate() {
        let mut end = if k + 1 < starts.len() {
            starts[k + 1] - 3
        } else {
            annex_b.len()
        };
        while end > start && annex_b[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            out.push(&annex_b[start..end]);
        }
    }
    out
}

/// Extracts the frame dimensions from an SPS (NAL 7) or subset SPS (NAL 15), applying cropping.
pub fn sps_size(nal: &[u8]) -> Option<(u32, u32)> {
    let mut b = Vec::new();
    for (i, &byte) in nal.iter().enumerate().skip(1) {
        if i >= 3 && byte == 3 && nal[i - 1] == 0 && nal[i - 2] == 0 {
            continue; // emulation prevention
        }
        b.push(byte);
    }
    let mut pos = 0usize;
    let total = b.len() * 8;
    let mut bit = |pos: &mut usize| -> Option<u32> {
        if *pos >= total {
            return None;
        }
        let v = (b[*pos >> 3] >> (7 - (*pos & 7))) & 1;
        *pos += 1;
        Some(u32::from(v))
    };
    fn bits(
        bit: &mut dyn FnMut(&mut usize) -> Option<u32>,
        pos: &mut usize,
        n: u32,
    ) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = v * 2 + bit(pos)?;
        }
        Some(v)
    }
    fn ue(bit: &mut dyn FnMut(&mut usize) -> Option<u32>, pos: &mut usize) -> Option<u32> {
        let mut z = 0u32;
        while bit(pos)? == 0 {
            z += 1;
            if z > 31 {
                return None;
            }
        }
        let suffix = bits(bit, pos, z)?;
        Some(suffix + ((1u32 << z) - 1))
    }
    fn se(bit: &mut dyn FnMut(&mut usize) -> Option<u32>, pos: &mut usize) -> Option<i32> {
        let k = ue(bit, pos)? as i32;
        Some(if k & 1 == 1 { (k + 1) / 2 } else { -k / 2 })
    }

    let profile = bits(&mut bit, &mut pos, 8)?;
    bits(&mut bit, &mut pos, 16)?;
    ue(&mut bit, &mut pos)?;
    let mut chroma = 1u32;
    let high = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];
    if high.contains(&profile) {
        chroma = ue(&mut bit, &mut pos)?;
        if chroma == 3 {
            bit(&mut pos)?;
        }
        ue(&mut bit, &mut pos)?;
        ue(&mut bit, &mut pos)?;
        bit(&mut pos)?;
        if bit(&mut pos)? == 1 {
            let count = if chroma == 3 { 12 } else { 8 };
            for i in 0..count {
                if bit(&mut pos)? == 0 {
                    continue;
                }
                let mut last = 8i32;
                let mut next = 8i32;
                let span = if i < 6 { 16 } else { 64 };
                for _ in 0..span {
                    if next != 0 {
                        let delta = se(&mut bit, &mut pos)?;
                        next = (last + delta + 256) % 256;
                    }
                    last = if next != 0 { next } else { last };
                }
            }
        }
    }
    ue(&mut bit, &mut pos)?;
    let poc = ue(&mut bit, &mut pos)?;
    if poc == 0 {
        ue(&mut bit, &mut pos)?;
    } else if poc == 1 {
        bit(&mut pos)?;
        se(&mut bit, &mut pos)?;
        se(&mut bit, &mut pos)?;
        let n = ue(&mut bit, &mut pos)?;
        for _ in 0..n {
            se(&mut bit, &mut pos)?;
        }
    }
    ue(&mut bit, &mut pos)?;
    bit(&mut pos)?;
    let w_mbs = ue(&mut bit, &mut pos)? + 1;
    let h_map = ue(&mut bit, &mut pos)? + 1;
    let frame_mbs_only = bit(&mut pos)?;
    if frame_mbs_only == 0 {
        bit(&mut pos)?;
    }
    bit(&mut pos)?;
    let (mut cl, mut cr, mut ct, mut cb) = (0u32, 0u32, 0u32, 0u32);
    if bit(&mut pos)? == 1 {
        cl = ue(&mut bit, &mut pos)?;
        cr = ue(&mut bit, &mut pos)?;
        ct = ue(&mut bit, &mut pos)?;
        cb = ue(&mut bit, &mut pos)?;
    }
    let cx = if chroma == 0 { 1 } else { 2 };
    let cy = (if chroma == 1 { 2 } else { 1 }) * (2 - frame_mbs_only);
    Some((
        w_mbs * 16 - cx * (cl + cr),
        (2 - frame_mbs_only) * h_map * 16 - cy * (ct + cb),
    ))
}

/// Builds the `avc1.PPCCLL` codec string from a key frame's SPS.
pub fn avc_codec_string(annex_b: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 7 < annex_b.len() {
        if annex_b[i] == 0
            && annex_b[i + 1] == 0
            && annex_b[i + 2] == 1
            && (annex_b[i + 3] & 0x1f) == 7
        {
            let p = annex_b[i + 4];
            let c = annex_b[i + 5];
            let l = annex_b[i + 6];
            return Some(format!("avc1.{p:02x}{c:02x}{l:02x}"));
        }
        i += 1;
    }
    None
}

/// Packetizes an Annex-B access unit into RTP payloads.
pub fn packetize_frame(annex_b: &[u8], max_chunk: usize) -> Vec<Vec<u8>> {
    let max_chunk = max_chunk.max(1);
    if annex_b.len() <= max_chunk {
        return vec![annex_b.to_vec()];
    }
    let mut out = Vec::new();
    let mut off = 0;
    while off < annex_b.len() {
        let end = (off + max_chunk).min(annex_b.len());
        let flag = if off == 0 {
            0x80
        } else if end == annex_b.len() {
            0x40
        } else {
            0x00
        };
        let mut chunk = Vec::with_capacity(2 + end - off);
        chunk.push(0x1c);
        chunk.push(flag);
        chunk.extend_from_slice(&annex_b[off..end]);
        out.push(chunk);
        off = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annex_b_with_two_nals() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&START_CODE);
        data.extend_from_slice(&[0x67, 0x42, 0x00, 0x1e]);
        data.extend_from_slice(&START_CODE);
        data.extend_from_slice(&[0x68, 0xce, 0x38, 0x80]);
        data
    }

    #[test]
    fn splits_nal_units() {
        let buffer = annex_b_with_two_nals();
        let units = nal_units(&buffer);
        assert_eq!(units.len(), 2);
        assert_eq!(units[0][0] & 0x1f, 7);
        assert_eq!(units[1][0] & 0x1f, 8);
    }

    #[test]
    fn packetizes_small_frame_whole() {
        let data = vec![0u8; 100];
        let chunks = packetize_frame(&data, MAX_CHUNK);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], data);
    }

    #[test]
    fn packetizes_large_frame_with_flags() {
        let data = vec![0xABu8; MAX_CHUNK * 2 + 10];
        let chunks = packetize_frame(&data, MAX_CHUNK);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0][1], 0x80);
        assert_eq!(chunks[1][1], 0x00);
        assert_eq!(chunks[2][1], 0x40);
        let total: usize = chunks.iter().map(|c| c.len() - 2).sum();
        assert_eq!(total, data.len());
    }

    #[test]
    fn codec_string_from_sps() {
        let mut data = Vec::new();
        data.extend_from_slice(&START_CODE);
        data.extend_from_slice(&[0x67, 0x64, 0x00, 0x2a, 0xac]);
        assert_eq!(avc_codec_string(&data).as_deref(), Some("avc1.64002a"));
        assert!(avc_codec_string(&[0, 0, 0, 1, 0x65]).is_none());
    }

    #[test]
    fn assembles_a_whole_key_frame() {
        let mut asm = VideoAssembler::new(false);
        let payload = vec![0u8, 0, 0, 1, 0x65, 1, 2, 3];
        let out = asm.push(1000, KEY_PT, true, &payload, 5, 0, 0);
        assert_eq!(out.len(), 1);
        assert!(out[0].key);
        assert_eq!(out[0].data, payload);
    }

    #[test]
    fn assembles_a_fragmented_frame() {
        let mut asm = VideoAssembler::new(false);
        let head = vec![0x1c, 0x80, 1, 2, 3];
        let tail = vec![0x1c, 0x40, 4, 5];
        // The first frame must be a key frame (matching video.js).
        assert!(asm.push(2000, KEY_PT, false, &head, 10, 0, 0).is_empty());
        let out = asm.push(2000, KEY_PT, true, &tail, 11, 0, 0);
        assert_eq!(out.len(), 1);
        assert!(out[0].key);
        assert_eq!(out[0].data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn drops_late_packets() {
        let mut asm = VideoAssembler::new(false);
        let payload = vec![0u8, 0, 0, 1, 0x65];
        asm.push(3000, KEY_PT, true, &payload, 20, 0, 0);
        // Same seq: treated as a late/duplicate packet.
        let out = asm.push(3001, DELTA_PT, true, &payload, 20, 0, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn waits_then_skips_a_gap() {
        let mut asm = VideoAssembler::new(false);
        let key = vec![0u8, 0, 0, 1, 0x65];
        let delta = vec![0u8, 0, 0, 1, 0x41];
        assert_eq!(asm.push(100, KEY_PT, true, &key, 1, 0, 0).len(), 1);
        // seq 2 lost: subsequent frames are buffered and not emitted yet.
        assert_eq!(asm.push(200, DELTA_PT, true, &delta, 3, 0, 0).len(), 0);
        assert_eq!(asm.push(400, DELTA_PT, true, &delta, 4, 0, 0).len(), 0);
        // Past REORDER_MS: skip the gap and emit all 3 buffered frames.
        assert_eq!(asm.push(500, DELTA_PT, true, &delta, 5, 0, 10_000).len(), 3);
        assert_eq!(asm.stats().gaps, 1);
        assert_eq!(asm.stats().frames, 4);
    }
}
