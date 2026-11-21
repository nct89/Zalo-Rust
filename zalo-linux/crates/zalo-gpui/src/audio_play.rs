//! In-process audio decoding (`symphonia`) and playback (`zcall-audio` `CardSpeaker`).
//!
//! Replaces all `ffplay` subprocesses for call ringtone/ringback/end tones,
//! notification chimes, settings sound previews, voice messages, and video
//! audio tracks — works cleanly on PipeWire/PulseAudio without `ffmpeg` installed.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// Handle to an active in-process audio playback thread.
pub(crate) struct AudioHandle {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl AudioHandle {
    /// Signals playback to stop immediately (within one ~40 ms PCM chunk).
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// True once the playback thread has finished or stopped.
    pub(crate) fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }
}

/// Decoded audio track metadata and interleaved `i16` PCM samples (used for unit tests
/// and short pre-decoded effects).
#[cfg(test)]
pub(crate) struct DecodedAudio {
    pub(crate) sample_rate: u32,
    pub(crate) channels: u8,
    pub(crate) samples: Vec<i16>,
}

/// Probes and decodes an audio file (MP3, AAC, M4A, MP4 audio, OGG/Vorbis, WAV)
/// on a background thread and streams PCM directly to `CardSpeaker` (`libpulse-simple`).
pub(crate) fn play_file(
    path: PathBuf,
    device: Option<String>,
    looped: bool,
    max_duration: Option<Duration>,
) -> Result<AudioHandle, String> {
    if !path.is_file() {
        return Err(format!("không tìm thấy tệp âm thanh: {}", path.display()));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let handle = AudioHandle {
        stop: Arc::clone(&stop),
        finished: Arc::clone(&finished),
    };
    if cfg!(test) {
        finished.store(true, Ordering::Relaxed);
        return Ok(handle);
    }
    std::thread::Builder::new()
        .name("zalo-audio-play".into())
        .spawn(move || {
            let started = Instant::now();
            let mut speaker: Option<(zcall_audio::capture::CardSpeaker, u32, u8)> = None;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if max_duration.is_some_and(|cap| started.elapsed() >= cap) {
                    break;
                }
                if play_once(
                    &path,
                    device.as_deref(),
                    &stop,
                    started,
                    max_duration,
                    &mut speaker,
                )
                .is_err()
                {
                    break;
                }
                if !looped {
                    break;
                }
            }
            finished.store(true, Ordering::Relaxed);
        })
        .map_err(|e| format!("không tạo được luồng phát âm thanh: {e}"))?;
    Ok(handle)
}

/// Fires a detached one-shot playback (notification chime, preview, speaker test).
pub(crate) fn play_detached(
    path: PathBuf,
    device: Option<String>,
    max_duration: Option<Duration>,
) {
    if cfg!(test) {
        return;
    }
    let _ = play_file(path, device, false, max_duration);
}

fn play_once(
    path: &Path,
    device: Option<&str>,
    stop: &AtomicBool,
    started: Instant,
    max_duration: Option<Duration>,
    speaker_slot: &mut Option<(zcall_audio::capture::CardSpeaker, u32, u8)>,
) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        // Freedesktop `.oga` chimes are Ogg Vorbis/Opus containers.
        if ext.eq_ignore_ascii_case("oga") {
            hint.with_extension("ogg");
        } else {
            hint.with_extension(ext);
        }
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "không có track âm thanh".to_owned())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;

    let mut sample_buf: Option<SampleBuffer<i16>> = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if max_duration.is_some_and(|cap| started.elapsed() >= cap) {
            return Ok(());
        }
        let packet = match format.next_packet() {
            Ok(pkt) => pkt,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(_) => return Ok(()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(SymphoniaError::IoError(_)) => return Ok(()),
            Err(e) => return Err(format!("decode: {e}")),
        };
        let spec = *decoded.spec();
        let rate = spec.rate;
        let channels = u8::try_from(spec.channels.count()).unwrap_or(2).clamp(1, 2);
        let need_open = match speaker_slot {
            Some((_, r, c)) => *r != rate || *c != channels,
            None => true,
        };
        if need_open {
            let spk = zcall_audio::capture::CardSpeaker::open_spec(device, rate, channels, "Media")
                .map_err(|e| format!("speaker: {e}"))?;
            *speaker_slot = Some((spk, rate, channels));
        }
        let buf = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<i16>::new(decoded.capacity() as u64, spec)
        });
        buf.copy_interleaved_ref(decoded);
        let samples = buf.samples();
        // Write in ~40 ms chunks so `stop` responds promptly.
        let chunk_samples = ((rate as usize) * usize::from(channels) / 25).max(512);
        if let Some((spk, _, _)) = speaker_slot.as_ref() {
            for slice in samples.chunks(chunk_samples) {
                if stop.load(Ordering::Relaxed)
                    || max_duration.is_some_and(|cap| started.elapsed() >= cap)
                {
                    return Ok(());
                }
                spk.write(slice).map_err(|e| format!("write: {e}"))?;
            }
        }
    }
}

/// Decodes an entire audio file into PCM samples (for unit test verification).
#[cfg(test)]
pub(crate) fn decode_file_pcm(path: &Path) -> Result<DecodedAudio, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "no audio track".to_owned())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("codec: {e}"))?;

    let mut out = Vec::new();
    let mut rate = 0;
    let mut channels = 0;
    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        if let Ok(decoded) = decoder.decode(&packet) {
            let spec = *decoded.spec();
            rate = spec.rate;
            channels = spec.channels.count() as u8;
            let mut sb = SampleBuffer::<i16>::new(decoded.capacity() as u64, spec);
            sb.copy_interleaved_ref(decoded);
            out.extend_from_slice(sb.samples());
        }
    }
    if out.is_empty() {
        return Err("no samples decoded".to_owned());
    }
    Ok(DecodedAudio {
        sample_rate: rate,
        channels,
        samples: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_bundled_call_sounds_in_process_without_ffplay() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../assets/sounds");
        for file in ["zalo_ringtone.mp3", "zalo_ringback.mp3", "endcall.mp3"] {
            let path = root.join(file);
            let decoded = decode_file_pcm(&path).expect(file);
            assert!(decoded.sample_rate >= 8_000, "{file}");
            assert!((1..=2).contains(&decoded.channels), "{file}");
            assert!(decoded.samples.len() > 1_000, "{file}");
        }
    }
}
