//! Non-blocking dual-channel (stereo) Ogg Opus call recorder.
//!
//! Records local microphone PCM on the **Left** channel and remote/group mixed
//! PCM on the **Right** channel into a standard RFC 7845 `.ogg` (Ogg Opus)
//! file on a dedicated background writer thread (`zcall-rec`), so disk I/O and
//! encoding never block the real-time capture or playout threads.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zcall_core::audio::{OpusDecoder, OpusStereoDecoder, OpusStereoEncoder, FRAME_SAMPLES, SAMPLE_RATE};

/// Target stereo Opus bitrate (48 kbps VBR ≈ ~21 MB / hour of crisp voice).
const RECORDING_BITRATE_BPS: i32 = 48_000;
/// 20 ms frame duration in the RFC 7845 48 kHz Ogg Opus granule clock.
const GRANULE_PER_FRAME: u64 = 960;
/// Flush an Ogg page every 25 frames (500 ms) so crash loss is at most 0.5 s.
const PACKETS_PER_PAGE: usize = 25;
/// Maximum queued frames per channel before dropping oldest to stay in sync.
const MAX_CHANNEL_QUEUE: usize = 25;
/// Maximum drift allowed on one channel before pairing with silence on the other.
const MAX_ONE_SIDED_LEAD: usize = 2;

/// Summary returned when a call recording stops cleanly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordingSummary {
    /// Destination `.ogg` file path.
    pub path: PathBuf,
    /// Total recorded duration in whole seconds.
    pub duration_secs: u64,
    /// Total encoded frames (20 ms each).
    pub frames: u64,
    /// Final file size in bytes (best effort).
    pub bytes: u64,
}

enum RecMsg {
    Near(Vec<i16>),
    Far(Vec<i16>),
    Stop,
}

/// Active call recorder handle. Cheap to tap from real-time audio threads.
pub struct CallRecorder {
    tx: SyncSender<RecMsg>,
    path: PathBuf,
    summary: Arc<Mutex<Option<RecordingSummary>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl CallRecorder {
    /// Opens `path` and starts the background `zcall-rec` Ogg Opus writer thread.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the parent directory/file cannot be created or
    /// libopus fails to initialize a stereo encoder.
    pub fn start(path: PathBuf, title: &str) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File::create(&path)?;
        let mut encoder = OpusStereoEncoder::new(RECORDING_BITRATE_BPS)
            .map_err(|err| io::Error::other(format!("opus stereo encoder: {err}")))?;
        let pre_skip = encoder.pre_skip_48k();
        let serial = stream_serial(&path);
        let mut muxer = OggOpusMuxer::new(BufWriter::new(file), serial, pre_skip, title)?;

        let (tx, rx) = mpsc::sync_channel::<RecMsg>(128);
        let summary = Arc::new(Mutex::new(None));
        let summary_t = Arc::clone(&summary);
        let path_t = path.clone();

        let handle = std::thread::Builder::new()
            .name("zcall-rec".into())
            .spawn(move || {
                let stats = run_recorder_loop(rx, &mut encoder, &mut muxer, &path_t);
                if let Ok(mut slot) = summary_t.lock() {
                    *slot = Some(stats);
                }
            })?;

        Ok(Self {
            tx,
            path,
            summary,
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Destination file path of this recording.
    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Non-blocking tap for a 20 ms local microphone frame (Left channel).
    pub fn tap_near(&self, pcm: &[i16]) {
        if pcm.len() >= FRAME_SAMPLES {
            let _ = self.tx.try_send(RecMsg::Near(pcm[..FRAME_SAMPLES].to_vec()));
        }
    }

    /// Non-blocking tap for a 20 ms remote/group playout frame (Right channel).
    pub fn tap_far(&self, pcm: &[i16]) {
        if pcm.len() >= FRAME_SAMPLES {
            let _ = self.tx.try_send(RecMsg::Far(pcm[..FRAME_SAMPLES].to_vec()));
        }
    }

    /// Flushes remaining frames, writes the Ogg EOS page, and joins `zcall-rec`.
    pub fn finish(&self) -> Option<RecordingSummary> {
        let _ = self.tx.send(RecMsg::Stop);
        if let Ok(mut guard) = self.handle.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
        self.summary
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
    }
}

impl Drop for CallRecorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn run_recorder_loop(
    rx: mpsc::Receiver<RecMsg>,
    encoder: &mut OpusStereoEncoder,
    muxer: &mut OggOpusMuxer<BufWriter<File>>,
    path: &PathBuf,
) -> RecordingSummary {
    let mut near_q: VecDeque<Vec<i16>> = VecDeque::new();
    let mut far_q: VecDeque<Vec<i16>> = VecDeque::new();
    let silence = vec![0_i16; FRAME_SAMPLES];
    let mut interleaved = vec![0_i16; FRAME_SAMPLES * 2];
    let mut frames = 0_u64;

    let mut encode_pair = |near: &[i16], far: &[i16], frames: &mut u64| {
        interleave_stereo(near, far, &mut interleaved);
        if let Ok(packet) = encoder.encode_frame(&interleaved) {
            if muxer.push_packet(&packet).is_ok() {
                *frames = frames.saturating_add(1);
            }
        }
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(RecMsg::Near(frame)) => {
                if near_q.len() >= MAX_CHANNEL_QUEUE {
                    near_q.pop_front();
                }
                near_q.push_back(frame);
            }
            Ok(RecMsg::Far(frame)) => {
                if far_q.len() >= MAX_CHANNEL_QUEUE {
                    far_q.pop_front();
                }
                far_q.push_back(frame);
            }
            Ok(RecMsg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        // Drain synchronized pairs whenever both channels have data.
        while !near_q.is_empty() && !far_q.is_empty() {
            let n = near_q.pop_front().expect("checked non-empty");
            let f = far_q.pop_front().expect("checked non-empty");
            encode_pair(&n, &f, &mut frames);
        }
        // When only one side is actively producing frames (e.g. ringing or muted
        // far-end), pair with silence once lead exceeds 40 ms so time stays true.
        while near_q.len() > MAX_ONE_SIDED_LEAD && far_q.is_empty() {
            let n = near_q.pop_front().expect("checked non-empty");
            encode_pair(&n, &silence, &mut frames);
        }
        while far_q.len() > MAX_ONE_SIDED_LEAD && near_q.is_empty() {
            let f = far_q.pop_front().expect("checked non-empty");
            encode_pair(&silence, &f, &mut frames);
        }
    }

    // Drain any remaining buffered frames before finalizing EOS.
    while !near_q.is_empty() || !far_q.is_empty() {
        let n = near_q.pop_front();
        let f = far_q.pop_front();
        encode_pair(
            n.as_deref().unwrap_or(&silence),
            f.as_deref().unwrap_or(&silence),
            &mut frames,
        );
    }
    if frames == 0 {
        // Ensure even an instant start/stop yields a valid playable Ogg Opus stream.
        encode_pair(&silence, &silence, &mut frames);
    }
    let _ = muxer.finish();
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    RecordingSummary {
        path: path.clone(),
        duration_secs: (frames * 20) / 1000,
        frames,
        bytes,
    }
}

fn interleave_stereo(left: &[i16], right: &[i16], out: &mut [i16]) {
    for i in 0..FRAME_SAMPLES {
        out[i * 2] = left[i];
        out[i * 2 + 1] = right[i];
    }
}

fn stream_serial(path: &PathBuf) -> u32 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5a41_4c4f);
    let mut hash = (nanos ^ (nanos >> 32)) as u32;
    for b in path.to_string_lossy().as_bytes() {
        hash = hash.wrapping_mul(16_777_619) ^ u32::from(*b);
    }
    if hash == 0 {
        0x5a41_4c4f
    } else {
        hash
    }
}

/// Minimal RFC 3533 + RFC 7845 Ogg Opus stream muxer in pure safe Rust.
struct OggOpusMuxer<W: Write> {
    writer: W,
    serial: u32,
    page_seq: u32,
    granule_pos: u64,
    pending_segments: Vec<u8>,
    pending_payload: Vec<u8>,
    pending_packets: usize,
    finished: bool,
}

impl<W: Write> OggOpusMuxer<W> {
    fn new(mut writer: W, serial: u32, pre_skip: u16, title: &str) -> io::Result<Self> {
        let head = build_opus_head(2, pre_skip, SAMPLE_RATE);
        write_ogg_page(&mut writer, 0x02, 0, serial, 0, &[&head])?;
        let tags = build_opus_tags(title);
        write_ogg_page(&mut writer, 0x00, 0, serial, 1, &[&tags])?;
        writer.flush()?;
        Ok(Self {
            writer,
            serial,
            page_seq: 2,
            granule_pos: 0,
            pending_segments: Vec::with_capacity(64),
            pending_payload: Vec::with_capacity(4096),
            pending_packets: 0,
            finished: false,
        })
    }

    fn push_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        let seg_count = packet.len() / 255 + 1;
        if !self.pending_segments.is_empty()
            && (self.pending_segments.len() + seg_count > 255
                || self.pending_packets >= PACKETS_PER_PAGE)
        {
            self.flush_page(false)?;
        }
        let mut rem = packet.len();
        while rem >= 255 {
            self.pending_segments.push(255);
            rem -= 255;
        }
        self.pending_segments.push(rem as u8);
        self.pending_payload.extend_from_slice(packet);
        self.pending_packets += 1;
        self.granule_pos = self.granule_pos.saturating_add(GRANULE_PER_FRAME);

        if self.pending_packets >= PACKETS_PER_PAGE {
            self.flush_page(false)?;
        }
        Ok(())
    }

    fn flush_page(&mut self, eos: bool) -> io::Result<()> {
        if self.pending_segments.is_empty() && !eos {
            return Ok(());
        }
        let header_type = if eos { 0x04 } else { 0x00 };
        write_raw_ogg_page(
            &mut self.writer,
            header_type,
            self.granule_pos,
            self.serial,
            self.page_seq,
            &self.pending_segments,
            &self.pending_payload,
        )?;
        self.page_seq = self.page_seq.wrapping_add(1);
        self.pending_segments.clear();
        self.pending_payload.clear();
        self.pending_packets = 0;
        self.writer.flush()
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.flush_page(true)
    }
}

fn build_opus_head(channels: u8, pre_skip: u16, input_sample_rate: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(19);
    buf.extend_from_slice(b"OpusHead");
    buf.push(1); // Version
    buf.push(channels); // Channel count (2 = stereo)
    buf.extend_from_slice(&pre_skip.to_le_bytes());
    buf.extend_from_slice(&input_sample_rate.to_le_bytes());
    buf.extend_from_slice(&0_i16.to_le_bytes()); // Output gain (0 dB)
    buf.push(0); // Channel mapping family 0 (L, R)
    buf
}

fn build_opus_tags(title: &str) -> Vec<u8> {
    let vendor = b"zalo-linux-rust";
    let clean_title = if title.trim().is_empty() {
        "Zalo Call Recording"
    } else {
        title.trim()
    };
    let comments = [
        format!("TITLE={clean_title}"),
        "ENCODER=zalo-linux-rust".to_owned(),
        "CHANNEL_LEFT=Local".to_owned(),
        "CHANNEL_RIGHT=Remote".to_owned(),
    ];
    let mut buf = Vec::with_capacity(128);
    buf.extend_from_slice(b"OpusTags");
    buf.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    buf.extend_from_slice(vendor);
    buf.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in &comments {
        let bytes = c.as_bytes();
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
    }
    buf
}

fn write_ogg_page<W: Write>(
    writer: &mut W,
    header_type: u8,
    granule_pos: u64,
    serial: u32,
    page_seq: u32,
    packets: &[&[u8]],
) -> io::Result<()> {
    let mut segments = Vec::new();
    let mut payload = Vec::new();
    for pkt in packets {
        let mut rem = pkt.len();
        while rem >= 255 {
            segments.push(255);
            rem -= 255;
        }
        segments.push(rem as u8);
        payload.extend_from_slice(pkt);
    }
    write_raw_ogg_page(
        writer,
        header_type,
        granule_pos,
        serial,
        page_seq,
        &segments,
        &payload,
    )
}

fn write_raw_ogg_page<W: Write>(
    writer: &mut W,
    header_type: u8,
    granule_pos: u64,
    serial: u32,
    page_seq: u32,
    segments: &[u8],
    payload: &[u8],
) -> io::Result<()> {
    let mut page = Vec::with_capacity(27 + segments.len() + payload.len());
    page.extend_from_slice(b"OggS");
    page.push(0); // stream_structure_version
    page.push(header_type);
    page.extend_from_slice(&granule_pos.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&page_seq.to_le_bytes());
    page.extend_from_slice(&0_u32.to_le_bytes()); // CRC placeholder (offset 22..26)
    page.push(segments.len() as u8);
    page.extend_from_slice(segments);
    page.extend_from_slice(payload);

    let crc = ogg_crc32(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    writer.write_all(&page)
}

/// Standard RFC 3533 Ogg CRC-32 (polynomial 0x04C11DB7, MSB-first, no final XOR).
fn ogg_crc32(data: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            if (crc & 0x8000_0000) != 0 {
                crc = (crc << 1) ^ 0x04c1_1db7;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Fast O(1) duration estimate for an Ogg Opus file by scanning the tail for
/// the last `OggS` page granule position (48 kHz clock per RFC 7845).
pub fn ogg_opus_duration_secs(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < 27 {
        return None;
    }
    let tail_len = len.min(65_536);
    file.seek(SeekFrom::Start(len - tail_len)).ok()?;
    let mut buf = vec![0_u8; tail_len as usize];
    file.read_exact(&mut buf).ok()?;
    let mut i = buf.len().saturating_sub(27);
    loop {
        if &buf[i..i + 4] == b"OggS" && buf[i + 4] == 0 {
            let granule = u64::from_le_bytes(buf[i + 6..i + 14].try_into().ok()?);
            if granule != u64::MAX {
                return Some(granule / 48_000);
            }
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    None
}

/// Decodes and plays a mono or stereo Ogg Opus recording directly through
/// CoreAudio until EOF or `stop` becomes `true`.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read or audio output cannot open.
pub fn play_ogg_opus(path: &Path, stop: &AtomicBool) -> io::Result<()> {
    let bytes = std::fs::read(path)?;
    let mut channels: u8 = 2;
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut pos = 0_usize;
    let mut cur_pkt = Vec::new();
    while pos + 27 <= bytes.len() {
        if &bytes[pos..pos + 4] != b"OggS" {
            pos += 1;
            continue;
        }
        let nsegs = usize::from(bytes[pos + 26]);
        if pos + 27 + nsegs > bytes.len() {
            break;
        }
        let segs = &bytes[pos + 27..pos + 27 + nsegs];
        let payload_len: usize = segs.iter().map(|&s| usize::from(s)).sum();
        let payload_start = pos + 27 + nsegs;
        if payload_start + payload_len > bytes.len() {
            break;
        }
        let payload = &bytes[payload_start..payload_start + payload_len];
        let mut p_off = 0_usize;
        for &seg in segs {
            let slen = usize::from(seg);
            cur_pkt.extend_from_slice(&payload[p_off..p_off + slen]);
            p_off += slen;
            if seg < 255 {
                if cur_pkt.starts_with(b"OpusHead") && cur_pkt.len() >= 10 {
                    channels = cur_pkt[9].clamp(1, 2);
                } else if !cur_pkt.starts_with(b"OpusTags") && !cur_pkt.is_empty() {
                    packets.push(std::mem::take(&mut cur_pkt));
                }
                cur_pkt.clear();
            }
        }
        pos = payload_start + payload_len;
    }

    let speaker = crate::host::CardSpeaker::open(None)?;

    if channels == 2 {
        let mut dec = OpusStereoDecoder::new()
            .map_err(|err| io::Error::other(format!("opus stereo decode: {err}")))?;
        let mut mono = Vec::with_capacity(FRAME_SAMPLES);
        for pkt in &packets {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(pcm) = dec.decode_frame(pkt) else {
                continue;
            };
            mono.clear();
            for lr in pcm.chunks_exact(2) {
                let mix = ((i32::from(lr[0]) + i32::from(lr[1])) / 2)
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                mono.push(mix);
            }
            if speaker.write(&mono).is_err() {
                break;
            }
        }
    } else {
        let mut dec = OpusDecoder::new()
            .map_err(|err| io::Error::other(format!("opus mono decode: {err}")))?;
        for pkt in &packets {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let Ok(pcm) = dec.decode_frame(pkt) else {
                continue;
            };
            if speaker.write(&pcm).is_err() {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcall_core::audio::OpusStereoDecoder;

    fn sine_frame(freq_hz: f64, frame_idx: usize, amp: f64) -> Vec<i16> {
        let base = frame_idx * FRAME_SAMPLES;
        (0..FRAME_SAMPLES)
            .map(|i| {
                let t = (base + i) as f64 / SAMPLE_RATE as f64;
                (amp * (2.0 * std::f64::consts::PI * freq_hz * t).sin()) as i16
            })
            .collect()
    }

    #[test]
    fn records_valid_stereo_ogg_opus_with_separated_left_and_right_channels() {
        let dir = std::env::temp_dir().join("zcall-recorder-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!(
            "stereo_{}.ogg",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let rec = CallRecorder::start(path.clone(), "Test Call").expect("start recorder");
        // First 15 frames: Left active (440 Hz), Right silent.
        // Next 15 frames: Left silent, Right active (880 Hz).
        let silence = vec![0_i16; FRAME_SAMPLES];
        for i in 0..15 {
            rec.tap_near(&sine_frame(440.0, i, 12_000.0));
            rec.tap_far(&silence);
        }
        for i in 15..30 {
            rec.tap_near(&silence);
            rec.tap_far(&sine_frame(880.0, i, 12_000.0));
        }
        let summary = rec.finish().expect("summary");
        assert_eq!(summary.frames, 30);
        assert!(summary.bytes > 200);

        // Parse Ogg pages, verify every page's CRC-32, headers, EOS, and decode audio.
        let bytes = std::fs::read(&path).expect("read ogg");
        let _ = std::fs::remove_file(&path);

        let mut pos = 0;
        let mut page_count = 0;
        let mut saw_bos = false;
        let mut saw_eos = false;
        let mut audio_packets: Vec<Vec<u8>> = Vec::new();
        while pos < bytes.len() {
            assert_eq!(&bytes[pos..pos + 4], b"OggS");
            let header_type = bytes[pos + 5];
            let nsegs = usize::from(bytes[pos + 26]);
            let seg_table = &bytes[pos + 27..pos + 27 + nsegs];
            let payload_len: usize = seg_table.iter().map(|&s| usize::from(s)).sum();
            let page_end = pos + 27 + nsegs + payload_len;

            // Verify CRC-32
            let stored_crc = u32::from_le_bytes(bytes[pos + 22..pos + 26].try_into().unwrap());
            let mut copy = bytes[pos..page_end].to_vec();
            copy[22..26].copy_from_slice(&[0, 0, 0, 0]);
            assert_eq!(ogg_crc32(&copy), stored_crc, "page {page_count} CRC mismatch");

            if (header_type & 0x02) != 0 {
                saw_bos = true;
            }
            if (header_type & 0x04) != 0 {
                saw_eos = true;
            }

            let payload = &bytes[pos + 27 + nsegs..page_end];
            let mut p_off = 0;
            let mut pkt = Vec::new();
            for &seg in seg_table {
                let len = usize::from(seg);
                pkt.extend_from_slice(&payload[p_off..p_off + len]);
                p_off += len;
                if seg < 255 {
                    if page_count == 0 {
                        assert_eq!(&pkt[..8], b"OpusHead");
                        assert_eq!(pkt[9], 2, "must be 2-channel stereo");
                    } else if page_count == 1 {
                        assert_eq!(&pkt[..8], b"OpusTags");
                    } else if !pkt.is_empty() {
                        audio_packets.push(std::mem::take(&mut pkt));
                    }
                    pkt.clear();
                }
            }
            page_count += 1;
            pos = page_end;
        }
        assert!(saw_bos, "missing BOS page");
        assert!(saw_eos, "missing EOS page");
        assert_eq!(audio_packets.len(), 30);

        // Decode stereo packets and confirm Left-only in first half, Right-only in second half.
        let mut dec = OpusStereoDecoder::new().expect("stereo decoder");
        let mut first_half_l = 0_u64;
        let mut first_half_r = 0_u64;
        let mut second_half_l = 0_u64;
        let mut second_half_r = 0_u64;
        for (idx, pkt) in audio_packets.iter().enumerate() {
            let pcm = dec.decode_frame(pkt).expect("decode stereo");
            assert_eq!(pcm.len(), FRAME_SAMPLES * 2);
            for s in pcm.chunks_exact(2) {
                let l = i64::from(s[0]).unsigned_abs();
                let r = i64::from(s[1]).unsigned_abs();
                if (3..14).contains(&idx) {
                    first_half_l += l;
                    first_half_r += r;
                } else if (18..29).contains(&idx) {
                    second_half_l += l;
                    second_half_r += r;
                }
            }
        }
        assert!(
            first_half_l > first_half_r * 10,
            "first half should be Left-only: L={first_half_l} R={first_half_r}"
        );
        assert!(
            second_half_r > second_half_l * 10,
            "second half should be Right-only: L={second_half_l} R={second_half_r}"
        );
    }
}
