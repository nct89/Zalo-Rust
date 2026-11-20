//! Private PipeWire WebRTC echo-cancellation and noise-suppression daemon.
//!
//! Spawns an isolated `pipewire -c <conf>` process loading
//! `libpipewire-module-echo-cancel` (`libspa-aec-webrtc`) with
//! `voice_detection = false` and `gain_control = false` (mirroring
//! `Processing` in `audio-io.py`), exposing virtual `<tag>_source` and
//! `<tag>_sink` nodes for `parec` and `pacat`.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use zcall_core::zrtc_helpers::AudioProcessingOptions;

static NEXT_TAG: AtomicU64 = AtomicU64::new(1);

const POLL_TIMEOUT: Duration = Duration::from_millis(1_500);
const POLL_STEP: Duration = Duration::from_millis(50);

/// Promotes user-owned PipeWire `data-loop` threads to `SCHED_RR` via RTKit
/// when they are running under normal time-sharing (`SCHED_OTHER`).
///
/// Mirrors `pipewire_realtime()` in `audio-io.py`: prevents H.264 encode/decode
/// spikes from causing microphone overruns or speaker resyncs. Set
/// `ZCALL_PW_RT=0` to skip.
pub fn ensure_pipewire_realtime() {
    if std::env::var("ZCALL_PW_RT").as_deref() == Ok("0") {
        return;
    }
    let Some(my_uid) = std::fs::metadata("/proc/self").ok().map(|m| m.uid()) else {
        return;
    };
    let Ok(proc_dir) = std::fs::read_dir("/proc") else {
        return;
    };
    let mut low = Vec::new();
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let pid_str = name.to_string_lossy();
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        let proc_path = entry.path();
        if std::fs::metadata(&proc_path).ok().map(|m| m.uid()) != Some(my_uid) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(proc_path.join("comm")) else {
            continue;
        };
        let comm = comm.trim();
        if comm != "pipewire" && comm != "pipewire-pulse" {
            continue;
        }
        let Ok(tasks) = std::fs::read_dir(proc_path.join("task")) else {
            continue;
        };
        for task in tasks.flatten() {
            let tid_name = task.file_name();
            let Ok(tid) = tid_name.to_string_lossy().parse::<u32>() else {
                continue;
            };
            let task_path = task.path();
            let Ok(tcomm) = std::fs::read_to_string(task_path.join("comm")) else {
                continue;
            };
            if !tcomm.trim_start().starts_with("data-loop") {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(task_path.join("stat")) else {
                continue;
            };
            if parse_proc_stat_policy(&stat) == Some(0) {
                low.push((pid, tid));
            }
        }
    }
    if low.is_empty() {
        return;
    }
    let prio = rtkit_max_priority().unwrap_or(20);
    for (pid, tid) in low {
        let _ = Command::new("busctl")
            .args([
                "--system",
                "call",
                "org.freedesktop.RealtimeKit1",
                "/org/freedesktop/RealtimeKit1",
                "org.freedesktop.RealtimeKit1",
                "MakeThreadRealtimeWithPID",
                "ttu",
                &pid.to_string(),
                &tid.to_string(),
                &prio.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn rtkit_max_priority() -> Option<u32> {
    let out = Command::new("busctl")
        .args([
            "--system",
            "get-property",
            "org.freedesktop.RealtimeKit1",
            "/org/freedesktop/RealtimeKit1",
            "org.freedesktop.RealtimeKit1",
            "MaxRealtimePriority",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(1)?.parse().ok()
}

/// Extracts Linux scheduler policy from `/proc/<pid>/task/<tid>/stat`.
fn parse_proc_stat_policy(stat: &str) -> Option<u32> {
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(38)?.parse().ok()
}

/// Running private PipeWire `module-echo-cancel` instance.
pub struct AudioProcessing {
    source_node: String,
    sink_node: String,
    conf_path: PathBuf,
    child: Option<Child>,
}

impl AudioProcessing {
    /// Starts PipeWire echo cancellation / noise suppression if enabled and
    /// available on the system; returns `None` when disabled or unavailable so
    /// the caller can fall back cleanly to raw devices.
    pub fn start(
        mic: Option<&str>,
        speaker: Option<&str>,
        opts: AudioProcessingOptions,
    ) -> Option<Self> {
        if std::env::var_os("ZCALL_AUDIO_PROCESSING").is_some_and(|v| v == "0") {
            return None;
        }
        if !opts.aec && !opts.noise_suppression {
            return None;
        }
        let seq = NEXT_TAG.fetch_add(1, Ordering::Relaxed);
        let tag = format!("zcall-ec-{}-{seq}", std::process::id());
        let conf_path = std::env::temp_dir().join(format!("{tag}.conf"));
        let conf = render_config(&tag, mic, speaker, opts);

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&conf_path)
            .ok()?;
        if file.write_all(conf.as_bytes()).is_err() {
            let _ = std::fs::remove_file(&conf_path);
            return None;
        }
        drop(file);

        let Ok(mut child) = Command::new("pipewire")
            .arg("-c")
            .arg(&conf_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            let _ = std::fs::remove_file(&conf_path);
            return None;
        };

        let source_node = format!("{tag}_source");
        let sink_node = format!("{tag}_sink");
        if !wait_for_nodes(&mut child, &source_node, &sink_node) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&conf_path);
            return None;
        }

        Some(Self {
            source_node,
            sink_node,
            conf_path,
            child: Some(child),
        })
    }

    /// Virtual PipeWire source node name to pass to `parec -d`.
    pub fn source_name(&self) -> &str {
        &self.source_node
    }

    /// Virtual PipeWire sink node name to pass to `pacat -d`.
    pub fn sink_name(&self) -> &str {
        &self.sink_node
    }

    /// Stops the PipeWire child and removes its temporary config file.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        remove_if_exists(&self.conf_path);
    }
}

impl Drop for AudioProcessing {
    fn drop(&mut self) {
        self.stop();
    }
}

fn remove_if_exists(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

fn wait_for_nodes(child: &mut Child, source: &str, sink: &str) -> bool {
    let deadline = Instant::now() + POLL_TIMEOUT;
    let mut have_source = false;
    let mut have_sink = false;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return false;
        }
        if !have_source {
            have_source = pactl_has_node("sources", source);
        }
        if !have_sink {
            have_sink = pactl_has_node("sinks", sink);
        }
        if have_source && have_sink {
            return true;
        }
        thread::sleep(POLL_STEP);
    }
    false
}

fn pactl_has_node(kind: &str, node: &str) -> bool {
    let Ok(out) = Command::new("pactl")
        .args(["list", "short", kind])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    output_has_node(&text, node)
}

fn output_has_node(text: &str, node: &str) -> bool {
    text.lines()
        .any(|line| line.split('\t').nth(1) == Some(node))
}

fn escape_pw_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

fn format_target(device: Option<&str>) -> String {
    match device.map(str::trim).filter(|s| !s.is_empty()) {
        Some(name) => format!(
            "target.object = \"{}\" node.dont-reconnect = true",
            escape_pw_str(name)
        ),
        None => String::new(),
    }
}

fn render_config(
    tag: &str,
    mic: Option<&str>,
    speaker: Option<&str>,
    opts: AudioProcessingOptions,
) -> String {
    let ns = if opts.noise_suppression {
        "true"
    } else {
        "false"
    };
    let hpf = if opts.high_pass { "true" } else { "false" };
    let vad = if opts.voice_detection {
        "true"
    } else {
        "false"
    };
    let mic_target = format_target(mic);
    let spk_target = format_target(speaker);
    // Clock stays on the session default. Forcing 48 kHz / quantum 256 against
    // this node's 16 kHz stream (256/48000 = 5.33 ms, not a divisor of the
    // 20 ms Opus frame) resamples on a grid the capture clock cannot meet.
    // Electron's `EC_CONF` does not pin a quantum.
    format!(
        r#"context.properties = {{
    log.level = 0
}}
context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}
context.modules = [
    {{ name = libpipewire-module-rt args = {{ nice.level = -11 rt.prio = 88 rtportal.enabled = false }} flags = [ ifexists nofail ] }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-echo-cancel
      args = {{
        library.name = aec/libspa-aec-webrtc
        aec.args = {{
            webrtc.gain_control = false
            webrtc.noise_suppression = {ns}
            webrtc.high_pass_filter = {hpf}
            webrtc.voice_detection = {vad}
            webrtc.transient_suppression = false
        }}
        audio.rate = 16000
        audio.channels = 1
        audio.position = [ MONO ]
        capture.props  = {{ node.name = "{tag}.capture" node.passive = true {mic_target} }}
        source.props   = {{ node.name = "{tag}_source" node.description = "Zalo call microphone (processed)" priority.session = 0 priority.driver = 0 }}
        sink.props     = {{ node.name = "{tag}_sink" node.description = "Zalo call speaker (processed)" priority.session = 0 priority.driver = 0 }}
        playback.props = {{ node.name = "{tag}.playback" node.passive = true {spk_target} }}
      }}
    }}
]
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_config_with_escaped_targets() {
        let opts = AudioProcessingOptions {
            aec: true,
            noise_suppression: true,
            ns_level: 2,
            high_pass: false,
            gain_control: true,
            agc_gain_db: 9,
            voice_detection: false,
        };
        let conf = render_config("zcall-ec-1-1", Some("alsa_input.\"usb\"\nfoo"), None, opts);
        assert!(conf.contains("webrtc.noise_suppression = true"));
        assert!(conf.contains("webrtc.gain_control = false"));
        assert!(!conf.contains("default.clock.quantum"));
        assert!(conf.contains("webrtc.high_pass_filter = false"));
        assert!(conf
            .contains("target.object = \"alsa_input.\\\"usb\\\"foo\" node.dont-reconnect = true"));
        assert!(conf.contains("node.name = \"zcall-ec-1-1_source\""));
        assert!(conf.contains("node.name = \"zcall-ec-1-1_sink\""));
    }

    #[test]
    fn matches_exact_pactl_node_column() {
        let out = "42\tzcall-ec-1_source\tPipeWire\ts16le\n43\tzcall-ec-1_source.monitor\tPipeWire\ts16le\n";
        assert!(output_has_node(out, "zcall-ec-1_source"));
        assert!(!output_has_node(out, "zcall-ec-1"));
    }

    #[test]
    fn parses_proc_stat_scheduler_policy() {
        let mut fields = vec!["0"; 42];
        fields[38] = "2"; // SCHED_RR
        let rr_stat = format!("123 (data-loop (0)) {}", fields.join(" "));
        assert_eq!(parse_proc_stat_policy(&rr_stat), Some(2));
        fields[38] = "0"; // SCHED_OTHER
        let ts_stat = format!("123 (data-loop.0) {}", fields.join(" "));
        assert_eq!(parse_proc_stat_policy(&ts_stat), Some(0));
    }
}
