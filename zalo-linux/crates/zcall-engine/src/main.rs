//! `zcall-native`: the native call engine in place of Zalo's ZaloCall helper.
//!
//! Connects to Zalo's two call-v2 TCP sockets, sends the token line, and runs
//! the 1-1 control plane. `--media stub` (default): signalling only, no UDP.
//! `--media zrtc --send-udp`: real UDP media with InitZRTP, SRTP audio, and
//! the Rust audio pipeline (mic → DSP → Opus → media / media → jitter → speaker).

mod args;
mod emit;

use args::Args;
use emit::emit_frame;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use zalo_protocol::{load_call_v2_key, CallV2Frame, CallV2FrameParser};
use zcall_core::call_ui::{CallUiModel, UiAction};
use zcall_core::engine::{EngineCore, Media, NoopMedia, NoopUi, Ui};
use zcall_core::zrtc_backend::{poll_and_tick, SharedZrtcBackend, ZrtcMediaBackend};
use zcall_engine::audio_bridge::AudioBridge;

fn main() {
    let args = match Args::parse_from(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // Local hardware self-test: mic capture + Opus + speaker playout.
    if args.selftest {
        std::process::exit(run_selftest());
    }

    let key = match load_call_v2_key(&args.key_path) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("load key failed ({0}): {1}", args.key_path.display(), e);
            std::process::exit(2);
        }
    };
    let use_zrtc = args.media == "zrtc";
    eprintln!(
        "start: media={0} send_udp={1} ports {2}/{3} key {4}",
        args.media,
        args.send_udp,
        args.recv_port,
        args.send_port,
        args.key_path.display()
    );

    let mut recv = match TcpStream::connect(("127.0.0.1", args.recv_port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("connect recv port {} failed: {e}", args.recv_port);
            std::process::exit(1);
        }
    };
    if let Err(e) = recv.write_all(
        format!(
            "{}
",
            args.token
        )
        .as_bytes(),
    ) {
        eprintln!("write token failed: {e}");
        std::process::exit(1);
    }
    let mut send = match TcpStream::connect(("127.0.0.1", args.send_port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("connect send port {} failed: {e}", args.send_port);
            std::process::exit(1);
        }
    };
    if let Err(e) = send.write_all(
        format!(
            "{}
",
            args.token
        )
        .as_bytes(),
    ) {
        eprintln!("write token failed: {e}");
        std::process::exit(1);
    }
    let _ = send.set_nonblocking(true);

    // Media backend: real UDP when --media zrtc, else signalling only.
    let mut media: Box<dyn Media> = Box::new(NoopMedia);
    let mut zrtc: Option<Arc<Mutex<ZrtcMediaBackend>>> = None;
    let mut audio: Option<AudioBridge> = None;
    let mut rx_audio_in: Option<mpsc::Receiver<(u32, u32, Vec<u8>)>> = None;
    let mut rx_roster_slot: Option<mpsc::Receiver<Vec<u32>>> = None;
    let mut rx_left_slot: Option<mpsc::Receiver<(u32, u8)>> = None;
    let mut rx_gone_slot: Option<mpsc::Receiver<String>> = None;

    if use_zrtc {
        let mut backend = ZrtcMediaBackend::new(args.send_udp);
        backend.set_log(|msg: &str| eprintln!("[media] {msg}"));

        // Channel for decrypted audio from the peer → jitter buffer.
        let (tx_ai, rx_ai) = mpsc::channel::<(u32, u32, Vec<u8>)>();
        rx_audio_in = Some(rx_ai);
        backend.set_on_audio_seq(move |seq: u32, ssrc: u32, opus: &[u8]| {
            let _ = tx_ai.send((seq, ssrc, opus.to_vec()));
        });

        let (tx_roster, rx_roster) = mpsc::channel::<Vec<u32>>();
        backend.set_on_roster(move |uids| {
            let _ = tx_roster.send(uids.to_vec());
        });
        let (tx_left, rx_left) = mpsc::channel::<(u32, u8)>();
        backend.set_on_member_left(move |uid, sub| {
            let _ = tx_left.send((uid, sub));
        });
        let (tx_gone, rx_gone) = mpsc::channel::<String>();
        backend.set_on_removed(move |why| {
            let _ = tx_gone.send(why.to_owned());
        });
        rx_roster_slot = Some(rx_roster);
        rx_left_slot = Some(rx_left);
        rx_gone_slot = Some(rx_gone);

        let shared = SharedZrtcBackend::new(backend);
        zrtc = Some(shared.clone_inner());
        media = Box::new(shared);

        // Start the audio bridge (mic + speaker subprocesses).
        let mut bridge = AudioBridge::new();
        match bridge.start(None, None, None, true) {
            Ok(()) => {
                eprintln!("[audio] bridge started");
                audio = Some(bridge);
            }
            Err(e) => eprintln!("[audio] bridge start failed: {e}"),
        }
    }

    let mut call_window = CallWindow::connect();
    let ui: Box<dyn Ui> = match &call_window {
        Some(window) => Box::new(WindowUi {
            model: Arc::clone(&window.model),
        }),
        None => Box::new(NoopUi),
    };
    let mut engine = EngineCore::new(media, ui);
    engine.group_enabled = std::env::var("ZCALL_GROUP").ok().as_deref() != Some("0");
    let mut parser = CallV2FrameParser::new(key.clone(), false);
    engine.start();
    // The engine may already have frames queued (native-ready).
    flush(&mut engine, &mut recv, &key, args.verbose);

    let mut buf = [0_u8; 65_536];
    let mut last_ping = Instant::now();
    let mut last_status = Instant::now();
    let mut last_twcc = Instant::now();
    let mut last_report = Instant::now();
    let mut last_early_rx = 0_u64;
    // One 20 ms playout tick. The loop itself polls signalling every 5 ms.
    let mut next_play = Instant::now();
    loop {
        match send.read(&mut buf) {
            Ok(0) => {
                eprintln!("zalo closed the send socket");
                break;
            }
            Ok(n) => {
                let key = &key;
                parser.push(&buf[..n], &mut |frame| match frame {
                    CallV2Frame::Json(json) => {
                        if args.verbose {
                            eprintln!(
                                "<- {}",
                                json.get("type").and_then(|v| v.as_str()).unwrap_or("")
                            );
                        }
                        engine.on_zalo_frame(&json);
                    }
                    CallV2Frame::Token(_) => {}
                    CallV2Frame::Text(_) | CallV2Frame::Error { .. } => {}
                });
                if let Some(window) = call_window.as_mut() {
                    window.read_actions(&mut engine, &mut audio);
                }
                engine.tick();
                apply_group_media(
                    &mut engine,
                    &mut rx_roster_slot,
                    &mut rx_left_slot,
                    &mut rx_gone_slot,
                );
                flush(&mut engine, &mut recv, key, args.verbose);
                if let Some(window) = call_window.as_mut() {
                    window.write_outbound();
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(window) = call_window.as_mut() {
                    window.read_actions(&mut engine, &mut audio);
                }
                engine.tick();
                apply_group_media(
                    &mut engine,
                    &mut rx_roster_slot,
                    &mut rx_left_slot,
                    &mut rx_gone_slot,
                );
                flush(&mut engine, &mut recv, &key, args.verbose);

                // Drive the UDP media backend.
                let mut early_rx = None;
                if let Some(shared) = zrtc.as_ref() {
                    if let Ok(mut backend) = shared.lock() {
                        poll_and_tick(
                            &mut backend,
                            &mut last_ping,
                            &mut last_status,
                            &mut last_twcc,
                            &mut last_report,
                        );
                        // Route mic audio → media.
                        if let Some(bridge) = audio.as_mut() {
                            while let Some(out) = bridge.next_audio_out() {
                                backend.send_audio(&out.opus, out.level);
                            }
                        }
                        early_rx = Some(backend.rx_count());
                    }
                }
                if let Some(rx) = early_rx {
                    if last_early_rx < 1 && rx >= 1 {
                        engine.early_audio("first");
                    }
                    if last_early_rx < 50 && rx >= 50 {
                        engine.early_audio("stream");
                    }
                    last_early_rx = rx;
                }

                // Route media → jitter buffer.
                if let Some(rx) = rx_audio_in.as_mut() {
                    while let Ok((seq, ssrc, opus)) = rx.try_recv() {
                        if let Some(bridge) = audio.as_mut() {
                            let source = engine
                                .call
                                .as_ref()
                                .is_some_and(|c| c.group)
                                .then_some(ssrc);
                            bridge.push_audio_in(seq, &opus, source, monotonic_secs());
                        }
                    }
                }

                // One 20 ms frame. A faster tick time-compresses speech.
                if next_play.elapsed() >= Duration::from_millis(20) {
                    next_play = Instant::now();
                    if let Some(bridge) = audio.as_mut() {
                        if let Err(e) = bridge.tick_playout() {
                            eprintln!("[audio] playout error: {e}");
                            engine.audio_fault("speaker");
                        }
                    }
                }

                if let Some(window) = call_window.as_mut() {
                    window.write_outbound();
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                eprintln!("read error: {e}");
                break;
            }
        }
    }

    // Clean up.
    engine.media.stop();
    if let Some(mut bridge) = audio.take() {
        bridge.stop();
    }
}

/// Call window socket. Present only when Zalo set `ZCALL_UI_PORT` and `ZCALL_UI_TOKEN`.
struct CallWindow {
    stream: TcpStream,
    model: Arc<Mutex<CallUiModel>>,
    buf: Vec<u8>,
}

impl CallWindow {
    /// Connects to the loopback call window and sends the token line.
    fn connect() -> Option<Self> {
        let port = std::env::var("ZCALL_UI_PORT").ok()?.parse::<u16>().ok()?;
        let token = std::env::var("ZCALL_UI_TOKEN").ok()?;
        if port == 0 || token.is_empty() {
            return None;
        }
        let mut stream = match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("[ui] connect failed: {e}");
                return None;
            }
        };
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        if stream.write_all(format!("{token}\n").as_bytes()).is_err() {
            eprintln!("[ui] token write failed");
            return None;
        }
        if stream.set_nonblocking(true).is_err() {
            return None;
        }
        eprintln!("[ui] connected to the call window");
        Some(Self {
            stream,
            model: Arc::new(Mutex::new(CallUiModel::new())),
            buf: Vec::new(),
        })
    }

    /// Reads click and frame lines and applies them to the engine.
    fn read_actions(&mut self, engine: &mut EngineCore, audio: &mut Option<AudioBridge>) {
        let mut tmp = [0_u8; 8192];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    eprintln!("[ui] read: {e}");
                    break;
                }
            }
        }
        while let Some(pos) = self.buf.iter().position(|byte| *byte == b'\n') {
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let action = self.model.lock().unwrap().push_line(line);
            if let Some(action) = action {
                dispatch_ui(engine, audio, action);
            }
        }
    }

    /// Writes state lines the engine queued since the last poll.
    fn write_outbound(&mut self) {
        let lines = {
            let mut model = self.model.lock().unwrap();
            std::mem::take(&mut model.outbound)
        };
        for line in lines {
            if self.stream.write_all(line.as_bytes()).is_err()
                || self.stream.write_all(b"\n").is_err()
            {
                eprintln!("[ui] write failed");
                return;
            }
        }
    }
}

/// `Ui` adapter that records call-window lines on a shared model.
struct WindowUi {
    model: Arc<Mutex<CallUiModel>>,
}

impl Ui for WindowUi {
    fn open(&mut self, info: &serde_json::Value) {
        self.model.lock().unwrap().open(info);
    }
    fn status(&mut self, text: &str) {
        self.model.lock().unwrap().status(text, 0);
    }
    fn status_at(&mut self, text: &str, since: u64) {
        self.model.lock().unwrap().status(text, since);
    }
    fn close(&mut self, text: &str) {
        self.model.lock().unwrap().close(text);
    }
    fn incoming(&mut self, info: &serde_json::Value) {
        self.model.lock().unwrap().incoming(info);
    }
    fn rename(&mut self, name: &str) {
        self.model.lock().unwrap().rename(name);
    }
    fn peer(&mut self, peer: &serde_json::Value) {
        self.model.lock().unwrap().peer(peer);
    }
    fn members(&mut self, list: &serde_json::Value) {
        self.model.lock().unwrap().members(list);
    }
    fn notice(&mut self, text: &str) {
        self.model.lock().unwrap().notice(text);
    }
    fn request_key_frame(&mut self) {
        self.model.lock().unwrap().request_key_frame();
    }
    fn speaking(&mut self, uid: u32, on: bool) {
        self.model.lock().unwrap().speaking(uid, on);
    }
    fn mic_volume(&mut self, percent: u8) {
        self.model.lock().unwrap().mic_volume(percent);
    }
}

fn dispatch_ui(engine: &mut EngineCore, audio: &mut Option<AudioBridge>, action: UiAction) {
    match action {
        UiAction::Hangup => engine.hangup("local"),
        UiAction::Accept { cam_start_off } => engine.answer(cam_start_off),
        UiAction::Reject { why } => engine.reject(&why),
        UiAction::Mute { on } => {
            engine.mute(on);
            if let Some(bridge) = audio.as_mut() {
                bridge.set_mute(on);
            }
        }
        UiAction::Speaker { off } => {
            engine.speaker(off);
            if let Some(bridge) = audio.as_mut() {
                bridge.set_speaker(off);
            }
        }
        UiAction::Camera { on } => engine._cam_mic(Some(on)),
        UiAction::Screen { on } => engine.screen_share(on),
        UiAction::MicVolume { percent } => {
            engine.set_mic_volume(percent);
            if let Some(bridge) = audio.as_mut() {
                bridge.set_mic_volume(percent.clamp(0, 100) as u8);
            }
        }
        UiAction::NeedKey { why, src } => engine.ask_peer_key(&why, src),
        UiAction::VideoFrame { key, data, screen } => {
            engine.media.send_video_frame(key, &data, screen);
        }
        UiAction::Log { text } => eprintln!("[window] {text}"),
        UiAction::Device { kind, id } => {
            eprintln!(
                "[ui] device {kind} {}",
                if id.is_empty() { "default" } else { "named" }
            );
        }
        UiAction::Tile { .. } => {}
    }
}

fn apply_group_media(
    engine: &mut EngineCore,
    roster: &mut Option<mpsc::Receiver<Vec<u32>>>,
    left: &mut Option<mpsc::Receiver<(u32, u8)>>,
    gone: &mut Option<mpsc::Receiver<String>>,
) {
    if let Some(rx) = roster {
        while let Ok(uids) = rx.try_recv() {
            engine.group_roster(&uids);
        }
    }
    if let Some(rx) = left {
        while let Ok((uid, _)) = rx.try_recv() {
            engine.group_member_left(uid, "zrtp");
        }
    }
    if let Some(rx) = gone {
        while let Ok(why) = rx.try_recv() {
            engine.group_ended(&why);
        }
    }
}

fn flush(engine: &mut EngineCore, recv: &mut TcpStream, key: &[u8], verbose: bool) {
    let frames: Vec<_> = std::mem::take(&mut engine.frames);
    for frame in frames {
        if let Err(e) = emit_frame(recv, &frame, key, verbose) {
            eprintln!("emit failed: {e}");
            break;
        }
    }
}

fn monotonic_secs() -> f64 {
    // Same clock as `tick_playout`. A private `Instant::now().elapsed()` stays
    // near zero and the mixer then treats every source as idle.
    zcall_engine::audio_bridge::monotonic_secs()
}

/// Runs the local audio self-test: start the mic and speaker, capture for a
/// few seconds, and report what came through. No Zalo connection needed.
fn run_selftest() -> i32 {
    let seconds: u64 = std::env::var("ZCALL_SELFTEST_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    eprintln!("selftest: starting audio bridge for {seconds}s");
    let mut bridge = AudioBridge::new();
    if let Err(e) = bridge.start(None, None, None, true) {
        eprintln!("selftest: FAIL — audio bridge: {e}");
        eprintln!("  hint: is parec/pacat (pipewire-pulse) installed?");
        return 1;
    }
    let start = Instant::now();
    let mut captured = 0_u64;
    let mut played = 0_u64;
    let mut opus_bytes = 0_u64;
    let mut failed = None;
    while start.elapsed() < Duration::from_secs(seconds) {
        while let Some(out) = bridge.next_audio_out() {
            captured += 1;
            opus_bytes += out.opus.len() as u64;
        }
        if let Err(e) = bridge.tick_playout() {
            failed = Some(e.to_string());
            break;
        }
        played += 1;
        std::thread::sleep(Duration::from_millis(20));
    }
    bridge.stop();
    match failed {
        Some(e) => {
            eprintln!("selftest: FAIL — playout: {e}");
            1
        }
        None if captured == 0 => {
            eprintln!("selftest: FAIL — no microphone frames ({played} playout ticks)");
            1
        }
        None => {
            eprintln!(
                "selftest: OK — {captured} mic frames ({opus_bytes} Opus bytes), {played} playout ticks"
            );
            0
        }
    }
}
