//! Pure helpers from `zrtc-media.js` (the parts that have no socket or
//! subprocess): RTP audio level, group video layer selection, EndCall peer
//! detection, early-audio phase, and the zrtcConfig -> audio-io args mapping.

use serde_json::Value;

/// RFC 6464 audio level of an RTP packet (one-byte extension id 2), 0..127.
pub fn rtp_audio_level(rtp: &[u8]) -> Option<u8> {
    if rtp.len() < 16 || rtp[0] & 0x10 == 0 {
        return None;
    }
    let mut o = 12 + 4 * (rtp[0] & 0x0f) as usize;
    if o + 4 > rtp.len() || u16::from_be_bytes([rtp[o], rtp[o + 1]]) != 0xbede {
        return None;
    }
    let ext_len = u16::from_be_bytes([rtp[o + 2], rtp[o + 3]]) as usize;
    let end = rtp.len().min(o + 4 + 4 * ext_len);
    o += 4;
    while o < end {
        if rtp[o] == 0 {
            o += 1;
            continue;
        }
        let id = rtp[o] >> 4;
        let len = (rtp[o] & 0x0f) as usize + 1;
        if id == 15 {
            return None;
        }
        if id == 2 && o + 1 < end {
            return Some(rtp[o + 1] & 0x7f);
        }
        o += 1 + len;
    }
    None
}

/// The layer id to ask the SFU for, from the layer table and a tile width.
pub fn select_quality_id(layers: &[Value], tile_width: u32) -> u8 {
    if layers.is_empty() {
        return 0xff;
    }
    // One layer per spatial index (id / 3), first seen.
    let mut spatial: Vec<&Value> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for layer in layers {
        let id = u32_of(layer, "id");
        let s = id / 3;
        if seen.insert(s) {
            spatial.push(layer);
        }
    }
    let width = tile_width;
    if width == 0 {
        return (u32_of(spatial[0], "id") & 0xff) as u8;
    }
    let mut top = spatial.len();
    if spatial.len() >= 3 {
        let hi = spatial[spatial.len() - 1];
        let hi_long = u32_of(hi, "b").max(u32_of(hi, "c"));
        if width < hi_long {
            top = spatial.len() - 1;
        }
    }
    let mut pick = spatial[0];
    for i in (0..top).rev() {
        let layer = spatial[i];
        let short = u32_of(layer, "b").min(u32_of(layer, "c"));
        if short <= width {
            pick = layer;
            break;
        }
    }
    (u32_of(pick, "id") & 0xff) as u8
}

/// Whether an EndCall reply is the peer hanging up (not an ack of our probe).
pub fn end_call_is_peer(
    cmd_is_end_call: bool,
    packet_call_id: Option<i64>,
    from_host: &str,
    from_port: u16,
    active_host: Option<&str>,
    active_port: Option<u16>,
    our_call_id: i64,
) -> bool {
    if !cmd_is_end_call {
        return false;
    }
    if active_host != Some(from_host) || active_port != Some(from_port) {
        return false;
    }
    let got = packet_call_id.unwrap_or(0) as u32;
    let ours = our_call_id as u32;
    !(got != 0 && ours != 0 && got != ours)
}

/// Before media.start: the first peer frame, then a full second of them.
pub fn early_audio_phase(started: bool, group: bool, rx: u64) -> Option<&'static str> {
    if started || group {
        return None;
    }
    match rx {
        1 => Some("first"),
        50 => Some("stream"),
        _ => None,
    }
}

/// Negotiated Opus/pre-filter encoder flags parsed from `zrtcConfig`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioEncoderOptions {
    /// Target bitrate in bits per second (`audioBitrate * 1000`).
    pub bitrate_bps: Option<i32>,
    /// Encoder complexity (`opusComplex`).
    pub complexity: Option<i32>,
    /// Explicit in-band FEC switch (`audioFecInband`).
    pub inband_fec: Option<bool>,
    /// Explicit 100 Hz high-pass switch (`audioHighPassFilter`).
    pub high_pass: Option<bool>,
}

/// Parses encoder options from `zrtcConfig`.
pub fn audio_encoder_options(config: &Value) -> AudioEncoderOptions {
    let c = config.as_object();
    let bitrate_bps = num(c, "audioBitrate")
        .filter(|br| *br > 0.0)
        .map(|br| ((br.trunc() as i64) * 1000).clamp(6_000, 128_000) as i32);
    let complexity = num(c, "opusComplex")
        .filter(|complex| *complex >= 0.0)
        .map(|complex| complex.trunc().clamp(0.0, 10.0) as i32);
    let inband_fec = c
        .and_then(|m| m.get("audioFecInband"))
        .and_then(Value::as_bool);
    let high_pass = c
        .and_then(|m| m.get("audioHighPassFilter"))
        .and_then(Value::as_bool);
    AudioEncoderOptions {
        bitrate_bps,
        complexity,
        inband_fec,
        high_pass,
    }
}

/// WebRTC audio-processing toggles parsed from `zrtcConfig` (matching Windows
/// `ZaloCall.exe` and Android `libzrtc.so`).
///
/// The Rust audio stack runs WebRTC AudioProcessing (`sonora`: AEC3, leveled
/// NS, AGC2 + limiter, optional HPF) in-process directly between `CardSpeaker`
/// and `CardMic`, so these flags match the official server defaults directly
/// instead of disabling AEC/NS for PipeWire's external plugin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioProcessingOptions {
    /// Acoustic Echo Cancellation (AEC3). On unless `audioEchoCancellation: false`
    /// or `ZCALL_AEC=0` / `ZCALL_AUDIO_PROCESSING=0`.
    pub aec: bool,
    /// Noise suppression (`audioNoiseSuppression`, default `true`).
    pub noise_suppression: bool,
    /// Noise suppression level (`audioNSLevel`: 0=Low, 1=Moderate, 2=High, 3=VeryHigh).
    /// Official Windows/Android configs use level `2` (`High`).
    pub ns_level: u8,
    /// High-pass filter (`audioHighPassFilter`, default `false` matching Windows/Android).
    pub high_pass: bool,
    /// Automatic gain control (`audioGainControl`, default `true`).
    pub gain_control: bool,
    /// Official `agcGain` cap in dB (default `9` dB).
    pub agc_gain_db: u8,
    /// Voice activity detection (`audioVoiceDetection`, default `false`).
    pub voice_detection: bool,
}

impl Default for AudioProcessingOptions {
    fn default() -> Self {
        Self {
            aec: true,
            // WebRTC NS on Linux USB/BT mics cuts continuous speech by 19–32 dB
            // after 1–2 s (observed in Linux microphone benchmarks).
            // Keep off by default; opt-in via ZCALL_NOISE_SUPPRESSION=1.
            noise_suppression: false,
            ns_level: 0,
            high_pass: false,
            // Zalo's agcMode=3 is fixed digital, not AGC2 AdaptiveDigital.
            // Keep sonora AGC2 off so AudioPipeline's single smooth MicLeveler
            // owns gain without pumping background noise/comfort noise.
            gain_control: false,
            agc_gain_db: 9,
            voice_detection: false,
        }
    }
}

/// Returns `true` only when the user explicitly opted in via
/// `ZCALL_NOISE_SUPPRESSION=1` (and did not disable all processing).
pub fn noise_suppression_requested() -> bool {
    std::env::var("ZCALL_AUDIO_PROCESSING").ok().as_deref() != Some("0")
        && std::env::var("ZCALL_NOISE_SUPPRESSION").ok().as_deref() == Some("1")
}

/// Returns `true` only when the user explicitly opted into WebRTC AGC2 via
/// `ZCALL_AGC2=1`.
pub fn agc2_requested() -> bool {
    std::env::var("ZCALL_AUDIO_PROCESSING").ok().as_deref() != Some("0")
        && std::env::var("ZCALL_AGC2").ok().as_deref() == Some("1")
}

/// Returns `true` unless the user explicitly opted out via `ZCALL_AEC=0`
/// or `ZCALL_AUDIO_PROCESSING=0`.
pub fn echo_cancellation_requested() -> bool {
    std::env::var("ZCALL_AUDIO_PROCESSING").ok().as_deref() != Some("0")
        && std::env::var("ZCALL_AEC").ok().as_deref() != Some("0")
}

/// Parses WebRTC audio-processing flags from `zrtcConfig`.
pub fn audio_processing_options(config: &Value) -> AudioProcessingOptions {
    let c = config.as_object();
    let ns_level = num(c, "audioNSLevel")
        .map(|v| v.round().clamp(0.0, 3.0) as u8)
        .unwrap_or(0);
    let agc_gain_db = num(c, "agcGain")
        .filter(|v| *v >= 0.0)
        .map(|v| v.round().clamp(0.0, 30.0) as u8)
        .unwrap_or(9);
    AudioProcessingOptions {
        aec: echo_cancellation_requested() && !is_false(c, "audioEchoCancellation"),
        noise_suppression: noise_suppression_requested() && !is_false(c, "audioNoiseSuppression"),
        ns_level,
        high_pass: is_true(c, "audioHighPassFilter"),
        gain_control: agc2_requested() && !is_false(c, "audioGainControl"),
        agc_gain_db,
        voice_detection: is_true(c, "audioVoiceDetection"),
    }
}

/// Group encoder flags from the zrtcConfig, as `groupAudioArgs`.
pub fn group_audio_args(config: &Value) -> Vec<String> {
    let opts = audio_encoder_options(config);
    let mut args = Vec::new();
    if let Some(bps) = opts.bitrate_bps {
        args.push("--bitrate".to_owned());
        args.push(bps.to_string());
    }
    if let Some(complex) = opts.complexity {
        args.push("--complexity".to_owned());
        args.push(complex.to_string());
    }
    if opts.inband_fec == Some(false) {
        args.push("--no-fec".to_owned());
    }
    if opts.high_pass == Some(false) {
        args.push("--no-high-pass".to_owned());
    }
    args
}

/// Jitter buffer floor and ceiling in milliseconds, as `callAudioArgs` plus
/// `--min-delay`.
///
/// The ceiling is `maxAudioDelayMs` (1000 or 2000 in captured configs).
/// `audioJitterMax` is 50 or 100 in those same configs and is only the
/// fallback: using the smaller of the two pinned playout to "delay 100..50"
/// and time-compressed speech. When neither is set, audio-io's default is
/// 200 ms (`--jitter 10`).
pub fn playout_delay_ms(config: &Value) -> (u32, u32) {
    let c = config.as_object();
    let min_ms = num(c, "minAudioDelayMs")
        .filter(|v| *v > 0.0)
        .unwrap_or(100.0);
    let max_ms = num(c, "maxAudioDelayMs")
        .filter(|v| *v > 0.0)
        .or_else(|| num(c, "audioJitterMax").filter(|v| *v > 0.0))
        .unwrap_or(200.0);
    let min_ms = min_ms.round().clamp(20.0, 10_000.0);
    let max_ms = max_ms.round().clamp(min_ms, 10_000.0);
    (min_ms as u32, max_ms as u32)
}

/// The audio args the PipeWire plugin honors, as `callAudioArgs`.
pub fn call_audio_args(config: &Value) -> Vec<String> {
    let mut args = Vec::new();
    let c = config.as_object();
    let max_delay = num(c, "maxAudioDelayMs").filter(|v| *v > 0.0);
    let jitter_max = num(c, "audioJitterMax").filter(|v| *v > 0.0);
    let cap = max_delay.or(jitter_max);
    if let Some(cap) = cap {
        args.push("--max-delay".to_owned());
        args.push(format!("{}", cap.trunc() as i64));
    }
    let proc = audio_processing_options(config);
    if !proc.aec {
        args.push("--no-aec".to_owned());
    }
    if !proc.noise_suppression {
        args.push("--no-ns".to_owned());
    }
    if !proc.high_pass {
        args.push("--no-high-pass".to_owned());
    }
    if proc.voice_detection {
        args.push("--vad".to_owned());
    }
    args
}

fn num(obj: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<f64> {
    obj.and_then(|m| m.get(key)).and_then(Value::as_f64)
}

fn is_false(obj: Option<&serde_json::Map<String, Value>>, key: &str) -> bool {
    obj.and_then(|m| m.get(key)).and_then(Value::as_bool) == Some(false)
}

fn is_true(obj: Option<&serde_json::Map<String, Value>>, key: &str) -> bool {
    obj.and_then(|m| m.get(key)).and_then(Value::as_bool) == Some(true)
}

fn u32_of(value: &Value, key: &str) -> u32 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_rfc6464_level() {
        // RTP v2, CSRC 0, ext present; id 2 len 0 -> level byte follows.
        let mut rtp = vec![0x90, 0x70, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        rtp.extend_from_slice(&[0xbe, 0xde, 0x00, 0x01]); // one 32-bit extension word
        rtp.extend_from_slice(&[0x20, 0x41]); // id 2, len 1 -> level 0x41 & 0x7f = 65
        rtp.push(0); // padding
        assert_eq!(rtp_audio_level(&rtp), Some(65));
        assert_eq!(rtp_audio_level(&[0x80, 0x00]), None); // no extension
    }

    #[test]
    fn picks_the_layer_for_a_tile() {
        let layers = json!([
            {"id": 0, "b": 180, "c": 320},
            {"id": 3, "b": 360, "c": 640},
            {"id": 6, "b": 720, "c": 1280}
        ]);
        let arr = layers.as_array().unwrap();
        assert_eq!(select_quality_id(arr, 0), 0); // no tile: lowest
        assert_eq!(select_quality_id(arr, 560), 3); // fits 360x640
        assert_eq!(select_quality_id(arr, 2000), 6); // fits the top
        assert_eq!(select_quality_id(&[], 100), 0xff);
    }

    #[test]
    fn detects_only_a_real_peer_end_call() {
        assert!(end_call_is_peer(
            true,
            Some(7),
            "1.2.3.4",
            9000,
            Some("1.2.3.4"),
            Some(9000),
            7
        ));
        // A different server: not the peer.
        assert!(!end_call_is_peer(
            true,
            Some(7),
            "9.9.9.9",
            9000,
            Some("1.2.3.4"),
            Some(9000),
            7
        ));
        // A different call id: not the peer.
        assert!(!end_call_is_peer(
            true,
            Some(8),
            "1.2.3.4",
            9000,
            Some("1.2.3.4"),
            Some(9000),
            7
        ));
    }

    #[test]
    fn maps_the_audio_config_to_args() {
        let config = json!({
            "audioBitrate": 20,
            "opusComplex": 3,
            "audioFecInband": false,
            "audioHighPassFilter": false,
            "maxAudioDelayMs": 1000,
            "audioEchoCancellation": false,
            "audioNoiseSuppression": false,
            "audioVoiceDetection": true
        });
        assert_eq!(
            group_audio_args(&config),
            vec![
                "--bitrate",
                "20000",
                "--complexity",
                "3",
                "--no-fec",
                "--no-high-pass"
            ]
        );
        assert_eq!(
            call_audio_args(&config),
            vec![
                "--max-delay",
                "1000",
                "--no-aec",
                "--no-ns",
                "--no-high-pass",
                "--vad"
            ]
        );
    }

    #[test]
    fn jitter_ceiling_is_max_audio_delay_not_audio_jitter_max() {
        let config = json!({
            "minAudioDelayMs": 100,
            "maxAudioDelayMs": 2000,
            "audioJitterMax": 50
        });
        assert_eq!(playout_delay_ms(&config), (100, 2000));
        assert_eq!(playout_delay_ms(&json!({"audioJitterMax": 50})), (100, 100));
        assert_eq!(playout_delay_ms(&json!({})), (100, 200));
    }

    #[test]
    fn reports_the_early_audio_phase() {
        assert_eq!(early_audio_phase(false, false, 1), Some("first"));
        assert_eq!(early_audio_phase(false, false, 50), Some("stream"));
        assert_eq!(early_audio_phase(true, false, 1), None);
        assert_eq!(early_audio_phase(false, true, 1), None);
        assert_eq!(early_audio_phase(false, false, 2), None);
    }

    #[test]
    fn audio_processing_defaults_match_windows_zrtc_config() {
        let opts = audio_processing_options(&json!({}));
        assert!(opts.aec);
        assert!(!opts.noise_suppression);
        assert!(!opts.gain_control);
        assert_eq!(opts.agc_gain_db, 9);
        assert!(!opts.high_pass);

        let custom = audio_processing_options(&json!({
            "audioEchoCancellation": false,
            "audioNoiseSuppression": true,
            "audioNSLevel": 3,
            "audioHighPassFilter": true,
            "audioGainControl": false,
            "agcGain": 15
        }));
        assert!(!custom.aec);
        assert!(!custom.noise_suppression);
        assert_eq!(custom.ns_level, 3);
        assert!(custom.high_pass);
        assert!(!custom.gain_control);
        assert_eq!(custom.agc_gain_db, 15);
    }
}
