//! The real UDP media backend (`ZrtcMediaBackend`), implementing the
//! `Media` trait. It bridges the engine's control plane to a `MediaDriver`
//! (bound UDP socket) and handles InitZRTP, server selection, ping/status
//! timers, and audio frame send/receive over SRTP. P2P candidates that arrive
//! with the answer are applied in `start`. Video send accepts an already
//! encoded H.264 access unit. The group SFU extension is used for a group
//! camera; capturing the screen and encoding H.264 are outside this backend.

use crate::engine::Media;
use crate::media::{parse_host_port, Reply, Server};
use crate::runtime::MediaDriver;
use crate::video::VideoFrame;
use crate::zrtp::GroupInitLayout;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Initial InitZRTP wait, in ms.
const INIT_WAIT_MS: u64 = 600;
/// Delay between InitZRTP retries, in ms.
const INIT_RETRY_MS: u64 = 1000;
/// Maximum InitZRTP attempts.
const INIT_TRIES: u32 = 3;
/// Ping interval while the call is up, in ms.
const PING_INTERVAL_MS: u64 = 6_000;
/// ReqForward status interval, in ms.
const STATUS_INTERVAL_MS: u64 = 1_000;

/// A logger callback.
type LogFn = Box<dyn Fn(&str) + Send>;
/// A callback receiving decrypted Opus audio from the peer.
type AudioFn = Box<dyn FnMut(&[u8]) + Send>;
/// Decrypted Opus with the extended sequence number and the sender SSRC.
type AudioSeqFn = Box<dyn FnMut(u32, u32, &[u8]) + Send>;
/// One assembled H.264 access unit from the peer.
type VideoFn = Box<dyn FnMut(&VideoFrame) + Send>;
/// Room roster from ZRTP command 50.
type RosterFn = Box<dyn FnMut(&[u32]) + Send>;
/// A group member left. The byte is the ZRTP sub-command.
type MemberLeftFn = Box<dyn FnMut(u32, u8) + Send>;
/// We were removed from the group room.
type RemovedFn = Box<dyn FnMut(&str) + Send>;

/// The real media backend, for `--media zrtc`.
pub struct ZrtcMediaBackend {
    /// The UDP driver (bound socket, SRTP state, timers).
    driver: Option<MediaDriver>,
    /// Whether UDP packets actually leave the machine.
    send_udp: bool,
    /// The call parameters, received via `on_negotiated`.
    params: Value,
    /// The background ping/status thread handle.
    thread: Option<JoinHandle<()>>,
    /// Set to stop the background thread.
    stop_flag: Arc<AtomicBool>,
    /// The InitZRTP replies, for server selection.
    replies: Vec<Reply>,
    /// The active server after start.
    active_server: Option<Server>,
    /// The active server's token.
    server_token: u32,
    /// Audio rx counter. The receive callback (moved into the driver) shares it.
    rx: Arc<AtomicU64>,
    /// Audio frames that left this process.
    tx: AtomicU64,
    /// When the last audio packet was received.
    last_rx_at: Instant,
    /// The logger callback.
    log: Option<LogFn>,
    /// The peer-audio callback (receives decrypted Opus).
    on_audio: Option<AudioFn>,
    /// The peer-audio callback with sequence number and SSRC.
    on_audio_seq: Option<AudioSeqFn>,
    /// Assembled remote video.
    on_video: Option<VideoFn>,
    /// Group room roster.
    on_roster: Option<RosterFn>,
    /// A member left the group room.
    on_member_left: Option<MemberLeftFn>,
    /// We were removed from the group room.
    on_removed: Option<RemovedFn>,
    /// Whether media has started (for early-audio detection).
    started: bool,
    /// When media started, for RTCP clocks.
    pub started_at: Instant,
}

impl ZrtcMediaBackend {
    /// Builds a backend. `send_udp` controls whether UDP packets leave the machine
    /// (`--send-udp` / `ZCALL_SEND_UDP=1`).
    pub fn new(send_udp: bool) -> Self {
        Self {
            driver: None,
            send_udp,
            params: Value::Null,
            thread: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            replies: Vec::new(),
            active_server: None,
            server_token: 0,
            rx: Arc::new(AtomicU64::new(0)),
            tx: AtomicU64::new(0),
            last_rx_at: Instant::now(),
            log: None,
            on_audio: None,
            on_audio_seq: None,
            on_video: None,
            on_roster: None,
            on_member_left: None,
            on_removed: None,
            started: false,
            started_at: Instant::now(),
        }
    }

    /// Sets a logger callback.
    pub fn set_log<F>(&mut self, log: F)
    where
        F: Fn(&str) + Send + 'static,
    {
        self.log = Some(Box::new(log));
    }

    /// Sets the callback that receives decrypted Opus frames from the peer.
    pub fn set_on_audio<F>(&mut self, on_audio: F)
    where
        F: FnMut(&[u8]) + Send + 'static,
    {
        self.on_audio = Some(Box::new(on_audio));
    }

    /// Sets the callback that receives decrypted Opus with sequence and SSRC.
    pub fn set_on_audio_seq<F>(&mut self, on_audio_seq: F)
    where
        F: FnMut(u32, u32, &[u8]) + Send + 'static,
    {
        self.on_audio_seq = Some(Box::new(on_audio_seq));
    }

    /// Sets the callback that receives one assembled H.264 access unit.
    pub fn set_on_video<F>(&mut self, on_video: F)
    where
        F: FnMut(&VideoFrame) + Send + 'static,
    {
        self.on_video = Some(Box::new(on_video));
    }

    /// Receives the group room roster (ZRTP command 50).
    pub fn set_on_roster<F>(&mut self, on_roster: F)
    where
        F: FnMut(&[u32]) + Send + 'static,
    {
        self.on_roster = Some(Box::new(on_roster));
    }

    /// Receives a group member-left notice.
    pub fn set_on_member_left<F>(&mut self, on_member_left: F)
    where
        F: FnMut(u32, u8) + Send + 'static,
    {
        self.on_member_left = Some(Box::new(on_member_left));
    }

    /// Receives our removal from the group room.
    pub fn set_on_removed<F>(&mut self, on_removed: F)
    where
        F: FnMut(&str) + Send + 'static,
    {
        self.on_removed = Some(Box::new(on_removed));
    }

    fn log_msg(&self, msg: &str) {
        if let Some(log) = &self.log {
            log(msg);
        }
    }

    /// Returns the number of received audio frames.
    pub fn rx_count(&self) -> u64 {
        self.rx.load(Ordering::Relaxed)
    }

    /// Returns the number of audio frames handed to the socket.
    pub fn tx_count(&self) -> u64 {
        self.tx.load(Ordering::Relaxed)
    }

    /// Polls the UDP socket and delivers decrypted audio to the callback.
    /// Returns true if a packet was received.
    pub fn poll(&mut self) -> bool {
        let Some(driver) = self.driver.as_mut() else {
            return false;
        };
        driver.poll_once(Duration::from_millis(0))
    }

    /// Sends one Opus frame as SRTP over the relay.
    pub fn send_audio(&mut self, opus: &[u8], level: u8) {
        let Some(driver) = self.driver.as_mut() else {
            return;
        };
        if driver.send_audio_frame(opus, level) {
            self.tx.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Asks one sender for a video key frame (PLI), at most once a second.
    pub fn request_peer_key_frame(&mut self, _why: &str, src: Option<u32>) {
        let Some(driver) = self.driver.as_mut() else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        driver.media.request_peer_key_frame(src, now);
    }

    /// Sends one already-encoded H.264 access unit.
    pub fn send_video(&mut self, key: bool, annex_b: &[u8], screen: bool) {
        if let Some(driver) = self.driver.as_mut() {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            driver.media.send_video_frame(key, annex_b, screen, now);
        }
    }

    /// Asks the SFU to forward one member's video.
    pub fn watch_member(&mut self, uid: u32) {
        if let Some(driver) = self.driver.as_mut() {
            driver.media.watch_member(uid);
        }
    }

    /// Reports the rendered tile width in pixels for a group member so the SFU
    /// selects a matching spatial layer.
    pub fn set_member_render_width(&mut self, uid: u32, width: u32) {
        if let Some(driver) = self.driver.as_mut() {
            driver.media.set_member_render_width(uid, width);
        }
    }

    /// Returns the target camera encode profile for the current call.
    pub fn current_video_profile(&self) -> Option<crate::send_rate::Profile> {
        self.driver
            .as_ref()
            .map(|driver| driver.media.current_video_profile())
    }

    /// Consumes a pending inbound PLI/FIR key-frame request targeting us.
    pub fn take_key_frame_request(&mut self) -> bool {
        self.driver
            .as_mut()
            .is_some_and(|driver| driver.media.take_key_frame_request())
    }

    /// Latest packet loss percentage (`0..=100`) from TWCC/RR or audio downlink.
    pub fn observed_loss_perc(&self) -> Option<i32> {
        self.driver
            .as_ref()
            .and_then(|driver| driver.media.observed_loss_perc())
    }

    /// Drains queued paced 1-1 video packets; returns true if more remain.
    pub fn drain_video_queue(&mut self, max_packets: usize) -> bool {
        self.driver
            .as_mut()
            .is_some_and(|driver| driver.media.drain_video_queue(max_packets))
    }

    /// Records that this group call is sending a camera.
    pub fn set_group_camera(&mut self, on: bool) {
        if let Some(driver) = self.driver.as_mut() {
            driver.media.set_group_camera(on);
        }
    }

    /// Starts the screen-share SSRC (and dedicated UDP socket for group `ShareScreenPeer`).
    pub fn start_share(&mut self, share_id: u32) {
        if let Some(driver) = self.driver.as_mut() {
            driver.start_share(share_id);
        }
    }

    /// Stops the screen-share SSRC and closes any dedicated group share socket.
    pub fn stop_share(&mut self) {
        if let Some(driver) = self.driver.as_mut() {
            driver.stop_share();
        }
    }

    /// Consumes a pending keyframe request for the group screen-share encoder.
    pub fn take_share_key_frame_request(&mut self) -> bool {
        self.driver
            .as_mut()
            .is_some_and(|driver| driver.media.take_share_key_frame_request())
    }

    /// Reports the encoded screen-share dimensions to the group share peer.
    pub fn set_share_dimensions(&mut self, width: u16, height: u16) {
        if let Some(driver) = self.driver.as_mut() {
            driver.media.set_share_dimensions(width, height);
        }
    }

    /// Sends a ping to the active server.
    pub fn send_ping(&mut self) {
        if let Some(driver) = self.driver.as_mut() {
            driver.media.send_ping();
        }
    }

    /// Sends a ReqForward status to the active server.
    pub fn send_status(&mut self) {
        if let Some(driver) = self.driver.as_mut() {
            driver.media.send_status();
        }
    }

    /// Whether the call has started.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Our P2P candidates.
    ///
    /// Host addresses are this machine's IPv4 addresses (as `os.networkInterfaces`
    /// in `zrtc-media.js`). `127.0.0.1` is not one of them: the phone treats a
    /// type-0 candidate as somewhere it can send audio, and loopback is itself.
    fn local_candidates_impl(&self, public_addr: Option<&str>) -> Option<Vec<Value>> {
        let driver = self.driver.as_ref()?;
        let port = driver.local_addr().ok()?.port();
        let mut out = Vec::new();
        if port != 0 {
            let mut hosts = host_ipv4_addrs();
            if let Ok(addr) = driver.local_addr() {
                if let std::net::IpAddr::V4(ip) = addr.ip() {
                    let text = ip.to_string();
                    if usable_ipv4(&text) && !hosts.iter().any(|h| h == &text) {
                        hosts.insert(0, text);
                    }
                }
            }
            for ip in hosts {
                out.push(json!({"ip": ip, "port": port, "type": 0}));
            }
        }
        if let Some(pa) = public_addr {
            if let Some(pipe) = pa.find('|') {
                let ip = &pa[..pipe];
                if let Ok(p) = pa[pipe + 1..].parse::<u16>() {
                    if ip.parse::<std::net::Ipv4Addr>().is_ok() {
                        out.push(json!({"ip": ip, "port": p, "type": 1}));
                    }
                }
            }
        }
        if out.is_empty() {
            self.log_msg("media: no P2P candidate");
            None
        } else {
            let hosts = out
                .iter()
                .filter(|c| c.get("type").and_then(Value::as_u64) == Some(0))
                .count();
            self.log_msg(&format!(
                "media: candidates host={hosts} public={}",
                out.len() - hosts
            ));
            Some(out)
        }
    }

    /// Spawns the background thread that runs ping/status/media-timeout.
    #[allow(dead_code)]
    fn start_timer_thread(&mut self) {
        let stop = Arc::clone(&self.stop_flag);
        // We cannot share &mut self with the thread, so the caller must
        // drive the timers from the main loop via poll_and_tick().
        self.log_msg("media: timers driven by caller poll_and_tick");
        let _ = stop;
    }
}

impl Media for ZrtcMediaBackend {
    fn sends_udp(&self) -> bool {
        self.send_udp
    }

    fn local_candidates(&self, public_addr: Option<&str>) -> Option<Vec<Value>> {
        self.local_candidates_impl(public_addr)
    }

    fn on_negotiated(&mut self, params: &Value) {
        self.params = params.clone();
        if let Some(driver) = self.driver.as_mut() {
            driver.negotiate(params);
        }
    }

    fn prepare(&mut self, role: &str) -> Option<Value> {
        if !self.send_udp {
            self.log_msg("media: observe-only (no --send-udp), not contacting servers");
            return None;
        }
        let mut driver = match MediaDriver::bind() {
            Ok(d) => d,
            Err(e) => {
                self.log_msg(&format!("media: bind failed: {e}"));
                return None;
            }
        };
        // Wire the audio callback into the media session.
        driver.media.on_negotiated(&self.params);
        // Take the stored audio callbacks and wire them.
        if let Some(mut cb) = self.on_audio_seq.take() {
            let rx = Arc::clone(&self.rx);
            driver.media.set_on_audio_seq(move |seq, ssrc, opus| {
                rx.fetch_add(1, Ordering::Relaxed);
                cb(seq, ssrc, opus);
            });
        } else if let Some(mut cb) = self.on_audio.take() {
            let rx = Arc::clone(&self.rx);
            driver.media.set_on_audio_packet(move |opus, _keys| {
                rx.fetch_add(1, Ordering::Relaxed);
                cb(opus);
            });
        }
        if let Some(cb) = self.on_video.take() {
            driver.media.set_on_video(cb);
        }
        if let Some(cb) = self.on_roster.take() {
            driver.media.set_on_roster(cb);
        }
        if let Some(cb) = self.on_member_left.take() {
            driver.media.set_on_member_left(cb);
        }
        if let Some(cb) = self.on_removed.take() {
            driver.media.set_on_removed(cb);
        }
        self.log_msg(&format!(
            "media: bound to {}",
            driver
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_default()
        ));

        let servers = driver.media.servers.clone();
        if servers.is_empty() {
            self.log_msg("media: no servers in params");
            return None;
        }
        self.log_msg(&format!("media: InitZRTP to {} server(s)", servers.len()));

        if driver.media.group {
            let mut accepted = false;
            for layout in GroupInitLayout::ALL {
                driver.media.set_group_layout(layout);
                self.log_msg(&format!("media: group InitZRTP layout {}", layout.name()));
                let mut no_answer = false;
                for attempt in 0..INIT_TRIES {
                    driver.media.send_init_zrtp(role);
                    let deadline = Instant::now() + Duration::from_millis(INIT_WAIT_MS);
                    while Instant::now() < deadline && !driver.all_answered() {
                        driver.poll_once(Duration::from_millis(20));
                    }
                    if !driver.media.replies.is_empty() {
                        accepted = true;
                        self.log_msg(&format!(
                            "media: group InitZRTP accepted with layout {}",
                            layout.name()
                        ));
                        break;
                    }
                    if !driver.media.refusals.is_empty() {
                        self.log_msg(&format!(
                            "media: group InitZRTP layout {} refused",
                            layout.name()
                        ));
                        break;
                    }
                    if attempt + 1 < INIT_TRIES {
                        self.log_msg(&format!(
                            "media: InitZRTP retry {}/{}",
                            attempt + 1,
                            INIT_TRIES - 1
                        ));
                        thread::sleep(Duration::from_millis(INIT_RETRY_MS));
                    } else {
                        no_answer = true;
                    }
                }
                if accepted || no_answer {
                    break;
                }
            }
            if !accepted && driver.media.replies.is_empty() && driver.media.refusals.is_empty() {
                self.log_msg("media: no server answered InitZRTP");
                return None;
            }
        } else {
            for attempt in 0..INIT_TRIES {
                driver.media.send_init_zrtp(role);
                let deadline = Instant::now() + Duration::from_millis(INIT_WAIT_MS);
                while Instant::now() < deadline && !driver.all_answered() {
                    driver.poll_once(Duration::from_millis(20));
                }
                if !driver.media.replies.is_empty() || !driver.media.refusals.is_empty() {
                    break;
                }
                if attempt + 1 < INIT_TRIES {
                    self.log_msg(&format!(
                        "media: InitZRTP retry {}/{}",
                        attempt + 1,
                        INIT_TRIES - 1
                    ));
                    thread::sleep(Duration::from_millis(INIT_RETRY_MS));
                }
            }
        }
        if driver.media.replies.is_empty() {
            if !driver.media.refusals.is_empty() {
                self.log_msg("media: every server refused InitZRTP");
            } else {
                self.log_msg("media: no server answered InitZRTP");
            }
            return None;
        }
        let best = driver
            .media
            .replies
            .iter()
            .min_by_key(|r| r.rtt_ms)
            .cloned()?;
        self.log_msg(&format!(
            "media: {}/{} servers answered, best rtt {} ms",
            driver.media.replies.len(),
            servers.len(),
            best.rtt_ms
        ));
        let result = json!({
            "server": {"host": best.server.host, "port": best.server.port},
            "token": best.token,
            "rttMs": best.rtt_ms,
            "publicAddr": best.public_addr
        });
        self.replies = driver.media.replies.clone();
        self.driver = Some(driver);
        Some(result)
    }

    fn start(&mut self, role: &str, rtp_ser_ip: Option<&str>, opts: &Value) {
        // 1-1 reaches `start` without a prior `prepare`. Group calls prepare
        // themselves, so this only binds when the socket is still absent.
        if self.driver.is_none() {
            if !self.send_udp {
                self.log_msg(&format!("media: start role={role} (observe-only)"));
                return;
            }
            if self.prepare(role).is_none() {
                self.log_msg("media: prepare failed");
                return;
            }
        }
        let Some(driver) = self.driver.as_mut() else {
            self.log_msg("media: no driver in start");
            return;
        };
        if let Some(list) = opts.get("p2pCandidates").and_then(Value::as_array) {
            driver.media.adopt_p2p_candidates(list);
        }
        if !self.send_udp {
            self.log_msg(&format!("media: start role={role} (observe-only)"));
            return;
        }
        if let Some(ip) = rtp_ser_ip {
            if let Some(want) = parse_host_port(ip) {
                if let Some(matched) = self
                    .replies
                    .iter()
                    .find(|r| r.server.host == want.host && r.server.port == want.port)
                {
                    self.active_server = Some(matched.server.clone());
                    self.server_token = matched.token;
                }
            }
        }
        if self.active_server.is_none() {
            let best = self.replies.iter().min_by_key(|r| r.rtt_ms);
            if let Some(b) = best {
                self.active_server = Some(b.server.clone());
                self.server_token = b.token;
            }
        }
        if self.active_server.is_none() {
            self.log_msg("media: no server");
            return;
        }
        driver.start(role, rtp_ser_ip);
        self.last_rx_at = Instant::now();
        self.started_at = Instant::now();
        self.started = true;
        self.log_msg("media: started");
    }

    fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        if let Some(mut driver) = self.driver.take() {
            driver.stop();
        }
        self.active_server = None;
        self.replies.clear();
        self.started = false;
    }

    fn request_peer_key_frame(&mut self, why: &str, src: Option<u32>) {
        ZrtcMediaBackend::request_peer_key_frame(self, why, src);
    }

    fn watch_member(&mut self, uid: u32) {
        ZrtcMediaBackend::watch_member(self, uid);
    }

    fn set_group_camera(&mut self, on: bool) {
        ZrtcMediaBackend::set_group_camera(self, on);
    }

    fn stats(&self) -> Value {
        json!({
            "rx": self.rx.load(Ordering::Relaxed),
            "tx": self.tx.load(Ordering::Relaxed),
            "active": self.driver.as_ref().map(|d| d.media.active).unwrap_or(false)
        })
    }
}

/// Transport-CC feedback interval in milliseconds (matches ZaloCall: ~50 ms).
pub const TWCC_INTERVAL_MS: u64 = 50;
/// SR/RR + SDES report interval in milliseconds.
pub const REPORT_INTERVAL_MS: u64 = 1_000;

/// Runs one tick: poll UDP, send RTCP feedback, then fire the ping/status timers.
/// Call this from the main loop every ~5 ms.
pub fn poll_and_tick(
    backend: &mut ZrtcMediaBackend,
    last_ping: &mut Instant,
    last_status: &mut Instant,
    last_twcc: &mut Instant,
    last_report: &mut Instant,
) {
    // Poll incoming UDP (decrypted audio goes to the callback).
    let _had_packet = backend.poll();
    let notes = backend
        .driver
        .as_mut()
        .map(|driver| driver.media.take_notes())
        .unwrap_or_default();
    if let Some(log) = backend.log.as_ref() {
        for line in notes {
            log(&line);
        }
    }

    if !backend.is_started() {
        // Keep 1-1 relay NAT bindings and InitZRTP sessions alive while ringing
        // before `start()` activates media.
        let ringing_1to1 = backend
            .driver
            .as_ref()
            .is_some_and(|d| !d.media.group && !d.media.replies.is_empty());
        if ringing_1to1 {
            let now = Instant::now();
            if now.duration_since(*last_ping) >= Duration::from_millis(PING_INTERVAL_MS) {
                backend.send_ping();
                *last_ping = now;
            }
        }
        return;
    }
    if let Some(driver) = backend.driver.as_mut() {
        driver.media.send_fwd8_if_due();
        let _ = driver.media.drain_video_queue(16);
        driver.media.tick_share();
    }
    let now = Instant::now();
    let now_ms = now.duration_since(backend.started_at).as_millis() as u64;

    // RTCP: transport-cc every 50 ms, SR/RR every second.
    let reports = now.duration_since(*last_report) >= Duration::from_millis(REPORT_INTERVAL_MS);
    if reports || now.duration_since(*last_twcc) >= Duration::from_millis(TWCC_INTERVAL_MS) {
        if let Some(driver) = backend.driver.as_mut() {
            // SR/RR must use the same unix clock as `on_rtcp`. `now_ms` above
            // is only milliseconds since the socket opened; subtracting that
            // from a sender-report timestamp overflows.
            let report_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            for (msg_type, payload) in driver.media.rtcp_packets(report_ms, reports) {
                driver.media.send_rtcp(msg_type, &payload);
            }
            // Scheduled P2P NAT hole punching (fast during the first second, then slower).
            for (packet, candidate) in driver.media.p2p_packets(now_ms) {
                driver.send_raw(&packet, &candidate);
            }
        }
        if reports {
            *last_report = now;
            // Video bitrate ladder: recalculate every second (matching send-rate.js).
            if let Some(driver) = backend.driver.as_mut() {
                let sending = driver.media.is_sending_video();
                let down_loss = driver.media.downlink_loss();
                if let Some(profile) = driver.media.tick_send_rate(sending, down_loss, now_ms) {
                    if let Some(log) = backend.log.as_ref() {
                        log(&format!(
                            "send-rate: {}x{} {} fps {} kbps",
                            profile.width,
                            profile.height,
                            profile.fps,
                            profile.bitrate / 1000
                        ));
                    }
                }
            }
        }
        *last_twcc = now;
    }

    // Group keepalive is ZaviPing, about every 5 s, and it also refreshes
    // VidQual. 1-1 stays on the 6 s Ping plus the per-second status.
    let group = backend
        .driver
        .as_ref()
        .is_some_and(|driver| driver.media.group);
    let ping_every = Duration::from_millis(if group { 5_000 } else { PING_INTERVAL_MS });
    if now.duration_since(*last_ping) >= ping_every {
        backend.send_ping();
        *last_ping = now;
    }
    if !group && now.duration_since(*last_status) >= Duration::from_millis(STATUS_INTERVAL_MS) {
        backend.send_status();
        *last_status = now;
    }
}

/// A thread-safe wrapper around ¤ZrtcMediaBackend¤ that implements ¤Media¤.
///
/// ¤EngineCore¤ owns this through the trait object; the main loop clones the
/// inner ¤Arc¤ to drive audio polling and timers.
pub struct SharedZrtcBackend {
    inner: Arc<Mutex<ZrtcMediaBackend>>,
}

impl SharedZrtcBackend {
    /// Wraps an existing backend for sharing.
    pub fn new(backend: ZrtcMediaBackend) -> Self {
        Self {
            inner: Arc::new(Mutex::new(backend)),
        }
    }

    /// Returns a clone of the inner ¤Arc¤ for use outside ¤EngineCore¤.
    pub fn clone_inner(&self) -> Arc<Mutex<ZrtcMediaBackend>> {
        Arc::clone(&self.inner)
    }
}

impl Media for SharedZrtcBackend {
    fn sends_udp(&self) -> bool {
        self.inner.lock().unwrap().sends_udp()
    }

    fn local_candidates(&self, public_addr: Option<&str>) -> Option<Vec<Value>> {
        self.inner.lock().unwrap().local_candidates(public_addr)
    }

    fn on_negotiated(&mut self, params: &Value) {
        self.inner.lock().unwrap().on_negotiated(params)
    }

    fn prepare(&mut self, role: &str) -> Option<Value> {
        self.inner.lock().unwrap().prepare(role)
    }

    fn start(&mut self, role: &str, rtp_ser_ip: Option<&str>, opts: &Value) {
        self.inner.lock().unwrap().start(role, rtp_ser_ip, opts)
    }

    fn stop(&mut self) {
        self.inner.lock().unwrap().stop()
    }

    fn set_group_camera(&mut self, on: bool) {
        self.inner.lock().unwrap().set_group_camera(on);
    }

    fn watch_member(&mut self, uid: u32) {
        self.inner.lock().unwrap().watch_member(uid);
    }

    fn start_share(&mut self, share_id: u32) {
        self.inner.lock().unwrap().start_share(share_id);
    }

    fn stop_share(&mut self) {
        self.inner.lock().unwrap().stop_share();
    }

    fn send_video_frame(&mut self, key: bool, data: &[u8], screen: bool) {
        self.inner.lock().unwrap().send_video(key, data, screen);
    }

    fn request_peer_key_frame(&mut self, why: &str, src: Option<u32>) {
        self.inner.lock().unwrap().request_peer_key_frame(why, src);
    }

    fn stats(&self) -> Value {
        self.inner.lock().unwrap().stats()
    }
}

/// An IPv4 address the phone can try to reach. Loopback and link-local are not.
fn usable_ipv4(ip: &str) -> bool {
    let Ok(v) = ip.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    !v.is_loopback()
        && !v.is_unspecified()
        && !v.is_link_local()
        && !v.is_broadcast()
        && !v.is_multicast()
}

/// Host addresses marked `LOCAL` in `/proc/net/fib_trie`.
#[cfg(any(target_os = "linux", test))]
fn host_ipv4_from_fib(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("|--") {
            let ip = rest.trim();
            if ip.parse::<std::net::Ipv4Addr>().is_ok() {
                pending = Some(ip.to_owned());
            }
        } else if t.starts_with("/32 host LOCAL") {
            if let Some(ip) = pending.take() {
                if usable_ipv4(&ip) && !out.contains(&ip) {
                    out.push(ip);
                }
            }
        }
    }
    out
}

/// This machine's IPv4 addresses, for the 416 / 402 candidate list.
///
/// Uses cross-platform `if_addrs` (Linux, Windows, macOS), falling back to
/// `/proc/net/fib_trie` on Linux and finally the default-route UDP socket.
fn host_ipv4_addrs() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if let std::net::IpAddr::V4(ip) = iface.ip() {
                let text = ip.to_string();
                if usable_ipv4(&text) && !out.contains(&text) {
                    out.push(text);
                }
            }
        }
    }
    #[cfg(target_os = "linux")]
    if out.is_empty() {
        let text = std::fs::read_to_string("/proc/net/fib_trie").unwrap_or_default();
        out = host_ipv4_from_fib(&text);
    }
    if out.is_empty() {
        if let Some(ip) = default_route_ipv4() {
            out.push(ip);
        }
    }
    out
}

/// The address of the default route, when the fib trie has no host entries.
fn default_route_ipv4() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:53").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if usable_ipv4(&ip.to_string()) => Some(ip.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_and_prepares_without_send_udp() {
        let mut backend = ZrtcMediaBackend::new(false);
        backend.on_negotiated(&json!({
            "callId": 1,
            "localUid": 100,
            "peerUid": 200,
            "sessId": "x".repeat(40),
            "servers": [{"rtpaddr": "127.0.0.1:9000"}]
        }));
        assert!(backend.prepare("caller").is_none());
    }

    #[test]
    fn local_candidates_are_none_without_driver() {
        let backend = ZrtcMediaBackend::new(true);
        assert!(backend.local_candidates(None).is_none());
    }

    #[test]
    fn fib_trie_skips_loopback_and_link_local() {
        let text = "\
|-- 127.0.0.1
   /32 host LOCAL
|-- 169.254.1.1
   /32 host LOCAL
|-- 192.168.1.20
   /32 host LOCAL
|-- 10.0.0.5
   /32 host LOCAL
";
        assert_eq!(
            host_ipv4_from_fib(text),
            vec!["192.168.1.20".to_owned(), "10.0.0.5".to_owned()]
        );
    }

    #[test]
    fn stats_are_null_when_idle() {
        let backend = ZrtcMediaBackend::new(true);
        let s = backend.stats();
        assert_eq!(s["rx"], json!(0));
        assert_eq!(s["active"], json!(false));
    }

    #[test]
    fn poll_and_tick_pings_all_replied_relays_while_ringing() {
        let sent = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let rec = Arc::clone(&sent);

        let mut driver = MediaDriver::bind().expect("bind driver");
        driver.media.send_udp = true;
        driver.media.set_send(move |_pkt, srv, what| {
            rec.lock()
                .unwrap()
                .push((srv.host.clone(), what.to_owned()));
        });
        driver.media.on_negotiated(&json!({
            "callId": 42,
            "role": "caller",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "ring-tick-session-123456789012345",
            "servers": [
                {"rtpaddr": "192.0.2.10:5000"},
                {"rtpaddr": "192.0.2.11:5001"}
            ]
        }));
        driver.media.replies.push(crate::media::Reply {
            server: Server {
                host: "192.0.2.10".into(),
                port: 5000,
            },
            token: 101,
            rtt_ms: 10,
            public_addr: None,
        });
        driver.media.replies.push(crate::media::Reply {
            server: Server {
                host: "192.0.2.11".into(),
                port: 5001,
            },
            token: 202,
            rtt_ms: 20,
            public_addr: None,
        });
        driver.media.active_server = Some(Server {
            host: "192.0.2.10".into(),
            port: 5000,
        });

        let mut backend = ZrtcMediaBackend::new(true);
        backend.driver = Some(driver);
        assert!(!backend.is_started());

        let mut last_ping = Instant::now() - Duration::from_millis(PING_INTERVAL_MS + 100);
        let mut last_status = Instant::now();
        let mut last_twcc = Instant::now();
        let mut last_report = Instant::now();
        poll_and_tick(
            &mut backend,
            &mut last_ping,
            &mut last_status,
            &mut last_twcc,
            &mut last_report,
        );
        let pkts = sent.lock().unwrap();
        assert_eq!(pkts.len(), 2);
        assert_eq!(pkts[0], ("192.0.2.10".to_owned(), "Ping".to_owned()));
        assert_eq!(pkts[1], ("192.0.2.11".to_owned(), "Ping".to_owned()));
    }
}
