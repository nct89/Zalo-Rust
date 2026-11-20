//! Microphone capture via `parec`, and speaker playout.
//!
//! Playback prefers `libpulse-simple`: `write` blocks until the card has room,
//! so the playout loop runs on the sound-card clock. `audio-io.py` (`PulseOut`)
//! does the same. A `pacat` pipe holds about two seconds, and a software 20 ms
//! clock then drifts away from the card (the latency grows, or the card runs
//! dry and clicks). `pacat` remains the fallback when the library is missing.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};

/// Bytes of one 20 ms, 16 kHz mono i16 frame.
pub const FRAME_BYTES: usize = 640;

/// The microphone capture command.
///
/// Same flags as `audio-io.py` `mic_command`: 16 kHz mono s16le, 20 ms.
/// Encode runs on another thread, so a slow Opus frame does not stall this read.
pub fn mic_command(device: Option<&str>, source: Option<&str>, pulse_running: bool) -> Vec<String> {
    if device.is_none() && pulse_running {
        let mut cmd = vec!["parec".to_owned()];
        if let Some(source) = source {
            cmd.push("-d".to_owned());
            cmd.push(source.to_owned());
        }
        for flag in [
            "--raw",
            "--rate=16000",
            "--channels=1",
            "--format=s16le",
            "--latency-msec=20",
        ] {
            cmd.push(flag.to_owned());
        }
        cmd
    } else {
        vec![
            "arecord".to_owned(),
            "-q".to_owned(),
            "-D".to_owned(),
            device.unwrap_or("default").to_owned(),
            "-t".to_owned(),
            "raw".to_owned(),
            "-f".to_owned(),
            "S16_LE".to_owned(),
            "-r".to_owned(),
            "16000".to_owned(),
            "-c".to_owned(),
            "1".to_owned(),
        ]
    }
}

/// Fallback speaker command when `libpulse-simple` cannot open.
///
/// Matches `audio-io.py` `speaker_command`: 60 ms, not a 100 ms ring. The
/// playout thread still has to pace this path itself. Prefer [`CardSpeaker`].
pub fn speaker_command(sink: Option<&str>) -> Vec<String> {
    let mut cmd = vec!["pacat".to_owned()];
    if let Some(sink) = sink {
        cmd.push("-d".to_owned());
        cmd.push(sink.to_owned());
    }
    for flag in [
        "--raw",
        "--rate=16000",
        "--channels=1",
        "--format=s16le",
        "--latency-msec=60",
    ] {
        cmd.push(flag.to_owned());
    }
    cmd
}

/// `true` when `name` is on `PATH`.
fn tool_exists(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':').any(|dir| {
        let dir = dir.trim();
        !dir.is_empty() && std::path::Path::new(dir).join(name).is_file()
    })
}

/// Capture command for this machine.
///
/// `parec` when Pulse tools are installed. Otherwise `pw-record`, which
/// PipeWire ships and which can open a Bluetooth headset source by node name.
/// ALSA `arecord` remains the fallback when neither is present.
pub fn live_mic_command(
    device: Option<&str>,
    source: Option<&str>,
    pulse_running: bool,
) -> Vec<String> {
    if pulse_running && !tool_exists("parec") && tool_exists("pw-record") {
        return pw_record_command(source);
    }
    mic_command(device, source, pulse_running && tool_exists("parec"))
}

/// Playback command when `libpulse-simple` cannot open.
///
/// Uses `pacat` when it exists, otherwise `pw-cat` in playback mode.
pub fn live_speaker_command(sink: Option<&str>) -> Vec<String> {
    if !tool_exists("pacat") && tool_exists("pw-cat") {
        return pw_play_command(sink);
    }
    speaker_command(sink)
}

/// `pw-record` of one 16 kHz mono s16 stream on stdout.
fn pw_record_command(source: Option<&str>) -> Vec<String> {
    let mut cmd = vec![
        "pw-record".to_owned(),
        "-a".to_owned(),
        "--rate".to_owned(),
        "16000".to_owned(),
        "--channels".to_owned(),
        "1".to_owned(),
        "--channel-map".to_owned(),
        "MONO".to_owned(),
        "--format".to_owned(),
        "s16".to_owned(),
        "--latency".to_owned(),
        "20ms".to_owned(),
    ];
    if let Some(source) = source {
        cmd.push("--target".to_owned());
        cmd.push(source.to_owned());
    }
    cmd.push("-".to_owned());
    cmd
}

/// `pw-cat --playback` of one 16 kHz mono s16 stream on stdin.
fn pw_play_command(sink: Option<&str>) -> Vec<String> {
    let mut cmd = vec![
        "pw-cat".to_owned(),
        "-p".to_owned(),
        "-a".to_owned(),
        "--rate".to_owned(),
        "16000".to_owned(),
        "--channels".to_owned(),
        "1".to_owned(),
        "--channel-map".to_owned(),
        "MONO".to_owned(),
        "--format".to_owned(),
        "s16".to_owned(),
        "--latency".to_owned(),
        "60ms".to_owned(),
    ];
    if let Some(sink) = sink {
        cmd.push("--target".to_owned());
        cmd.push(sink.to_owned());
    }
    cmd.push("-".to_owned());
    cmd
}

/// Target device buffer of [`CardSpeaker`], in milliseconds.
pub const CARD_LATENCY_MS: u32 = 60;

/// Playback through `libpulse-simple`. `write` blocks until the stream has room.
pub struct CardSpeaker {
    simple: libpulse_simple_binding::Simple,
}

impl CardSpeaker {
    /// Opens the default sink, or `device` when it is set.
    ///
    /// # Errors
    ///
    /// Returns an error when `libpulse-simple` cannot connect. Callers fall
    /// back to [`Speaker`].
    pub fn open(device: Option<&str>) -> Result<Self, std::io::Error> {
        use libpulse_binding::def::BufferAttr;
        use libpulse_binding::sample::{Format, Spec};
        use libpulse_binding::stream::Direction;
        use libpulse_simple_binding::Simple;

        let spec = Spec {
            format: Format::S16le,
            rate: 16_000,
            channels: 1,
        };
        if !spec.is_valid() {
            return Err(std::io::Error::other("invalid playback spec"));
        }
        // `u32::MAX` is the "leave this field alone" value (`0xFFFFFFFF`).
        // `tlength` is 60 ms: 640 bytes × 60 / 20.
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: (FRAME_BYTES as u32) * CARD_LATENCY_MS / 20,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };
        let simple = Simple::new(
            None,
            "Zalo",
            Direction::Playback,
            device,
            "Call",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|err| std::io::Error::other(format!("pa_simple_new failed ({err})")))?;
        Ok(Self { simple })
    }

    /// Writes one PCM buffer. Blocks while the 60 ms stream is full.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream has failed.
    pub fn write(&self, pcm: &[i16]) -> Result<(), std::io::Error> {
        let mut buf = [0_u8; FRAME_BYTES];
        for chunk in pcm.chunks(FRAME_BYTES / 2) {
            let bytes_len = chunk.len() * 2;
            for (dst, &sample) in buf[..bytes_len].chunks_exact_mut(2).zip(chunk.iter()) {
                dst.copy_from_slice(&sample.to_le_bytes());
            }
            self.simple
                .write(&buf[..bytes_len])
                .map_err(|err| std::io::Error::other(format!("pa_simple_write failed ({err})")))?;
        }
        Ok(())
    }

    /// Writes `frames` of silence so the canceller has a reference before the mic.
    ///
    /// # Errors
    ///
    /// Returns the error from [`Self::write`].
    pub fn prime_silence(&self, frames: usize) -> Result<(), std::io::Error> {
        let zeros = [0_i16; FRAME_BYTES / 2];
        for _ in 0..frames {
            self.write(&zeros)?;
        }
        Ok(())
    }

    /// Measured output stream latency in milliseconds, for WebRTC AEC3 delay
    /// alignment. Falls back to [`CARD_LATENCY_MS`] if unavailable.
    pub fn latency_ms(&self) -> u32 {
        self.simple
            .get_latency()
            .ok()
            .map(|us| (us.0 / 1_000).min(400) as u32)
            .unwrap_or(CARD_LATENCY_MS)
    }
}

/// Target capture fragment of [`CardMic`], in milliseconds.
pub const CARD_MIC_LATENCY_MS: u32 = 20;

/// Microphone capture directly through `libpulse-simple` on the sound-card
/// clock (zero `parec` subprocess or OS pipe buffering).
pub struct CardMic {
    simple: libpulse_simple_binding::Simple,
}

impl CardMic {
    /// Opens the default source, or `device` when set, at 16 kHz mono s16le.
    ///
    /// # Errors
    ///
    /// Returns an error when `libpulse-simple` cannot connect. Callers fall
    /// back to [`MicCapture`].
    pub fn open(device: Option<&str>) -> Result<Self, std::io::Error> {
        use libpulse_binding::def::BufferAttr;
        use libpulse_binding::sample::{Format, Spec};
        use libpulse_binding::stream::Direction;
        use libpulse_simple_binding::Simple;

        let spec = Spec {
            format: Format::S16le,
            rate: 16_000,
            channels: 1,
        };
        if !spec.is_valid() {
            return Err(std::io::Error::other("invalid capture spec"));
        }
        // `fragsize` = 20 ms (640 bytes). Leave `maxlength` at `u32::MAX` so
        // PipeWire (`pipewire-pulse`) manages its ring around the device
        // quantum (often 21.3 ms / 42.6 ms at 48 kHz); a small `maxlength`
        // causes continuous server-side buffer drops and harsh crackle on USB mics.
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: FRAME_BYTES as u32,
        };
        let simple = Simple::new(
            None,
            "Zalo",
            Direction::Record,
            device,
            "Call",
            &spec,
            None,
            Some(&attr),
        )
        .map_err(|err| std::io::Error::other(format!("pa_simple_new record failed ({err})")))?;
        Ok(Self { simple })
    }

    /// Reads one 20 ms (320-sample) i16 PCM frame directly from the sound card.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream disconnects or fails.
    pub fn read_frame(&self) -> Result<Vec<i16>, std::io::Error> {
        let mut buf = [0_u8; FRAME_BYTES];
        self.simple
            .read(&mut buf)
            .map_err(|err| std::io::Error::other(format!("pa_simple_read failed ({err})")))?;
        Ok(buf
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect())
    }

    /// Measured capture stream latency in milliseconds, for WebRTC AEC3 delay
    /// alignment. Falls back to [`CARD_MIC_LATENCY_MS`] if unavailable.
    pub fn latency_ms(&self) -> u32 {
        self.simple
            .get_latency()
            .ok()
            .map(|us| (us.0 / 1_000).min(200) as u32)
            .unwrap_or(CARD_MIC_LATENCY_MS)
    }
}

/// A running microphone capture process.
pub struct MicCapture {
    proc: Option<Child>,
}

impl MicCapture {
    /// Spawns an arbitrary command whose stdout is the raw PCM stream.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the command cannot start.
    pub fn spawn(command: &[String]) -> Result<Self, std::io::Error> {
        let mut parts = command.iter();
        let program = parts.next().cloned().unwrap_or_default();
        let child = Command::new(&program)
            .args(parts)
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(Self { proc: Some(child) })
    }

    /// Spawns the microphone command from `mic_command`.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the command cannot start.
    pub fn from_mic(
        device: Option<&str>,
        source: Option<&str>,
        pulse_running: bool,
    ) -> Result<Self, std::io::Error> {
        Self::spawn(&mic_command(device, source, pulse_running))
    }

    /// Reads one 640-byte frame, blocking until it is complete.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let proc = self.proc.as_mut()?;
        let stdout = proc.stdout.as_mut()?;
        let mut frame = vec![0_u8; FRAME_BYTES];
        let mut filled = 0;
        while filled < FRAME_BYTES {
            match stdout.read(&mut frame[filled..]) {
                Ok(0) => return None,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
        Some(frame)
    }

    /// Hands stdout to another thread. `close` still kills the process, which
    /// unblocks that read.
    pub fn take_stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.proc.as_mut()?.stdout.take()
    }

    /// Stops the capture process.
    pub fn close(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            let _ = proc.kill();
            let _ = proc.wait();
        }
    }
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.close();
    }
}

/// A running speaker playout process (pacat), fed PCM on stdin.
pub struct Speaker {
    proc: Option<Child>,
}

impl Speaker {
    /// Spawns an arbitrary command whose stdin receives raw i16 PCM.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the command cannot start.
    pub fn spawn(command: &[String]) -> Result<Self, std::io::Error> {
        let mut parts = command.iter();
        let program = parts.next().cloned().unwrap_or_default();
        let child = Command::new(&program)
            .args(parts)
            .stdin(Stdio::piped())
            .spawn()?;
        Ok(Self { proc: Some(child) })
    }

    /// Spawns the speaker command from ¤speaker_command¤.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the command cannot start.
    pub fn from_sink(sink: Option<&str>) -> Result<Self, std::io::Error> {
        Self::spawn(&speaker_command(sink))
    }

    /// Writes PCM samples to the speaker without per-frame heap allocation.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the write fails or the process is gone.
    pub fn write(&mut self, pcm: &[i16]) -> Result<(), std::io::Error> {
        let stdin = self
            .proc
            .as_mut()
            .and_then(|p| p.stdin.as_mut())
            .ok_or_else(|| std::io::Error::other("speaker stdin closed"))?;
        let mut buf = [0_u8; FRAME_BYTES];
        for chunk in pcm.chunks(FRAME_BYTES / 2) {
            let bytes_len = chunk.len() * 2;
            for (dst, &sample) in buf[..bytes_len].chunks_exact_mut(2).zip(chunk.iter()) {
                dst.copy_from_slice(&sample.to_le_bytes());
            }
            stdin.write_all(&buf[..bytes_len])?;
        }
        Ok(())
    }

    /// Prefills `frames` silent 20 ms frames right after spawn so PulseAudio's
    /// prebuffer starts full and the echo canceller has a live reference.
    ///
    /// # Errors
    ///
    /// Returns the OS error when writing silence fails.
    pub fn prime_silence(&mut self, frames: usize) -> Result<(), std::io::Error> {
        if frames == 0 {
            return Ok(());
        }
        let stdin = self
            .proc
            .as_mut()
            .and_then(|p| p.stdin.as_mut())
            .ok_or_else(|| std::io::Error::other("speaker stdin closed"))?;
        let zero = [0_u8; FRAME_BYTES];
        for _ in 0..frames {
            stdin.write_all(&zero)?;
        }
        Ok(())
    }

    /// Closes stdin and waits, so the child flushes and exits cleanly.
    pub fn finish(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            let _ = proc.stdin.take();
            let _ = proc.wait();
        }
    }

    /// Stops the speaker process.
    pub fn close(&mut self) {
        if let Some(mut proc) = self.proc.take() {
            let _ = proc.kill();
            let _ = proc.wait();
        }
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_parec_command() {
        assert_eq!(
            mic_command(None, Some("alsa_input.usb"), true),
            vec![
                "parec",
                "-d",
                "alsa_input.usb",
                "--raw",
                "--rate=16000",
                "--channels=1",
                "--format=s16le",
                "--latency-msec=20",
            ]
        );
        assert_eq!(
            mic_command(None, None, true),
            vec![
                "parec",
                "--raw",
                "--rate=16000",
                "--channels=1",
                "--format=s16le",
                "--latency-msec=20",
            ]
        );
    }

    #[test]
    fn builds_the_arecord_fallback() {
        assert_eq!(
            mic_command(Some("hw:1"), None, false),
            vec![
                "arecord", "-q", "-D", "hw:1", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c",
                "1",
            ]
        );
    }

    #[test]
    fn pw_record_targets_the_headset_node() {
        let cmd = pw_record_command(Some("bluez_input.74:5B:C5:7F:B9:0E"));
        assert_eq!(cmd[0], "pw-record");
        assert!(cmd
            .windows(2)
            .any(|w| w == ["--target", "bluez_input.74:5B:C5:7F:B9:0E"]));
        assert_eq!(cmd.last().map(String::as_str), Some("-"));
    }

    #[test]
    fn builds_the_pacat_command() {
        assert_eq!(
            speaker_command(Some("alsa_output.usb")),
            vec![
                "pacat",
                "-d",
                "alsa_output.usb",
                "--raw",
                "--rate=16000",
                "--channels=1",
                "--format=s16le",
                "--latency-msec=60",
            ]
        );
    }

    #[test]
    fn speaker_primes_silence_frames() {
        let path = std::env::temp_dir().join(format!("zcall-prime-{}.pcm", std::process::id()));
        let child = Command::new("tee")
            .arg(path.to_string_lossy().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn tee");
        let mut speaker = Speaker { proc: Some(child) };
        speaker.prime_silence(3).expect("prime");
        speaker.finish();
        let data = std::fs::read(&path).expect("read");
        assert_eq!(data.len(), 3 * FRAME_BYTES);
        assert!(data.iter().all(|&b| b == 0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reads_a_frame_from_a_fake_process() {
        // /bin/dd writes 640 zero bytes then EOF: exactly one frame.
        let mut mic = MicCapture::spawn(&[
            "/bin/dd".to_owned(),
            "if=/dev/zero".to_owned(),
            "bs=640".to_owned(),
            "count=1".to_owned(),
        ])
        .expect("spawn dd");
        let frame = mic.next_frame().expect("frame");
        assert_eq!(frame.len(), FRAME_BYTES);
        assert!(frame.iter().all(|b| *b == 0));
        assert!(mic.next_frame().is_none());
    }

    #[test]
    fn speaker_writes_pcm_to_a_fake_process() {
        let path = std::env::temp_dir().join(format!("zcall-speaker-{}.pcm", std::process::id()));
        let child = Command::new("tee")
            .arg(path.to_string_lossy().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn tee");
        let mut speaker = Speaker { proc: Some(child) };
        speaker.write(&[1_i16, -2, 300, -400]).expect("write");
        speaker.finish();
        let data = std::fs::read(&path).expect("read");
        let expected: Vec<u8> = [1_i16, -2, 300, -400]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(data, expected);
        let _ = std::fs::remove_file(&path);
    }
}
