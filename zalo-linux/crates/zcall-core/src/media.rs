//! Media session ported from the reference call implementation
//! (`zrtc-media.js`).
//!
//! Covers the 1-1 relay path: negotiated servers, SRTP session keys, downlink
//! audio decryption (observe mode), and the deterministic send/select state
//! machine (InitZRTP, server reply, fastest-server selection, EndCall, Ping,
//! ReqForward status). The async timers (600 ms init wait, 6 s ping, 1 s
//! status) are left to the async runtime that will drive this struct.

use crate::p2p::{P2p, P2pAction};
use crate::rtp::{
    build_audio_rtp_header, build_group_audio_rtp_header, SfuLayer, AUDIO_PT, FRAME_SAMPLES,
};
use crate::send_rate::Profile;
use crate::srtp::{
    decrypt_unverified, master_from_sess_id, parse_rtp, protect_rtp, protect_rtp_auth,
    session_keys, unprotect_rtp, unprotect_rtp_auth, SessionKeys, SrtpAuthProfile, SrtpError,
};
use crate::video::{VideoAssembler, VideoFrame};
use crate::zrtp::{
    build_end_call_packet, build_group_init_zrtp_packet, build_group_vid_qual_config,
    build_group_vid_qual_req, build_init_zrtp_packet, build_ping_packet, build_req_forward_packet,
    build_zavi_ping_packet, req_forward_status, unwrap_packet, wrap_media_packet, Command,
    GroupInitLayout, MsgType, ParsedPacket, VidQualEntry, VidQualLayer,
};
use rand::Rng;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum video UDP packets sent in one burst (`zrtc-media.js` `VIDEO_BURST`).
/// A larger frame, including a group camera keyframe, is queued and drained
/// by the UDP tick. A single burst of a big picture is dropped by the SFU.
const VIDEO_BURST: usize = 8;

/// Electron `vSeq` / `vTsBase`: a non-zero clock, not a stream that starts at 0.
fn fresh_video_clock() -> (u16, u32) {
    let mut rng = rand::thread_rng();
    (rng.gen::<u16>() & 0x7fff, rng.gen::<u32>() & 0x7fff_ffff)
}

fn default_group_profile() -> Profile {
    Profile {
        width: 640,
        height: 360,
        fps: 15,
        bitrate: 500_000,
        key_ms: 5_000,
        capture_big: false,
        rung: 0,
    }
}

/// A callback receiving decrypted Opus plus the SRTP session.
type AudioCallback = Box<dyn FnMut(&[u8], &SessionKeys) + Send>;
/// Decrypted audio: extended sequence, SSRC, Opus payload.
type AudioSeqCallback = Box<dyn FnMut(u32, u32, &[u8]) + Send>;

/// A packet sender (the UDP transport, or a test recorder).
type SendHook = Box<dyn FnMut(&[u8], &Server, &str) + Send>;
/// Callback invoked when a complete video access unit is reassembled.
type VideoCallback = Box<dyn FnMut(&VideoFrame) + Send>;
/// UIDs currently in the group room (command 50).
type RosterCallback = Box<dyn FnMut(&[u32]) + Send>;
/// A member left. The byte is the ZRTP sub-command.
type MemberLeftCallback = Box<dyn FnMut(u32, u8) + Send>;
/// We were removed. The text is `kicked` or `host-ended`.
type RemovedCallback = Box<dyn FnMut(&str) + Send>;

/// A relay server host:port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Server {
    /// IPv4 host (or the literal IPv6 part, unbracketed).
    pub host: String,
    /// UDP port.
    pub port: u16,
}

/// Parses an "ip:port" address (IPv4, or a bracketed IPv6 literal) into a [`Server`].
pub fn parse_host_port(addr: &str) -> Option<Server> {
    if addr.is_empty() {
        return None;
    }
    if addr.starts_with('[') {
        let end = addr.find("]:")?;
        let host = &addr[1..end];
        let port = addr[end + 2..].parse::<u16>().ok()?;
        return Some(Server {
            host: host.to_owned(),
            port,
        });
    }
    let idx = addr.rfind(':')?;
    let host = &addr[..idx];
    let port = addr[idx + 1..].parse::<u16>().ok()?;
    if host.is_empty() {
        return None;
    }
    Some(Server {
        host: host.to_owned(),
        port,
    })
}

/// RFC 3711 §3.3.1 rollover-counter guess for received packets.
#[derive(Clone, Debug, Default)]
pub struct RocTracker {
    last: Option<u16>,
    roc: u32,
}

impl RocTracker {
    /// Guesses the rollover counter for a sequence number.
    pub fn guess(&self, seq: u16) -> u32 {
        let Some(last) = self.last else {
            return 0;
        };
        let last = i32::from(last);
        let seq = i32::from(seq);
        if last - seq > 0x8000 {
            self.roc.wrapping_add(1)
        } else if seq - last > 0x8000 && self.roc > 0 {
            self.roc.wrapping_sub(1)
        } else {
            self.roc
        }
    }

    /// Records the latest sequence number and rollover counter.
    pub fn update(&mut self, seq: u16, roc: u32) {
        let advanced = match self.last {
            None => true,
            Some(last) => {
                (u64::from(roc) << 16 | u64::from(seq))
                    > (u64::from(self.roc) << 16 | u64::from(last))
            }
        };
        if advanced {
            self.last = Some(seq);
            self.roc = roc;
        }
    }
}

/// A server's accepting InitZRTP reply.
#[derive(Clone, Debug)]
pub struct Reply {
    /// The server that answered.
    pub server: Server,
    /// Its media token.
    pub token: u32,
    /// Round-trip time in milliseconds.
    pub rtt_ms: u64,
    /// Our address as the server sees it ("ip|port").
    pub public_addr: Option<String>,
}

/// A media session (relay transport for 1-1 calls).
pub struct MediaSession {
    /// Relay servers parsed from the negotiated parameters (IPv4 only).
    pub servers: Vec<Server>,
    /// The chosen server, once media starts.
    pub active_server: Option<Server>,
    /// The active server's media token.
    pub server_token: u32,
    /// SRTP session keys, when the sessId is long enough.
    pub srtp_context: Option<SessionKeys>,
    /// Call-session master, tried once when `zrtcConfig.srtpKey` fails the MAC.
    /// Promoted into `srtp_context` on the first match. Never logged.
    alt_keys: Option<SessionKeys>,
    /// Latched SRTP profile for audio (`audSrtpNoSeq` / fallback).
    audio_auth: Option<SrtpAuthProfile>,
    /// Latched SRTP profile for video (`srtpNoSeq` / fallback).
    video_auth: Option<SrtpAuthProfile>,
    /// One-time unverified AES-CM probe flags for group diagnostics.
    audio_probe_noted: bool,
    video_probe_noted: bool,
    /// Whether this is a group call.
    pub group: bool,
    /// The negotiated parameters, kept for later slices.
    pub params: Value,
    /// Whether UDP packets leave this process.
    pub send_udp: bool,
    /// Whether media is active.
    pub active: bool,
    /// Accepting InitZRTP replies.
    pub replies: Vec<Reply>,
    /// InitZRTP refusal codes.
    pub refusals: Vec<u32>,
    local_uid: u32,
    peer_uid: u32,
    call_id: u32,
    sess_id: String,
    ended: Vec<String>,
    down_roc: RocTracker,
    init_sent_at: u64,
    fwd_seq: u32,
    /// Audio packets decrypted since the last ReqForward status.
    rx_second: u32,
    /// Sub 8 has been sent for this media start.
    fwd8_sent: bool,
    started_at: u64,
    seq: u16,
    ts: u32,
    tw_seq: u16,
    send_roc: u32,
    on_audio_packet: Option<AudioCallback>,
    on_audio_seq: Option<AudioSeqCallback>,
    send_hook: SendHook,
    /// RTCP feedback to send back to the phone.
    pub rtcp: crate::rtcp::RtcpFeedback,
    /// P2P NAT hole punching state, when the call has peer candidates.
    pub p2p: Option<P2p>,
    /// Reassembler for incoming video access units.
    video: VideoAssembler,
    video_roc: RocTracker,
    /// One audio ROC per group member. A shared counter rolls the wrong way
    /// when two SSRCs interleave, and SRTP then rejects every packet.
    down_rocs: BTreeMap<u32, RocTracker>,
    /// One video ROC per group member.
    video_rocs: BTreeMap<u32, RocTracker>,
    /// One assembler per group member. 1-1 uses `video`.
    member_video: BTreeMap<u32, VideoAssembler>,
    on_video: Option<VideoCallback>,
    /// Outgoing video bitrate ladder selector (when `zrtc_config` is present).
    pub send_rate: Option<crate::send_rate::SendRate>,
    video_seq: u16,
    /// ROC of outbound video, separate from the downlink tracker.
    video_send_roc: u32,
    /// RTP timestamp added to `(now - video_clock_start) * 90`.
    video_ts_base: u32,
    video_clock_start: u64,
    video_key_sent: bool,
    video_layer_seq: u16,
    /// The first group-camera packet of this call was logged once.
    video_tx_noted: bool,
    /// SSRC of the screen-sharing stream, or 0 when inactive.
    share_id: u32,
    share_on: bool,
    group_camera_on: bool,
    /// Group SFU with an empty `srtpKey` sends and accepts plain RTP.
    plain_media: bool,
    /// Byte length of the group `srtpKey`. Never the key itself.
    srtp_key_len: usize,
    /// Inbound UDP first bytes, so a quiet call shows whether media arrived.
    rx_types: BTreeMap<u8, u32>,
    /// How many distinct UDP types have already been logged.
    rx_type_kinds: usize,
    /// Inbound datagrams this call.
    rx_udp: u32,
    /// Group audio packets whose SRTP tag matched.
    rx_ok: u32,
    /// Group audio packets whose SRTP tag failed.
    rx_bad: u32,
    /// A downlink packet carried a 4-byte token before the RTP header.
    saw_token_prefix: bool,
    /// A short group-audio datagram was logged once. It is not an auth failure.
    short_noted: bool,
    /// The first group-video open result was logged once.
    video_open_noted: bool,
    /// Body layout of the group InitZRTP currently being tried.
    group_layout: GroupInitLayout,
    /// Members whose video we have asked the SFU to forward.
    vid_wanted: BTreeSet<u32>,
    /// Last quality id sent for each member. 0xff is never stored: that id
    /// tells the SFU to stop the member for good.
    vid_asked: BTreeMap<u32, u8>,
    /// Layer table from `zrtcConfig.groupcall`. Empty means ask for id 0.
    vid_layers: Vec<Value>,
    /// VidQualConfig has been logged once for this call.
    vid_cfg_logged: bool,
    /// Last camera bit written into a ZaviPing, so a change is logged once.
    zavi_video: Option<bool>,
    /// Sequence on group VidQual packets (cmd 32).
    ctl_seq: u32,
    /// Lines drained by the UDP tick into the call log.
    notes: Vec<String>,
    on_roster: Option<RosterCallback>,
    on_member_left: Option<MemberLeftCallback>,
    on_removed: Option<RemovedCallback>,
    /// Last 1-1 PLI, unix millis.
    last_pli_at: u64,
    /// Last PLI per group sender, unix millis.
    pli_at: BTreeMap<u32, u64>,
    /// Missing video sequence numbers, one stream per SSRC.
    nack: crate::rtcp::NackTracker,
    /// Last time a camera frame was queued for transmission, ms.
    last_video_tx_ms: u64,
    /// Previous audio reception counters for `_downlinkLoss()`.
    down_prev: Option<(u32, u32)>,
    /// Last reported UI render width (px) per remote group member UID.
    render_width: BTreeMap<u32, u32>,
    /// Active group uplink video profile (or 640x360 fallback).
    group_profile: Profile,
    /// Single spatial layer announced in group `VidQualConfig` when negotiated.
    group_send_layer: Option<VidQualLayer>,
    /// Group SFU RTP header extension setting (`codec` 60/61 -> id 12).
    group_sfu: SfuLayer,
    /// Paced video packets waiting to drain in `VIDEO_BURST` batches.
    v_out: VecDeque<(Vec<u8>, Server, &'static str)>,
}

impl Default for MediaSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Payload type, padding bit, header length, and the first 16 header bytes.
/// The payload and the key are not included.
fn rtp_shape(rtp: &[u8]) -> String {
    let pt = rtp.get(1).copied().unwrap_or(0) & 0x7f;
    let pad = u8::from(rtp.first().is_some_and(|byte| byte & 0x20 != 0));
    let header_len = parse_rtp(rtp).map(|header| header.header_len).unwrap_or(0);
    let shown = rtp.len().min(16);
    let mut hex = String::with_capacity(shown * 2);
    for byte in &rtp[..shown] {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("pt {pt} pad {pad} header {header_len} hex {hex}")
}

fn config_flag(config: &Value, key: &str) -> Option<bool> {
    let value = config.get(key)?;
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(num) => num
            .as_i64()
            .map(|n| n != 0)
            .or_else(|| num.as_u64().map(|n| n != 0))
            .or_else(|| num.as_f64().map(|n| n != 0.0)),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

impl MediaSession {
    /// Builds a session with no audio callback and a no-op sender.
    pub fn new() -> Self {
        Self {
            servers: Vec::new(),
            active_server: None,
            server_token: 0,
            srtp_context: None,
            alt_keys: None,
            audio_auth: None,
            video_auth: None,
            audio_probe_noted: false,
            video_probe_noted: false,
            group: false,
            params: Value::Null,
            send_udp: false,
            active: false,
            replies: Vec::new(),
            refusals: Vec::new(),
            local_uid: 0,
            peer_uid: 0,
            call_id: 0,
            sess_id: String::new(),
            ended: Vec::new(),
            down_roc: RocTracker::default(),
            init_sent_at: 0,
            fwd_seq: 0,
            rx_second: 0,
            fwd8_sent: false,
            started_at: 0,
            seq: 0,
            ts: 0,
            tw_seq: 1,
            send_roc: 0,
            on_audio_packet: None,
            on_audio_seq: None,
            send_hook: Box::new(|_, _, _| {}),
            rtcp: crate::rtcp::RtcpFeedback::new(0),
            p2p: None,
            video: VideoAssembler::new(false),
            video_roc: RocTracker::default(),
            down_rocs: BTreeMap::new(),
            video_rocs: BTreeMap::new(),
            member_video: BTreeMap::new(),
            on_video: None,
            send_rate: None,
            video_seq: 0,
            video_send_roc: 0,
            video_ts_base: 0,
            video_clock_start: 0,
            video_key_sent: false,
            video_layer_seq: 0,
            video_tx_noted: false,
            share_id: 0,
            share_on: false,
            group_camera_on: false,
            plain_media: false,
            srtp_key_len: 0,
            rx_types: BTreeMap::new(),
            rx_type_kinds: 0,
            rx_udp: 0,
            rx_ok: 0,
            rx_bad: 0,
            saw_token_prefix: false,
            short_noted: false,
            video_open_noted: false,
            group_layout: GroupInitLayout::Zero,
            vid_wanted: BTreeSet::new(),
            vid_asked: BTreeMap::new(),
            vid_layers: Vec::new(),
            vid_cfg_logged: false,
            zavi_video: None,
            ctl_seq: 0,
            notes: Vec::new(),
            on_roster: None,
            on_member_left: None,
            on_removed: None,
            last_pli_at: 0,
            pli_at: BTreeMap::new(),
            nack: crate::rtcp::NackTracker::default(),
            last_video_tx_ms: 0,
            down_prev: None,
            render_width: BTreeMap::new(),
            group_profile: default_group_profile(),
            group_send_layer: None,
            group_sfu: SfuLayer {
                id: 12,
                long: false,
            },
            v_out: VecDeque::new(),
        }
    }

    /// Builds a session that hands decrypted audio to `on_audio_packet`.
    pub fn with_on_audio_packet<F>(on_audio_packet: F) -> Self
    where
        F: FnMut(&[u8], &SessionKeys) + Send + 'static,
    {
        let mut session = Self::new();
        session.on_audio_packet = Some(Box::new(on_audio_packet));
        session
    }

    /// Replaces the packet sender (used by tests and the UDP transport).
    pub fn set_send<F>(&mut self, send_hook: F)
    where
        F: FnMut(&[u8], &Server, &str) + Send + 'static,
    {
        self.send_hook = Box::new(send_hook);
    }

    /// Sets the callback that receives decrypted audio frames.
    pub fn set_on_audio_packet<F>(&mut self, callback: F)
    where
        F: FnMut(&[u8], &SessionKeys) + Send + 'static,
    {
        self.on_audio_packet = Some(Box::new(callback));
    }

    /// Sets the callback that receives decrypted audio with sequence and SSRC.
    pub fn set_on_audio_seq<F>(&mut self, callback: F)
    where
        F: FnMut(u32, u32, &[u8]) + Send + 'static,
    {
        self.on_audio_seq = Some(Box::new(callback));
    }

    /// Sets the callback invoked for each reassembled video access unit.
    pub fn set_on_video<F>(&mut self, callback: F)
    where
        F: FnMut(&VideoFrame) + Send + 'static,
    {
        self.on_video = Some(Box::new(callback));
    }

    /// Receives the room roster from ZRTP command 50.
    pub fn set_on_roster<F>(&mut self, callback: F)
    where
        F: FnMut(&[u32]) + Send + 'static,
    {
        self.on_roster = Some(Box::new(callback));
    }

    /// Receives a member-left notice (group EndCall, header UID is the member).
    pub fn set_on_member_left<F>(&mut self, callback: F)
    where
        F: FnMut(u32, u8) + Send + 'static,
    {
        self.on_member_left = Some(Box::new(callback));
    }

    /// Receives our own removal. `why` is `kicked` or `host-ended`.
    pub fn set_on_removed<F>(&mut self, callback: F)
    where
        F: FnMut(&str) + Send + 'static,
    {
        self.on_removed = Some(Box::new(callback));
    }

    /// Advances one second and updates the outgoing video bitrate ladder.
    ///
    /// `down_loss` is the fraction of peer packets we failed to receive (`0..1`).
    pub fn tick_send_rate(
        &mut self,
        sending: bool,
        down_loss: Option<f64>,
        now_ms: u64,
    ) -> Option<crate::send_rate::Profile> {
        let sr = self.send_rate.as_mut()?;
        let tick = sr.tick(sending, down_loss, now_ms);
        tick.profile
    }

    /// Current target encode profile (dynamic 1-1 ladder or negotiated group layer).
    pub fn current_video_profile(&self) -> Profile {
        if self.group {
            self.group_profile
        } else {
            self.send_rate
                .as_ref()
                .map_or_else(default_group_profile, |sr| sr.profile())
        }
    }

    /// Whether camera video has been transmitted within the last 1.5 s.
    pub fn is_sending_video(&self) -> bool {
        self.last_video_tx_ms != 0 && now_ms().saturating_sub(self.last_video_tx_ms) < 1_500
    }

    /// Share of the peer's audio packets lost since the previous check (0..=1).
    pub fn downlink_loss(&mut self) -> Option<f64> {
        let (expected, received) = self.rtcp.audio_reception_counts()?;
        let (prev_exp, prev_rec) = self.down_prev.replace((expected, received))?;
        let exp = expected.saturating_sub(prev_exp);
        let rec = received.saturating_sub(prev_rec);
        if exp < 10 {
            return None;
        }
        let lost = exp.saturating_sub(rec);
        Some((f64::from(lost) / f64::from(exp)).clamp(0.0, 1.0))
    }

    /// Consumes a pending inbound PLI/FIR key-frame request targeting us.
    pub fn take_key_frame_request(&mut self) -> bool {
        self.rtcp.take_key_frame_request()
    }

    /// Latest packet loss percentage (`0..=100`) observed from TWCC/RR/downlink.
    pub fn observed_loss_perc(&self) -> Option<i32> {
        let loss = self.send_rate.as_ref().and_then(|sr| sr.observed_loss())?;
        Some((loss * 100.0).round().clamp(0.0, 100.0) as i32)
    }

    /// Reports the rendered tile width in pixels for one group member so the SFU
    /// upgrades or downgrades their spatial layer accordingly.
    pub fn set_member_render_width(&mut self, uid: u32, width: u32) {
        if !self.group || uid == 0 {
            return;
        }
        if self.render_width.get(&uid).copied() == Some(width) {
            return;
        }
        self.render_width.insert(uid, width);
        if self.vid_wanted.contains(&uid)
            && self.vid_asked.get(&uid).copied() != Some(self.quality_id_for(uid))
        {
            self.send_vid_qual();
        }
    }

    /// Drains up to `max_packets` queued 1-1 paced video packets. Returns true
    /// when more paced packets remain.
    pub fn drain_video_queue(&mut self, max_packets: usize) -> bool {
        let mut count = 0usize;
        while count < max_packets {
            let Some((packet, server, label)) = self.v_out.pop_front() else {
                break;
            };
            self.send(&packet, &server, label);
            count += 1;
        }
        !self.v_out.is_empty()
    }

    /// Attaches P2P candidates that arrive after initial negotiation.
    ///
    /// The caller only receives the callee's candidates in the answer. The JS
    /// `start()` copies `opts.p2pCandidates` into params before NAT punching.
    pub fn adopt_p2p_candidates(&mut self, list: &[Value]) {
        if self.group {
            return;
        }
        let candidates = ipv4_servers(list);
        if candidates.is_empty() {
            return;
        }
        let callee = self.params.get("role").and_then(Value::as_str) == Some("callee");
        self.p2p = Some(P2p::new(callee, self.call_id, candidates, now_ms()));
        if let Some(obj) = self.params.as_object_mut() {
            obj.insert("p2pCandidates".to_owned(), Value::Array(list.to_vec()));
        }
    }

    /// Toggles the camera state in a group call.
    ///
    /// The next ZaviPing must carry the new camera bit. Waiting for the 5 s
    /// tick left the SFU thinking the camera was off and it stopped forwarding.
    pub fn set_group_camera(&mut self, on: bool) {
        if self.group_camera_on == on {
            return;
        }
        self.group_camera_on = on;
        if self.group && self.active && self.active_server.is_some() {
            self.send_ping();
        }
    }

    /// Lines the UDP tick should print. Cleared after this call.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Picks the next group InitZRTP layout. 1-1 ignores it.
    pub fn set_group_layout(&mut self, layout: GroupInitLayout) {
        self.group_layout = layout;
    }

    fn note(&mut self, line: impl Into<String>) {
        if self.notes.len() < 64 {
            self.notes.push(line.into());
        }
    }

    /// Starts the screen-sharing stream with SSRC `share_id`.
    pub fn start_share(&mut self, share_id: u32) {
        self.share_id = share_id;
        self.share_on = share_id > 0;
    }

    /// Stops the screen-sharing stream.
    pub fn stop_share(&mut self) {
        self.share_on = false;
    }

    /// Sends one H.264 Annex-B access unit.
    ///
    /// Uses PT 98 for key frames and PT 97 for delta frames, with the marker bit
    /// on the last packet. Group calls add the SFU header while the camera is on;
    /// raw payload is only sent when `srtpKey` is absent. No encoding happens here:
    /// `annex_b` is already compressed.
    pub fn send_video_frame(&mut self, key: bool, annex_b: &[u8], screen: bool, now: u64) {
        if !self.active || self.active_server.is_none() || annex_b.is_empty() {
            return;
        }
        if screen {
            if !self.share_on || self.share_id == 0 {
                return;
            }
        } else {
            self.last_video_tx_ms = now_ms();
            if !key && !self.video_key_sent {
                return;
            }
        }
        if key {
            self.video_key_sent = true;
        }
        if self.video_clock_start == 0 {
            self.video_clock_start = now;
        }
        let elapsed = now.saturating_sub(self.video_clock_start);
        let ts = self
            .video_ts_base
            .wrapping_add((elapsed.saturating_mul(90)) as u32);
        let ssrc = if screen {
            self.share_id
        } else {
            self.local_uid
        };
        let pt = if key {
            crate::video::KEY_PT
        } else {
            crate::video::DELTA_PT
        };
        let chunks = crate::video::packetize_frame(annex_b, crate::video::MAX_CHUNK);
        // Group SFU video (camera or screen share) always carries the SFU
        // extension header (`id: 12`); `plain_media` only skips SRTP when
        // `srtpKey` is empty.
        let group_sfu = self.group && (screen || self.group_camera_on);
        // Screen share stays in one send: its own socket. Camera frames,
        // group included, are paced so a keyframe is not one UDP burst.
        let pace = !screen && (chunks.len() > VIDEO_BURST || !self.v_out.is_empty());
        let tx_now = now_ms();
        let last = chunks.len().saturating_sub(1);
        for (index, chunk) in chunks.iter().enumerate() {
            let marker = index == last;
            self.video_layer_seq = self.video_layer_seq.wrapping_add(1);
            if let Some(sr) = self.send_rate.as_mut() {
                sr.on_sent(self.tw_seq, tx_now);
            }
            let header = if group_sfu {
                crate::rtp::build_group_video_rtp_header(
                    self.video_seq,
                    ts,
                    ssrc,
                    self.tw_seq,
                    marker,
                    pt,
                    self.group_sfu,
                    self.video_layer_seq,
                    now,
                    now,
                )
            } else {
                crate::rtp::build_audio_rtp_header(
                    self.video_seq,
                    ts,
                    ssrc,
                    self.tw_seq,
                    marker,
                    pt,
                )
            };
            let rtp = if self.plain_media {
                let mut raw = header;
                raw.extend_from_slice(chunk);
                raw
            } else {
                let Some(keys) = self.srtp_context.clone() else {
                    return;
                };
                match self.protect_uplink(
                    &header,
                    chunk,
                    &keys,
                    self.video_seq,
                    ssrc,
                    self.video_send_roc,
                    false,
                ) {
                    Ok(packet) => packet,
                    Err(_) => return,
                }
            };
            let server = match self.active_server.clone() {
                Some(server) => server,
                None => return,
            };
            let label = if screen { "share" } else { "video" };
            if index == 0 && self.group && !screen && !self.video_tx_noted {
                self.video_tx_noted = true;
                let head: String = chunk
                    .iter()
                    .take(8)
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                let no_seq = self.video_auth == Some(SrtpAuthProfile::NullAuthNoSeq);
                self.note(format!(
                    "media: group video out plain {} no_seq {} sfu {} seq {} ts {} payload {head}",
                    u8::from(self.plain_media),
                    u8::from(no_seq),
                    u8::from(group_sfu),
                    self.video_seq,
                    ts
                ));
            }
            let packet = wrap_media_packet(MsgType::VIDEO_RTP, self.server_token, &rtp);
            let p2p_pkt = if !self.group {
                self.p2p.as_ref().and_then(|p2p| {
                    p2p.path()
                        .cloned()
                        .and_then(|path| p2p.wrap_video(&rtp).map(|w| (w, path)))
                })
            } else {
                None
            };
            if pace {
                self.v_out.push_back((packet, server, label));
                if let Some((w, path)) = p2p_pkt {
                    self.v_out.push_back((w, path, "P2P video"));
                }
            } else {
                self.send(&packet, &server, label);
                if let Some((w, path)) = p2p_pkt {
                    self.send(&w, &path, "P2P video");
                }
            }
            self.video_seq = self.video_seq.wrapping_add(1);
            if self.video_seq == 0 {
                self.video_send_roc = self.video_send_roc.wrapping_add(1);
            }
            self.tw_seq = self.tw_seq.wrapping_add(1);
        }
        if pace {
            self.drain_video_queue(VIDEO_BURST);
        }
    }

    /// Returns P2P NAT hole-punching packets due at `now_ms`.
    pub fn p2p_packets(&mut self, now_ms: u64) -> Vec<(Vec<u8>, Server)> {
        if self.group {
            return Vec::new();
        }
        match self.p2p.as_mut() {
            Some(p2p) => p2p.poll(now_ms),
            None => Vec::new(),
        }
    }

    /// Returns the established P2P peer path, if any.
    pub fn p2p_path(&self) -> Option<&Server> {
        self.p2p.as_ref().and_then(|p| p.path())
    }

    /// Asks one sender for a key frame. At most one PLI a second per sender.
    ///
    /// `src` is the member in a group call. Without it, 1-1 uses the SSRC of
    /// the first video packet. Returns whether a PLI was sent.
    pub fn request_peer_key_frame(&mut self, src: Option<u32>, now: u64) -> bool {
        if !self.active || self.active_server.is_none() {
            return false;
        }
        let key = src.filter(|id| *id > 0);
        let stamp = match key {
            Some(id) => self.pli_at.get(&id).copied().unwrap_or(0),
            None => self.last_pli_at,
        };
        if now.saturating_sub(stamp) < 1_000 {
            return false;
        }
        let pli = if let Some(id) = key {
            self.rtcp.counters.pli += 1;
            crate::rtcp::build_pli(self.rtcp.local_ssrc, id)
        } else {
            match self.rtcp.pli() {
                Some(packet) => packet,
                None => return false,
            }
        };
        if let Some(id) = key {
            self.pli_at.insert(id, now);
        } else {
            self.last_pli_at = now;
        }
        self.send_rtcp(MsgType::VIDEO_FEC, &pli);
        true
    }

    /// Asks the sender to resend video packets that never arrived.
    fn nack_video(&mut self, ssrc: u32, seq: u16, now: u64) {
        if self.active_server.is_none() {
            let _ = self.nack.note(ssrc, seq, now);
            return;
        }
        for (media_ssrc, seqs) in self.nack.note(ssrc, seq, now) {
            if seqs.is_empty() {
                continue;
            }
            let packet = crate::rtcp::build_nack(self.rtcp.local_ssrc, media_ssrc, &seqs);
            self.rtcp.counters.nack += 1;
            self.send_rtcp(MsgType::VIDEO_FEC, &packet);
        }
    }

    /// Sends a wrapped RTCP packet over the active relay server (and P2P if up).
    pub fn send_rtcp(&mut self, msg_type: u8, payload: &[u8]) {
        let Some(server) = self.active_server.clone() else {
            return;
        };
        let packet = wrap_media_packet(msg_type, self.server_token, payload);
        self.send(&packet, &server, "rtcp");
        if !self.group {
            let p2p = self.p2p.as_ref().and_then(|p| {
                p.path().cloned().and_then(|dst| {
                    p.wrap_rtcp(msg_type == MsgType::VIDEO_FEC, payload)
                        .map(|pkt| (pkt, dst))
                })
            });
            if let Some((pkt, dst)) = p2p {
                self.send(&pkt, &dst, "P2P RTCP");
            }
        }
    }

    /// Builds RTCP packets due at `now`: transport-cc feedback and SR/RR reports.
    ///
    /// Returns `(msg_type, payload)` pairs for the UDP layer to wrap and send.
    pub fn rtcp_packets(&mut self, now: u64, reports: bool) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        for pkt in self.rtcp.twcc_packets() {
            out.push((MsgType::AUDIO_FEC, pkt));
        }
        // A group call has one stream per member. The SFU wants transport-cc
        // only; a 1-1 sender report does not describe those streams.
        if reports && !self.group {
            out.push((
                MsgType::AUDIO_FEC,
                self.rtcp.audio_report(now, self.ts, self.seq.into(), 0),
            ));
            if let Some(video) = self.rtcp.video_report(now) {
                out.push((MsgType::VIDEO_FEC, video));
            }
        }
        out
    }

    /// Consumes the negotiated parameters and derives the SRTP session.
    pub fn on_negotiated(&mut self, params: &Value) {
        self.params = params.clone();
        self.group = params
            .get("group")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.video = VideoAssembler::new(self.group);
        self.member_video.clear();
        self.down_roc = RocTracker::default();
        self.video_roc = RocTracker::default();
        self.down_rocs.clear();
        self.video_rocs.clear();
        self.local_uid = num(params, "localUid").unwrap_or(0) as u32;
        self.peer_uid = num(params, "peerUid").unwrap_or(0) as u32;
        self.call_id = num(params, "callId").unwrap_or(0) as u32;
        self.rtcp = crate::rtcp::RtcpFeedback::new(self.local_uid);
        // 1-1 climbs zrtc_config. A group call encodes one fixed layer and
        // tells the SFU about that layer; the 1-1 ladder does not apply.
        let config = params.get("config").cloned().unwrap_or(Value::Null);
        self.send_rate = if self.group {
            None
        } else {
            crate::send_rate::SendRate::new_1v1(self.local_uid, &config)
                .ok()
                .map(|(sr, _profile)| sr)
        };
        // P2P: punch NAT toward the peer candidates sent in the offer/answer.
        let list = params
            .get("p2pCandidates")
            .or_else(|| params.get("p2p_candidates"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.p2p = None;
        self.plain_media = false;
        self.vid_wanted.clear();
        self.vid_asked.clear();
        self.vid_layers.clear();
        self.render_width.clear();
        self.last_video_tx_ms = 0;
        self.down_prev = None;
        self.v_out.clear();
        self.group_profile = default_group_profile();
        self.group_send_layer = None;
        self.group_sfu = SfuLayer {
            id: 12,
            long: false,
        };
        if self.group {
            let plan = group_video_plan(&config);
            self.vid_layers = plan.layers;
            self.group_profile = plan.encode;
            self.group_send_layer = plan.send_layer;
            self.group_sfu = plan.sfu;
        }
        self.vid_cfg_logged = false;
        self.zavi_video = None;
        self.ctl_seq = 0;
        self.notes.clear();
        self.rx_types.clear();
        self.rx_type_kinds = 0;
        self.rx_udp = 0;
        self.rx_ok = 0;
        self.rx_bad = 0;
        self.saw_token_prefix = false;
        self.short_noted = false;
        self.video_open_noted = false;
        self.audio_probe_noted = false;
        self.video_probe_noted = false;
        self.srtp_key_len = 0;
        self.group_layout = GroupInitLayout::Zero;
        if !self.group {
            self.adopt_p2p_candidates(&list);
        }
        self.last_pli_at = 0;
        self.pli_at.clear();
        self.nack = crate::rtcp::NackTracker::default();
        self.sess_id = str(params, "sessId");
        self.servers.clear();
        self.replies.clear();
        self.refusals.clear();
        self.ended.clear();
        self.active_server = None;
        self.server_token = 0;
        self.srtp_context = None;
        self.alt_keys = None;
        self.audio_auth = None;
        self.video_auth = None;
        self.seq = 0;
        self.ts = 0;
        self.tw_seq = 1;
        self.send_roc = 0;
        self.rx_second = 0;
        self.fwd8_sent = false;
        let (video_seq, video_ts_base) = fresh_video_clock();
        self.video_seq = video_seq;
        self.video_send_roc = 0;
        self.video_ts_base = video_ts_base;
        self.video_clock_start = 0;
        self.video_key_sent = false;
        self.video_layer_seq = 0;
        self.video_tx_noted = false;
        // The first ZaviPing is sent from `start`, before the camera thread
        // exists. A 0 there makes the SFU stop forwarding this camera.
        self.group_camera_on = self.group
            && params
                .get("video")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let list = params
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for server in list {
            let addr = server
                .get("rtpaddr")
                .or_else(|| server.get("rtp"))
                .or_else(|| server.get("rtpAddress"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(parsed) = parse_host_port(addr) {
                self.servers.push(parsed);
            }
        }
        let group_key = params
            .get("config")
            .and_then(|c| c.get("srtpKey"))
            .and_then(Value::as_str)
            .unwrap_or("");
        self.srtp_key_len = group_key.len();
        if self.group && group_key.is_empty() {
            // GroupZRtcConfig keeps only srtpKey. Empty means plain RTP, the
            // same path the macOS client takes when createAndInitSRTP fails.
            self.plain_media = true;
        }
        let sess = if self.group {
            group_key
        } else {
            self.sess_id.as_str()
        };
        if sess.len() >= 30 {
            if let Ok(master) = master_from_sess_id(sess.as_bytes()) {
                self.srtp_context = session_keys(&master).ok();
            }
        }
        if self.group && self.srtp_context.is_none() {
            // A key shorter than 30 bytes cannot build session keys. The macOS
            // client then sends and reads plain RTP.
            self.plain_media = true;
        }
        // Electron negotiate uses srtpKey. A later comment assumes the group
        // master is sessId[:30] and the same SrtpTransport. Keep srtpKey as
        // the primary, and try the call session only when the first 30 bytes
        // differ. An empty key stays on the plain path above.
        let session_same = self.group
            && group_key.len() >= 30
            && self.sess_id.len() >= 30
            && group_key.as_bytes()[..30] == self.sess_id.as_bytes()[..30];
        if self.group && !self.plain_media && self.sess_id.len() >= 30 && !session_same {
            if let Ok(master) = master_from_sess_id(self.sess_id.as_bytes()) {
                self.alt_keys = session_keys(&master).ok();
            }
        }
        let (video_no_seq, audio_no_seq) = if self.group {
            // GroupZRtcConfig defaults srtpNoSeq and audSrtpNoSeq to 1; when present,
            // audSrtpNoSeq falls back to srtpNoSeq if omitted.
            let v = config_flag(&config, "srtpNoSeq").unwrap_or(true);
            let a = config_flag(&config, "audSrtpNoSeq").unwrap_or(v);
            (v, a)
        } else {
            let v = config_flag(&config, "srtpNoSeq").unwrap_or(false);
            (v, v)
        };
        if !self.plain_media {
            self.video_auth = video_no_seq.then_some(SrtpAuthProfile::NullAuthNoSeq);
            self.audio_auth = audio_no_seq.then_some(SrtpAuthProfile::NullAuthNoSeq);
        }
        if self.group {
            let mode = self
                .params
                .get("srtpMode")
                .or_else(|| config.get("srtpMode"))
                .map(|v| match v {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) => s.clone(),
                    _ => v.to_string(),
                })
                .unwrap_or_else(|| "none".to_owned());
            let mut key_names: Vec<&str> = config
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            key_names.sort_unstable();
            self.note(format!(
                "media: group srtp key {} bytes, plain {}, session {} bytes, same {}, mode {mode}, no_seq v={} a={}, config keys [{}]",
                self.srtp_key_len,
                u8::from(self.plain_media),
                self.sess_id.len(),
                u8::from(session_same),
                u8::from(video_no_seq),
                u8::from(audio_no_seq),
                key_names.join(",")
            ));
        }
    }

    /// Consumes one UDP datagram from a server.
    pub fn receive_packet(&mut self, buf: &[u8], source: Option<&Server>) {
        self.note_udp(buf);
        let Some(parsed) = unwrap_packet(buf) else {
            return;
        };
        // P2P: direct packet from the peer (7f | u16 dir | u32 token | u8 kind | 00 | payload).
        if parsed.msg_type == MsgType::P2P_EXT {
            if let Some(from) = source {
                self.on_p2p(buf, from);
            }
            return;
        }
        if parsed.is_media && parsed.msg_type == MsgType::VIDEO_DOWN {
            self.on_video_packet(&parsed.payload);
        } else if parsed.is_media && parsed.msg_type == MsgType::AUDIO_RTP_DOWN {
            self.rtcp.on_rtp(false, &parsed.payload, now_ms());
            self.on_audio(&parsed.payload);
        } else if parsed.msg_type == MsgType::AUDIO_FEC || parsed.msg_type == MsgType::AUDIO_RTCP {
            self.rtcp.on_rtcp(false, &parsed.payload, now_ms());
            if let Some(sr) = self.send_rate.as_mut() {
                sr.on_rtcp("audio", &parsed.payload);
            }
        } else if parsed.msg_type == MsgType::VIDEO_FEC {
            self.rtcp.on_rtcp(true, &parsed.payload, now_ms());
            if let Some(sr) = self.send_rate.as_mut() {
                sr.on_rtcp("video", &parsed.payload);
            }
        } else if parsed.msg_type == MsgType::SERVER_REPLY
            && (parsed.cmd == Some(Command::INIT_CALL_CALLER)
                || parsed.cmd == Some(Command::INIT_CALL_CALLEE))
        {
            if let Some(server) = source {
                self.handle_init_reply(&parsed, server);
            }
        } else if self.group
            && self.active
            && self.started_at > 0
            && parsed.msg_type == MsgType::SERVER_REPLY
            && (parsed.cmd == Some(Command::ROOM_UPDATED) || parsed.cmd == Some(Command::END_CALL))
        {
            self.on_group_control(&parsed);
        }
    }

    /// Group ZRTP: command 50 is the roster, command 3 is a leave or a removal.
    fn on_group_control(&mut self, parsed: &ParsedPacket) {
        if parsed.cmd == Some(Command::ROOM_UPDATED) {
            if parsed.uid != Some(self.local_uid) || parsed.data.len() % 4 != 0 {
                return;
            }
            let share = self.share_id;
            let ours = self.local_uid;
            let uids: Vec<u32> = parsed
                .data
                .chunks_exact(4)
                .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .filter(|uid| *uid != 0 && *uid != ours && (share == 0 || *uid != share))
                .collect();
            if let Some(callback) = self.on_roster.as_mut() {
                callback(&uids);
            }
            return;
        }
        let uid = parsed.uid.unwrap_or(0);
        let sub = parsed.sub_cmd.unwrap_or(0);
        if uid == self.local_uid {
            if sub != 1 && sub != 2 {
                return;
            }
            let why = if sub == 2 { "kicked" } else { "host-ended" };
            if let Some(callback) = self.on_removed.as_mut() {
                callback(why);
            }
        } else if uid != 0 {
            if let Some(callback) = self.on_member_left.as_mut() {
                callback(uid, sub);
            }
        }
    }

    /// Processes a P2P packet and executes its actions (reply, receive audio/video/RTCP).
    fn on_p2p(&mut self, buf: &[u8], from: &Server) {
        let Some(p2p) = self.p2p.as_mut() else {
            return;
        };
        let actions = p2p.on_packet(buf, from, now_ms());
        let mut queued: Vec<(Vec<u8>, Server)> = Vec::new();
        let mut audio: Vec<Vec<u8>> = Vec::new();
        let mut video: Vec<Vec<u8>> = Vec::new();
        let mut rtcp: Vec<(bool, Vec<u8>)> = Vec::new();
        for action in actions {
            match action {
                P2pAction::Send(pkt, dst) => queued.push((pkt, dst)),
                P2pAction::Audio(rtp) => audio.push(rtp),
                P2pAction::Video(rtp) => video.push(rtp),
                P2pAction::Rtcp { video: v, data } => rtcp.push((v, data)),
                P2pAction::PathUp(_) => {}
            }
        }
        for (pkt, dst) in queued {
            self.send(&pkt, &dst, "P2P");
        }
        for rtp in audio {
            self.rtcp.on_rtp(false, &rtp, now_ms());
            self.on_audio(&rtp);
        }
        for rtp in video {
            self.on_video_packet(&rtp);
        }
        for (v, data) in rtcp {
            self.rtcp.on_rtcp(v, &data, now_ms());
            if let Some(sr) = self.send_rate.as_mut() {
                sr.on_rtcp(if v { "video" } else { "audio" }, &data);
            }
        }
    }

    /// Reassembles an incoming video RTP packet into an access unit and dispatches the callback.
    fn on_video_packet(&mut self, rtp: &[u8]) {
        let Some(offset) = self.rtp_offset(rtp) else {
            return;
        };
        let rtp = &rtp[offset..];
        if rtp.len() < 12 {
            return;
        }
        let Some(header) = parse_rtp(rtp) else {
            return;
        };
        if self.group
            && (header.ssrc == self.local_uid
                || (self.share_id != 0 && header.ssrc == self.share_id))
        {
            return;
        }
        let roc = if self.group {
            self.video_rocs
                .entry(header.ssrc)
                .or_default()
                .guess(header.seq)
        } else {
            self.video_roc.guess(header.seq)
        };
        let Some((seq, ssrc, payload)) = self.open_rtp(rtp, roc, false) else {
            return;
        };
        if self.group {
            self.video_rocs.entry(ssrc).or_default().update(seq, roc);
        } else {
            self.video_roc.update(seq, roc);
        }
        // Transport-cc covers audio and video. Leaving video out makes the
        // phone count its own pictures as lost and it stays on a soft, blocky
        // rung (live 2026-11-13: 360x640 and no climb).
        let now = now_ms();
        self.rtcp.on_rtp(true, rtp, now);
        self.nack_video(ssrc, seq, now);
        if self.group {
            // The first packet is enough. Waiting for a whole frame left the
            // SFU unasked, and quality id 0xff would stop this member for good.
            self.note_video_member(ssrc);
        }
        let ts = u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]);
        let pt = rtp[1] & 0x7f;
        let marker = rtp[1] & 0x80 != 0;
        let frames = if self.group {
            self.member_video
                .entry(ssrc)
                .or_insert_with(|| VideoAssembler::new(true))
                .push(ts, pt, marker, &payload, seq, roc, now_ms())
        } else {
            self.video
                .push(ts, pt, marker, &payload, seq, roc, now_ms())
        };
        for mut frame in frames {
            frame.src = ssrc;
            if let Some(cb) = self.on_video.as_mut() {
                cb(&frame);
            }
        }
    }

    /// Sends InitZRTP to every server.
    pub fn send_init_zrtp(&mut self, role: &str) {
        self.replies.clear();
        self.refusals.clear();
        self.init_sent_at = now_ms();
        let packet = if self.group {
            let host = self
                .params
                .get("host")
                .and_then(Value::as_bool)
                .unwrap_or(role == "caller");
            let host_call = num(&self.params, "hostCall").unwrap_or(0) as u32;
            build_group_init_zrtp_packet(
                host,
                self.local_uid,
                self.sess_id.as_bytes(),
                true,
                self.call_id,
                host_call,
                self.group_layout,
            )
        } else {
            build_init_zrtp_packet(
                role == "caller",
                self.local_uid,
                self.peer_uid,
                self.call_id,
                self.sess_id.as_bytes(),
                &[],
                0x02,
            )
        };
        let servers = self.servers.clone();
        for server in &servers {
            self.send(&packet, server, "InitZRTP");
        }
    }

    /// Marks media active, keeps the chosen server, ends the others and sends
    /// the first Ping + ReqForward status.
    pub fn start(&mut self, _role: &str, rtp_ser_ip: Option<&str>) {
        self.active = true;
        if !self.send_udp {
            return;
        }
        if let Some(want) = rtp_ser_ip.and_then(parse_host_port) {
            if let Some(reply) = self.replies.iter().find(|r| r.server == want) {
                self.active_server = Some(reply.server.clone());
                self.server_token = reply.token;
            }
        }
        if self.active_server.is_none() && !self.replies.is_empty() {
            // No matching server: keep the fastest reply.
            let best = self.replies.iter().min_by_key(|r| r.rtt_ms).unwrap();
            self.active_server = Some(best.server.clone());
            self.server_token = best.token;
        }
        let Some(active) = self.active_server.clone() else {
            return;
        };
        let others: Vec<(Server, u32)> = self
            .replies
            .iter()
            .filter(|r| r.server != active)
            .map(|r| (r.server.clone(), r.token))
            .collect();
        for (server, token) in others {
            self.send_end_call(&server, token);
        }
        self.started_at = now_ms();
        self.send_ping();
        if !self.group {
            self.send_status();
        }
    }

    /// Sends a Ping to the active server. A group call sends ZaviPing instead,
    /// then asks the SFU for each watched member and announces our one layer.
    pub fn send_ping(&mut self) {
        let Some(server) = self.active_server.clone() else {
            return;
        };
        if self.group {
            let video_on = self.group_camera_on;
            let packet = build_zavi_ping_packet(
                self.local_uid,
                self.server_token,
                self.sess_id.as_bytes(),
                video_on,
            );
            self.send(&packet, &server, "ZaviPing");
            if self.zavi_video != Some(video_on) {
                self.zavi_video = Some(video_on);
                self.note(format!(
                    "media: ZaviPing video {}",
                    if video_on { "on" } else { "off" }
                ));
            }
            self.send_vid_qual();
            self.send_vid_qual_config();
            return;
        }
        let packet = build_ping_packet(self.local_uid, self.server_token, self.sess_id.as_bytes());
        self.send(&packet, &server, "Ping");
    }

    /// Sends a ReqForward status (sub 10) to the active server.
    ///
    /// The body carries how many audio packets we decrypted since the previous
    /// status. Electron's `_sendStatus` does the same with `rxSecond`. Sending
    /// 0 on every tick makes the phone treat the path as dead (`poorSignalTime`)
    /// and drop the call as a weak signal.
    pub fn send_status(&mut self) {
        if self.group {
            return;
        }
        let Some(server) = self.active_server.clone() else {
            return;
        };
        let rx_per_second = self.rx_second;
        self.rx_second = 0;
        let payload = req_forward_status(rx_per_second, 0, 0);
        let packet = build_req_forward_packet(
            self.local_uid,
            self.server_token,
            self.fwd_seq,
            0x0a,
            &payload,
        );
        self.fwd_seq = self.fwd_seq.wrapping_add(1);
        self.send(&packet, &server, "ReqForward");
    }

    /// One ReqForward sub 8 about 200 ms after media starts, as `zrtc-media.js`.
    pub fn send_fwd8_if_due(&mut self) {
        if self.group || self.fwd8_sent || !self.active || self.started_at == 0 {
            return;
        }
        if now_ms().saturating_sub(self.started_at) < 200 {
            return;
        }
        let Some(server) = self.active_server.clone() else {
            return;
        };
        self.fwd8_sent = true;
        let payload = [4_u8, 0, 0, 0, 0, 0, 0, 0];
        let packet =
            build_req_forward_packet(self.local_uid, self.server_token, self.fwd_seq, 8, &payload);
        self.fwd_seq = self.fwd_seq.wrapping_add(1);
        self.send(&packet, &server, "ReqForward");
    }

    /// Sends one encoded Opus frame as SRTP over the relay (MsgType 3).
    ///
    /// Returns false when media is not up yet, or when the server or the
    /// SRTP keys are still missing, so the frame never left the process.
    pub fn send_audio_frame(&mut self, opus: &[u8], level: u8) -> bool {
        if !self.active {
            return false;
        }
        let Some(server) = self.active_server.clone() else {
            return false;
        };
        let ssrc = self.local_uid;
        if let Some(sr) = self.send_rate.as_mut() {
            sr.on_sent(self.tw_seq, now_ms());
        }
        let now = now_ms();
        let header = if self.group {
            build_group_audio_rtp_header(
                self.seq,
                self.ts,
                ssrc,
                self.tw_seq,
                false,
                AUDIO_PT,
                level,
                now,
            )
        } else {
            build_audio_rtp_header(self.seq, self.ts, ssrc, self.tw_seq, false, AUDIO_PT)
        };
        let rtp = if self.plain_media {
            let mut raw = header;
            raw.extend_from_slice(opus);
            raw
        } else {
            let Some(keys) = self.srtp_context.clone() else {
                return false;
            };
            match self.protect_uplink(&header, opus, &keys, self.seq, ssrc, self.send_roc, true) {
                Ok(packet) => packet,
                Err(_) => return false,
            }
        };
        let packet = wrap_media_packet(MsgType::AUDIO_RTP, self.server_token, &rtp);
        self.send(&packet, &server, "audio");
        // Mirror the same packet over P2P once the peer has reached us (it deduplicates by seq).
        // A group call stays on the SFU.
        if !self.group {
            if let Some(p2p) = self.p2p.as_ref() {
                if let Some(path) = p2p.path().cloned() {
                    if let Some(wrapped) = p2p.wrap_audio(&rtp) {
                        self.send(&wrapped, &path, "P2P audio");
                    }
                }
            }
        }
        self.seq = self.seq.wrapping_add(1);
        if self.seq == 0 {
            self.send_roc = self.send_roc.wrapping_add(1);
        }
        self.ts = self.ts.wrapping_add(FRAME_SAMPLES);
        self.tw_seq = self.tw_seq.wrapping_add(1);
        true
    }

    /// Stops media and ends the call on the active server.
    pub fn stop(&mut self) {
        self.active = false;
        self.v_out.clear();
        if let Some(server) = self.active_server.clone() {
            self.send_end_call(&server, self.server_token);
        }
    }

    fn handle_init_reply(&mut self, parsed: &ParsedPacket, server: &Server) {
        if self.replies.iter().any(|r| r.server == *server)
            || self.refusals.len() + self.replies.len() >= self.servers.len()
        {
            return;
        }
        if let Some(res) = parsed.res {
            if res != 0 {
                self.refusals.push(res);
                return;
            }
        }
        let rtt = now_ms().saturating_sub(self.init_sent_at);
        self.replies.push(Reply {
            server: server.clone(),
            token: parsed.server_token.unwrap_or(0),
            rtt_ms: rtt,
            public_addr: parsed.public_addr.clone(),
        });
        if self.replies.len() + self.refusals.len() >= self.servers.len() {
            let best = self.replies.iter().min_by_key(|r| r.rtt_ms);
            if let Some(best) = best {
                self.active_server = Some(best.server.clone());
                self.server_token = best.token;
            }
        }
    }

    fn send_end_call(&mut self, server: &Server, token: u32) {
        let key = format!("{}:{}", server.host, server.port);
        if self.ended.contains(&key) {
            return;
        }
        self.ended.push(key);
        let packet = build_end_call_packet(self.local_uid, token, self.call_id);
        self.send(&packet, server, "EndCall");
    }

    fn send(&mut self, packet: &[u8], server: &Server, what: &str) {
        (self.send_hook)(packet, server, what);
    }

    fn on_audio(&mut self, rtp: &[u8]) {
        let Some(offset) = self.rtp_offset(rtp) else {
            return;
        };
        let rtp = &rtp[offset..];
        let Some(header) = parse_rtp(rtp) else {
            return;
        };
        // Group: each member is its own SSRC. Our own uplink and the screen
        // share are not played (Electron `_onAudio`).
        if self.group
            && (header.ssrc == self.local_uid
                || (self.share_id != 0 && header.ssrc == self.share_id))
        {
            return;
        }
        let roc = if self.group {
            self.down_rocs
                .entry(header.ssrc)
                .or_default()
                .guess(header.seq)
        } else {
            self.down_roc.guess(header.seq)
        };
        let Some((seq, ssrc, opus)) = self.open_rtp(rtp, roc, true) else {
            return;
        };
        if self.group {
            self.down_rocs.entry(ssrc).or_default().update(seq, roc);
        } else {
            self.down_roc.update(seq, roc);
        }
        let ext_seq = (roc << 16) | u32::from(seq);
        if let Some(callback) = self.on_audio_packet.as_mut() {
            if let Some(keys) = self.srtp_context.as_ref() {
                callback(&opus, keys);
            }
        }
        if let Some(callback) = self.on_audio_seq.as_mut() {
            callback(ext_seq, ssrc, &opus);
        }
        self.rx_second = self.rx_second.saturating_add(1);
    }

    /// 0 when `rtp` is already an RTP header. 4 when a token sits in front.
    fn rtp_offset(&mut self, rtp: &[u8]) -> Option<usize> {
        if rtp.first().is_some_and(|byte| byte >> 6 == 2) {
            return Some(0);
        }
        if rtp.len() > 16 && rtp.get(4).is_some_and(|byte| byte >> 6 == 2) {
            if !self.saw_token_prefix {
                self.saw_token_prefix = true;
                self.note("media: downlink RTP follows a 4-byte token");
            }
            return Some(4);
        }
        None
    }

    /// Reads a plain RTP payload. The header stays unencrypted on this path.
    fn plain_rtp(rtp: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
        if rtp.first().is_none_or(|byte| byte >> 6 != 2) {
            return None;
        }
        let header = parse_rtp(rtp)?;
        if header.header_len >= rtp.len() {
            return None;
        }
        Some((header.seq, header.ssrc, rtp[header.header_len..].to_vec()))
    }

    /// Decrypts SRTP, or reads plain RTP when the group call has no key.
    ///
    /// A failed MAC tries the call session once. It never switches the call to
    /// plain RTP: playing the ciphertext is the crackle, and it is not a picture.
    /// A datagram too short for a tag is not an auth failure.
    fn open_rtp(&mut self, rtp: &[u8], roc: u32, audio: bool) -> Option<(u16, u32, Vec<u8>)> {
        if rtp.first().is_none_or(|byte| byte >> 6 != 2) {
            return None;
        }
        if self.plain_media {
            return Self::plain_rtp(rtp);
        }
        let keys = self.srtp_context.as_ref()?.clone();
        match self.unprotect_downlink(rtp, roc, &keys, audio) {
            Ok(got) => self.accept_opened(rtp, got, audio),
            // A 36-byte group audio datagram cannot carry a 32-byte header and
            // a 10-byte tag. Counting it as an auth failure dropped a valid
            // key, and the picture was then parsed as plaintext.
            Err(SrtpError::TruncatedRtp) if self.group && audio => {
                if !self.short_noted {
                    self.short_noted = true;
                    self.note(format!(
                        "media: group audio packet too short for SRTP (rtp {} bytes)",
                        rtp.len()
                    ));
                }
                None
            }
            Err(SrtpError::TruncatedRtp) if self.group && !audio => {
                if !self.video_open_noted {
                    self.video_open_noted = true;
                    self.note(format!(
                        "media: group video packet too short for SRTP (rtp {} bytes)",
                        rtp.len()
                    ));
                }
                None
            }
            Err(SrtpError::AuthFailed) if self.group => {
                if let Some(alt) = self.alt_keys.clone() {
                    if let Ok(got) = self.unprotect_downlink(rtp, roc, &alt, audio) {
                        self.srtp_context = Some(alt);
                        self.alt_keys = None;
                        self.note("media: group srtp accepted the call session");
                        return self.accept_opened(rtp, got, audio);
                    }
                }
                self.note_group_auth_failed(rtp, roc, &keys, audio);
                None
            }
            Err(_) => None,
        }
    }

    fn stream_auth(&self, audio: bool) -> Option<SrtpAuthProfile> {
        if audio {
            self.audio_auth
        } else {
            self.video_auth
        }
    }

    fn set_stream_auth(&mut self, audio: bool, profile: SrtpAuthProfile) {
        if audio {
            self.audio_auth = Some(profile);
        } else {
            self.video_auth = Some(profile);
        }
    }

    /// Encrypts one uplink packet using the configured or latched stream profile.
    fn protect_uplink(
        &self,
        header: &[u8],
        payload: &[u8],
        keys: &SessionKeys,
        seq: u16,
        ssrc: u32,
        roc: u32,
        audio: bool,
    ) -> Result<Vec<u8>, SrtpError> {
        if let Some(profile) = self.stream_auth(audio) {
            return protect_rtp_auth(header, payload, keys, seq, ssrc, roc, profile);
        }
        protect_rtp(header, payload, keys, seq, ssrc, roc)
    }

    /// Opens one downlink packet. Group uses configured `NullAuthNoSeq` when
    /// enabled, otherwise tries SHA1-80 and alternative HMAC coverages/tags.
    fn unprotect_downlink(
        &mut self,
        rtp: &[u8],
        roc: u32,
        keys: &SessionKeys,
        audio: bool,
    ) -> Result<(u16, u32, Vec<u8>), SrtpError> {
        if let Some(profile) = self.stream_auth(audio) {
            return unprotect_rtp_auth(rtp, keys, roc, profile);
        }
        if !self.group {
            return unprotect_rtp(rtp, keys, roc);
        }
        let standard = unprotect_rtp(rtp, keys, roc);
        if standard.is_ok() {
            self.set_stream_auth(audio, SrtpAuthProfile::Sha1_80);
            return standard;
        }
        for profile in [
            SrtpAuthProfile::Sha1_80NoExt,
            SrtpAuthProfile::Sha1_80NoExtClearX,
            SrtpAuthProfile::Sha1_80RocLe,
            SrtpAuthProfile::Sha1_32,
            SrtpAuthProfile::Sha1_80NoRoc,
            SrtpAuthProfile::Sha1_32NoRoc,
        ] {
            if let Ok(got) = unprotect_rtp_auth(rtp, keys, roc, profile) {
                self.set_stream_auth(audio, profile);
                let stream = if audio { "audio" } else { "video" };
                self.note(format!(
                    "media: group srtp profile {} ({stream})",
                    profile.label()
                ));
                return Ok(got);
            }
        }
        Err(standard.err().unwrap_or(SrtpError::AuthFailed))
    }

    /// Counts a decrypted non-empty packet. Group video logs the first success once.
    fn accept_opened(
        &mut self,
        rtp: &[u8],
        got: (u16, u32, Vec<u8>),
        audio: bool,
    ) -> Option<(u16, u32, Vec<u8>)> {
        if got.2.is_empty() {
            return None;
        }
        if audio {
            self.rx_ok = self.rx_ok.saturating_add(1);
        } else if self.group && !self.video_open_noted {
            self.video_open_noted = true;
            self.note(format!(
                "media: group video decrypted (rtp {} bytes, payload {} bytes)",
                rtp.len(),
                got.2.len()
            ));
        }
        Some(got)
    }

    /// Records one failed group MAC. The packet is not decoded and the key stays.
    fn note_group_auth_failed(&mut self, rtp: &[u8], roc: u32, keys: &SessionKeys, audio: bool) {
        let shape = rtp_shape(rtp);
        if audio {
            self.rx_bad = self.rx_bad.saturating_add(1);
            if self.rx_bad == 1 {
                self.note(format!(
                    "media: group audio SRTP auth failed (key {} bytes, rtp {} bytes, {shape})",
                    self.srtp_key_len,
                    rtp.len()
                ));
            }
            if !self.audio_probe_noted {
                self.audio_probe_noted = true;
                self.probe_group_aes_cm(rtp, roc, keys, true);
            }
            return;
        }
        if !self.video_open_noted {
            self.video_open_noted = true;
            self.note(format!(
                "media: group video SRTP auth failed (rtp {} bytes, plain {}, {shape})",
                rtp.len(),
                u8::from(self.plain_media)
            ));
        }
        if !self.video_probe_noted {
            self.video_probe_noted = true;
            self.probe_group_aes_cm(rtp, roc, keys, false);
        }
    }

    /// One-time unverified AES-CM decrypt to classify whether key+IV are right.
    /// Never feeds decoders and never enables plain RTP.
    fn probe_group_aes_cm(&mut self, rtp: &[u8], roc: u32, keys: &SessionKeys, audio: bool) {
        let Ok((pt, plain)) = decrypt_unverified(rtp, keys, roc, 10) else {
            return;
        };
        let head: String = plain.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let kind = if !audio {
            if plain.starts_with(&[0, 0, 0, 1]) || plain.starts_with(&[0, 0, 1]) {
                "annex-b"
            } else if plain.len() >= 2 && plain[0] == 0x1c && matches!(plain[1], 0x80 | 0x00 | 0x40)
            {
                "fu-a-1c"
            } else if plain.windows(4).any(|w| w == [0, 0, 0, 1]) {
                "contains-annex-b"
            } else {
                "high-entropy"
            }
        } else if let Some(&b0) = plain.first() {
            if (b0 & 0x07) == 0 && (b0 >> 3) >= 12 {
                "opus-mono-1f"
            } else {
                "high-entropy"
            }
        } else {
            "empty"
        };
        let stream = if audio { "audio" } else { "video" };
        self.note(format!(
            "media: group {stream} aes-cm probe pt {pt} len {} head {head} kind {kind}",
            plain.len()
        ));
    }

    /// Records the first byte of an inbound datagram, without its contents.
    fn note_udp(&mut self, buf: &[u8]) {
        let Some(&kind) = buf.first() else {
            return;
        };
        let count = self.rx_types.entry(kind).or_default();
        *count = count.saturating_add(1);
        self.rx_udp = self.rx_udp.saturating_add(1);
        let kinds = self.rx_types.len();
        let milestone = self.rx_udp == 1 || self.rx_udp == 50 || self.rx_udp == 200;
        if kinds != self.rx_type_kinds || milestone {
            self.rx_type_kinds = kinds;
            let mut parts = Vec::new();
            for (kind, count) in &self.rx_types {
                parts.push(format!("{kind:02x}:{count}"));
            }
            self.note(format!(
                "media: udp {} bytes, types {}",
                buf.len(),
                parts.join(" ")
            ));
        }
    }

    /// Asks the SFU for this member before their first picture arrives.
    pub fn watch_member(&mut self, uid: u32) {
        if !self.group
            || uid == 0
            || uid == self.local_uid
            || (self.share_id != 0 && uid == self.share_id)
        {
            return;
        }
        if !self.vid_wanted.insert(uid) {
            return;
        }
        self.send_vid_qual();
    }

    /// Asks the SFU for this member the first time their video arrives.
    fn note_video_member(&mut self, ssrc: u32) {
        if ssrc == 0 || ssrc == self.local_uid || (self.share_id != 0 && ssrc == self.share_id) {
            return;
        }
        if !self.vid_wanted.insert(ssrc) {
            return;
        }
        self.send_vid_qual();
    }

    /// quality id 0 until a tile width exists. An empty table must not become
    /// 0xff: that value stops the member's video permanently.
    fn quality_id_for(&self, uid: u32) -> u8 {
        if self.vid_layers.is_empty() {
            return 0;
        }
        let width = self.render_width.get(&uid).copied().unwrap_or(0);
        let id = crate::zrtc_helpers::select_quality_id(&self.vid_layers, width);
        if id == 0xff {
            0
        } else {
            id
        }
    }

    fn send_vid_qual(&mut self) {
        let Some(server) = self.active_server.clone() else {
            return;
        };
        if !self.group || self.vid_wanted.is_empty() {
            return;
        }
        let entries: Vec<VidQualEntry> = self
            .vid_wanted
            .iter()
            .map(|uid| VidQualEntry {
                uid: *uid,
                quality_id: self.quality_id_for(*uid),
            })
            .collect();
        let packet =
            build_group_vid_qual_req(self.local_uid, self.server_token, self.ctl_seq, &entries);
        self.ctl_seq = self.ctl_seq.wrapping_add(1);
        self.send(&packet, &server, "VidQual");
        for entry in entries {
            if self.vid_asked.get(&entry.uid) == Some(&entry.quality_id) {
                continue;
            }
            self.vid_asked.insert(entry.uid, entry.quality_id);
            self.note(format!(
                "media: VidQualReq member {}: quality id 0x{:02x}",
                entry.uid, entry.quality_id
            ));
        }
    }

    /// Announces the single spatial layer we encode (`id: 0`). Falls back to
    /// 640×360, 500 kbps when `groupcall.spatialLayers` is absent.
    fn send_vid_qual_config(&mut self) {
        let Some(server) = self.active_server.clone() else {
            return;
        };
        if !self.group {
            return;
        }
        let layer = self.group_send_layer.unwrap_or(VidQualLayer {
            id: 0,
            a: 500,
            b: 640,
            c: 360,
        });
        let packet =
            build_group_vid_qual_config(self.local_uid, self.server_token, self.ctl_seq, &[layer]);
        self.ctl_seq = self.ctl_seq.wrapping_add(1);
        self.send(&packet, &server, "VidQualConfig");
        if !self.vid_cfg_logged {
            self.vid_cfg_logged = true;
            self.note(format!(
                "media: VidQualConfig 1 layer, encode {}x{} {} kbps",
                layer.b, layer.c, layer.a
            ));
        }
    }
}

struct GroupVideoPlan {
    layers: Vec<Value>,
    encode: Profile,
    send_layer: Option<VidQualLayer>,
    sfu: SfuLayer,
}

/// Spatial layers and chosen uplink layer from `zrtcConfig.groupcall`, as `groupVideoPlan`.
fn group_video_plan(config: &Value) -> GroupVideoPlan {
    let default_sfu = SfuLayer {
        id: 12,
        long: false,
    };
    let Some(group) = config.get("groupcall") else {
        return GroupVideoPlan {
            layers: Vec::new(),
            encode: default_group_profile(),
            send_layer: None,
            sfu: default_sfu,
        };
    };
    let temporal = json_u32(group, "temporalNum").unwrap_or(0);
    if temporal == 0 {
        return GroupVideoPlan {
            layers: Vec::new(),
            encode: default_group_profile(),
            send_layer: None,
            sfu: default_sfu,
        };
    }
    let max_w = json_u32(config, "maxWidth").unwrap_or(1280);
    let max_h = json_u32(config, "maxHeight").unwrap_or(720);
    let key_ms = match json_u32(config, "keyFrameIntervalMs") {
        Some(0) | None => 10_000,
        Some(ms) => ms.max(2_000),
    };
    let max_br = match json_u32(config, "maxBitrate") {
        Some(0) | None => 1_500,
        Some(kbps) => kbps.max(150),
    };
    let Some(rows_json) = group.get("spatialLayers").and_then(Value::as_array) else {
        return GroupVideoPlan {
            layers: Vec::new(),
            encode: default_group_profile(),
            send_layer: None,
            sfu: default_sfu,
        };
    };
    let mut layers = Vec::new();
    let mut rows = Vec::new();
    let mut spatial = 0_u32;
    for raw in rows_json {
        let Some(num) = json_u32(raw, "factorNum") else {
            continue;
        };
        let Some(den) = json_u32(raw, "factorDen") else {
            continue;
        };
        let Some(kbps) = json_u32(raw, "bitrateKbps") else {
            continue;
        };
        let Some(fps) = json_u32(raw, "fps") else {
            continue;
        };
        if den == 0 || fps == 0 {
            continue;
        }
        let width =
            ((u64::from(max_w) * u64::from(num & 0xff) / u64::from(den & 0xff)) as u32) & !1;
        let height =
            ((u64::from(max_h) * u64::from(num & 0xff) / u64::from(den & 0xff)) as u32) & !1;
        if width == 0 || height == 0 {
            continue;
        }
        let steps = temporal.min(8);
        for temporal_index in 0..steps {
            let shift = steps.saturating_sub(1).saturating_sub(temporal_index);
            let wire = kbps / (1_u32 << shift.min(31));
            let id = spatial * 3 + temporal_index;
            layers.push(serde_json::json!({ "id": id, "a": wire, "b": width, "c": height }));
            if temporal_index + 1 == steps {
                rows.push((width, height, fps, wire.min(max_br), id));
            }
        }
        spatial += 1;
    }
    if layers.is_empty() || rows.is_empty() {
        return GroupVideoPlan {
            layers: Vec::new(),
            encode: default_group_profile(),
            send_layer: None,
            sfu: default_sfu,
        };
    }
    let want = std::env::var("ZCALL_GROUP_CAM_LAYER")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1);
    let idx = want.min(rows.len() - 1);
    let (w, h, fps, kbps, rung) = rows[idx];
    let encode = Profile {
        width: w,
        height: h,
        fps: fps.min(30),
        bitrate: kbps.saturating_mul(1000).max(150_000),
        key_ms,
        capture_big: w > 640,
        rung,
    };
    let send_layer = Some(VidQualLayer {
        id: 0,
        a: kbps,
        b: w as u16,
        c: h as u16,
    });
    let sfu = match json_u32(group, "codec") {
        Some(60 | 61 | 4) | None => SfuLayer {
            id: 12,
            long: false,
        },
        Some(_) => SfuLayer { id: 14, long: true },
    };
    GroupVideoPlan {
        layers,
        encode,
        send_layer,
        sfu,
    }
}

#[allow(dead_code)]
fn group_quality_layers(config: &Value) -> Vec<Value> {
    group_video_plan(config).layers
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    let number = value.get(key)?;
    if let Some(whole) = number.as_u64() {
        return Some(whole as u32);
    }
    let float = number.as_f64()?;
    if float.is_finite() && float > 0.0 {
        Some(float as u32)
    } else {
        None
    }
}

fn ipv4_servers(list: &[Value]) -> Vec<Server> {
    list.iter()
        .filter_map(|c| {
            let host = c.get("ip").and_then(Value::as_str)?;
            if !host.split('.').all(|p| p.parse::<u8>().is_ok()) {
                return None;
            }
            let port = c.get("port").and_then(Value::as_u64)? as u16;
            if port == 0 {
                return None;
            }
            Some(Server {
                host: host.to_owned(),
                port,
            })
        })
        .collect()
}

fn num(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(json_i64)
}

fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            if f.is_finite() && f.fract() == 0.0 && (i64::MIN as f64..=i64::MAX as f64).contains(&f)
            {
                Some(f as i64)
            } else {
                None
            }
        }),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(n) = s.parse::<i64>() {
                Some(n)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn str(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::srtp::{master_from_sess_id, protect_rtp, protect_rtp_profile, session_keys};
    use std::sync::{Arc, Mutex};

    #[test]
    fn parses_host_port() {
        assert_eq!(
            parse_host_port("192.0.2.10:4200"),
            Some(Server {
                host: "192.0.2.10".to_owned(),
                port: 4200
            })
        );
        assert_eq!(
            parse_host_port("[2001:db8::f]:4200"),
            Some(Server {
                host: "2001:db8::f".to_owned(),
                port: 4200
            })
        );
        assert_eq!(parse_host_port(""), None);
        assert_eq!(parse_host_port("192.0.2.10"), None);
        assert_eq!(parse_host_port(":4200"), None);
    }

    #[test]
    fn roc_tracker_guesses_and_advances() {
        let mut tracker = RocTracker::default();
        assert_eq!(tracker.guess(100), 0);
        tracker.update(100, 0);
        assert_eq!(tracker.guess(200), 0); // normal forward
        tracker.update(65535, 0);
        assert_eq!(tracker.guess(0), 1); // wrapped backwards
    }

    #[test]
    fn observe_only_session_decrypts_downlink_audio() {
        let got = Arc::new(Mutex::new(Vec::<u8>::new()));
        let capture = got.clone();
        let mut session = MediaSession::with_on_audio_packet(move |opus, _keys| {
            capture.lock().unwrap().clone_from(&opus.to_vec());
        });
        session.on_negotiated(&serde_json::json!({
            "callId": 1830974409,
            "role": "caller",
            "localUid": 251883420,
            "peerUid": 133222413,
            "srtpMode": 1,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [
                {"rtpaddr": "192.0.2.10:4200", "rtcpaddr": "192.0.2.10:4200"},
                {"rtpaddr": "192.0.2.11:4200", "rtcpaddr": "192.0.2.11:4200", "rtpIPv6": "2001:db8::f:4200"}
            ]
        }));

        assert_eq!(session.servers.len(), 2);
        assert_eq!(session.servers[0].host, "192.0.2.10");
        assert_eq!(session.servers[0].port, 4200);
        assert!(session.active_server.is_none());
        assert_eq!(session.srtp_context.as_ref().unwrap().enc.len(), 16);

        let keys = session.srtp_context.clone().unwrap();
        let ssrc = 133222413_u32;
        let mut header = vec![0_u8; 12];
        header[0] = 0x80;
        header[1] = 112;
        header[2..4].copy_from_slice(&7_u16.to_be_bytes());
        header[8..12].copy_from_slice(&ssrc.to_be_bytes());
        let protected = protect_rtp(&header, b"dummy-opus", &keys, 7, ssrc, 0).expect("protect");

        let mut down = vec![0x04];
        down.extend_from_slice(&protected);
        session.receive_packet(&down, None);
        assert_eq!(got.lock().unwrap().as_slice(), b"dummy-opus");

        session.start("caller", None);
        assert!(session.active);
        session.stop();
        assert!(!session.active);
    }

    #[test]
    fn group_srtp_mode_is_read_from_config() {
        let mut session = MediaSession::new();
        let secret = "PLAINTEXT-KEY-MUST-NOT-LEAK!!!";
        assert_eq!(secret.len(), 30);
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "callId": 1,
            "localUid": 2,
            "peerUid": 3,
            "sessId": "different-session-id-value!!",
            "config": {
                "srtpKey": secret,
                "srtpMode": 1
            }
        }));
        let line = session.notes.join("\n");
        assert!(line.contains("mode 1"), "{line}");
        assert!(line.contains("srtpMode"), "{line}");
        assert!(line.contains("srtpKey"), "{line}");
        assert!(!line.contains(secret), "{line}");
        assert!(!line.contains("mode none"), "{line}");
    }

    #[test]
    fn send_and_select_the_fastest_server() {
        let sent = Arc::new(Mutex::new(Vec::<(Vec<u8>, Server, String)>::new()));
        let record = sent.clone();
        let mut session = MediaSession::new();
        session.set_send(move |pkt, server, what| {
            record
                .lock()
                .unwrap()
                .push((pkt.to_vec(), server.clone(), what.to_owned()));
        });
        session.on_negotiated(&serde_json::json!({
            "callId": 1830974409,
            "role": "caller",
            "localUid": 251883420,
            "peerUid": 133222413,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [
                {"rtpaddr": "192.0.2.10:4200"},
                {"rtpaddr": "192.0.2.11:4200"}
            ]
        }));

        session.send_init_zrtp("caller");
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 2);
            assert!(sent.iter().all(|(_, _, what)| what == "InitZRTP"));
            assert_eq!(
                unwrap_packet(&sent[0].0).unwrap().cmd,
                Some(Command::INIT_CALL_CALLER)
            );
        }

        // Server reply (MsgType 2) from the second server, then the first.
        let reply = |token: u32| {
            let mut p = crate::zrtp::build_control_header(
                MsgType::SERVER_REPLY,
                crate::zrtp::MAGIC_BYTE,
                0,
                0,
                251883420,
                0,
                Command::INIT_CALL_CALLER,
                2,
            );
            p.extend_from_slice(&0_u32.to_le_bytes());
            p.extend_from_slice(&1830974409_u32.to_le_bytes());
            p.extend_from_slice(&token.to_le_bytes());
            p.extend_from_slice(&0_u16.to_le_bytes());
            p
        };
        let servers = session.servers.clone();
        session.receive_packet(&reply(0x12345678), Some(&servers[1]));
        assert!(session.active_server.is_none());
        session.receive_packet(&reply(0x11111111), Some(&servers[0]));
        // The first reply (servers[1]) has the lower round-trip time, so it wins.
        assert_eq!(session.active_server.as_ref(), Some(&servers[1]));
        assert_eq!(session.server_token, 0x12345678);

        sent.lock().unwrap().clear();
        session.send_udp = true;
        session.start("caller", None);
        {
            let sent = sent.lock().unwrap();
            let ends = sent
                .iter()
                .filter(|(_, s, w)| *w == "EndCall" && s.host == "192.0.2.10")
                .count();
            assert_eq!(ends, 1);
            assert_eq!(sent.iter().filter(|(_, _, w)| *w == "Ping").count(), 1);
            assert_eq!(
                sent.iter().filter(|(_, _, w)| *w == "ReqForward").count(),
                1
            );
        }
        session.stop();
    }

    #[test]
    fn sends_audio_as_srtp_over_relay() {
        let sent = Arc::new(Mutex::new(Vec::<(Vec<u8>, Server, String)>::new()));
        let record = sent.clone();
        let mut session = MediaSession::new();
        session.set_send(move |pkt, server, what| {
            record
                .lock()
                .unwrap()
                .push((pkt.to_vec(), server.clone(), what.to_owned()));
        });
        session.on_negotiated(&serde_json::json!({
            "callId": 1830974409,
            "role": "caller",
            "localUid": 251883420,
            "peerUid": 133222413,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}]
        }));
        let keys = session.srtp_context.clone().unwrap();
        session.send_udp = true;
        session.active = true;
        session.active_server = Some(session.servers[0].clone());
        session.server_token = 7;

        session.send_audio_frame(b"dummy-opus", 127);
        {
            let sent = sent.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert_eq!(sent[0].2, "audio");
            let parsed = unwrap_packet(&sent[0].0).unwrap();
            assert_eq!(parsed.msg_type, MsgType::AUDIO_RTP);
            assert_eq!(parsed.token, Some(7));
            let (seq, ssrc, opus) = unprotect_rtp(&parsed.payload, &keys, 0).unwrap();
            assert_eq!(seq, 0);
            assert_eq!(ssrc, 251883420);
            assert_eq!(opus, b"dummy-opus");
        }
        // Counters advance for the next frame.
        session.send_audio_frame(b"next", 127);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        let parsed = unwrap_packet(&sent[1].0).unwrap();
        let (seq, _ssrc, opus) = unprotect_rtp(&parsed.payload, &keys, 0).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(opus, b"next");
    }

    #[test]
    fn rtcp_feedback_follows_received_audio() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 1,
            "role": "callee",
            "localUid": 251883420,
            "peerUid": 133222413,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}]
        }));
        // Incoming audio packet carrying a transport-wide sequence number (extension ID 5).
        let mut rtp = vec![0x90, 0x70, 0x00, 0x07, 0, 0, 0, 0, 0, 0, 0, 0];
        rtp.extend_from_slice(&[0xbe, 0xde, 0x00, 0x01]);
        rtp.push(0x51);
        rtp.extend_from_slice(&41_u16.to_be_bytes());
        rtp.push(0);
        for _ in 0..12 {
            session.rtcp.on_rtp(false, &rtp, 1_000);
        }
        assert_eq!(session.rtcp.peer_ssrc, 0);
        // peer_ssrc read from the header SSRC (bytes 8..12) is 0 here, so set it manually.
        session.rtcp.peer_ssrc = 133222413;
        let packets = session.rtcp_packets(2_000, false);
        assert!(!packets.is_empty(), "phải có transport-cc để gửi");
        assert_eq!(packets[0].0, MsgType::AUDIO_FEC);
        let fb = crate::rtcp::parse_twcc(&packets[0].1).expect("parse twcc");
        assert_eq!(fb.media_ssrc, 133222413);
        // Periodic report: includes audio SR + SDES.
        let reports = session.rtcp_packets(3_000, true);
        assert!(reports
            .iter()
            .any(|(t, p)| *t == MsgType::AUDIO_FEC && p.len() >= 40));
    }

    #[test]
    fn p2p_candidates_start_nat_binding() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 5,
            "role": "caller",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}],
            "p2pCandidates": [
                {"ip": "203.0.113.9", "port": 51000, "type": 0},
                {"ip": "2001:db8::1", "port": 51001, "type": 0}
            ]
        }));
        assert!(session.p2p.is_some());
        // IPv6 candidates are skipped, leaving only IPv4 candidates.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let packets = session.p2p_packets(now);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].1.host, "203.0.113.9");
        assert_eq!(packets[0].0[7], crate::p2p::kind::BIND);
        assert!(session.p2p_path().is_none());
    }

    #[test]
    fn answer_candidates_start_nat_binding_after_negotiate() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 9,
            "role": "caller",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}]
        }));
        assert!(session.p2p.is_none());
        session.adopt_p2p_candidates(&[
            serde_json::json!({"ip": "198.51.100.4", "port": 4000, "type": 0}),
        ]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let packets = session.p2p_packets(now);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].1.host, "198.51.100.4");
        assert_eq!(packets[0].0[7], crate::p2p::kind::BIND);
    }

    #[test]
    fn sends_a_key_frame_as_srtp_video() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 11,
            "role": "caller",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}]
        }));
        session.active = true;
        session.active_server = Some(Server {
            host: "192.0.2.10".to_owned(),
            port: 4200,
        });
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = sent.clone();
        session.set_send(move |packet, _server, what| {
            assert_eq!(what, "video");
            sink.lock().unwrap().push(packet.to_vec());
        });
        let annex_b = [0u8, 0, 0, 1, 0x65, 1, 2, 3];
        session.send_video_frame(false, &annex_b, false, 1_000);
        assert!(
            sent.lock().unwrap().is_empty(),
            "delta before a key is dropped"
        );
        session.send_video_frame(true, &annex_b, false, 1_000);
        assert_eq!(sent.lock().unwrap().len(), 1);
        assert_eq!(sent.lock().unwrap()[0][0], MsgType::VIDEO_RTP);
    }

    #[test]
    fn room_updated_lists_other_uids() {
        use crate::zrtp::{build_control_header, Command, MsgType, MAGIC_BYTE};
        let mut session = MediaSession::new();
        session.group = true;
        session.active = true;
        session.started_at = 1;
        session.local_uid = 10;
        session.share_id = 99;
        let got = Arc::new(Mutex::new(Vec::<u32>::new()));
        let sink = got.clone();
        session.set_on_roster(move |uids| sink.lock().unwrap().extend_from_slice(uids));
        let mut packet = build_control_header(
            MsgType::SERVER_REPLY,
            MAGIC_BYTE,
            0,
            0,
            10,
            0,
            Command::ROOM_UPDATED,
            0,
        );
        for uid in [10_u32, 0, 77, 99] {
            packet.extend_from_slice(&uid.to_le_bytes());
        }
        session.receive_packet(&packet, None);
        assert_eq!(*got.lock().unwrap(), vec![77]);
    }

    #[test]
    fn assembles_downlink_video() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 6,
            "role": "callee",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}]
        }));
        let keys = session.srtp_context.clone().expect("khoá SRTP");
        let got = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = got.clone();
        session.set_on_video(move |frame| sink.lock().unwrap().push(frame.data.clone()));

        // One complete H.264 key frame (Annex-B) at PT 98 with the marker bit set.
        let annex_b = [0u8, 0, 0, 1, 0x65, 1, 2, 3, 4];
        let header = build_audio_rtp_header(7, 960, 200, 1, true, crate::video::KEY_PT);
        let rtp = protect_rtp(&header, &annex_b, &keys, 7, 200, 0).expect("protect");
        let (dseq, _dssrc, dpl) = unprotect_rtp(&rtp, &keys, 0).expect("direct unprotect");
        assert_eq!(dseq, 7);
        assert_eq!(dpl, annex_b.to_vec(), "unprotect trực tiếp phải đúng");
        // Downlink video: 1-byte type, no token.
        let mut packet = vec![MsgType::VIDEO_DOWN];
        packet.extend_from_slice(&rtp);
        session.receive_packet(&packet, None);

        let frames = got.lock().unwrap();
        assert_eq!(frames.len(), 1, "phải ghép được 1 khung hình");
        assert_eq!(
            frames[0], annex_b,
            "ghép được nhưng dữ liệu khác: {:?}",
            frames[0]
        );
        let feedback = session.rtcp.twcc_packets();
        assert!(
            !feedback.is_empty(),
            "một gói video phải vào transport-cc, nếu không điện thoại giữ bitrate thấp"
        );
    }

    #[test]
    fn video_hole_asks_for_the_missing_packet() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 6,
            "role": "callee",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}]
        }));
        session.active = true;
        session.active_server = Some(Server {
            host: "192.0.2.10".to_owned(),
            port: 4200,
        });
        let nacks = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&nacks);
        session.set_send(move |_, _, what| {
            if what == "rtcp" {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        });
        let keys = session.srtp_context.clone().expect("khoá SRTP");
        let annex_b = [0u8, 0, 0, 1, 0x65, 1, 2, 3, 4];
        for (seq, tw) in [(7_u16, 1_u16), (9, 3)] {
            let header = build_audio_rtp_header(seq, 960, 200, tw, true, crate::video::KEY_PT);
            let rtp = protect_rtp(&header, &annex_b, &keys, seq, 200, 0).expect("protect");
            let mut packet = vec![MsgType::VIDEO_DOWN];
            packet.extend_from_slice(&rtp);
            session.receive_packet(&packet, None);
        }
        assert!(
            nacks.load(Ordering::Relaxed) >= 1,
            "lỗ sequence 8 phải được xin lại"
        );
        assert!(session.rtcp.counters.nack >= 1);
    }

    #[test]
    fn negotiated_config_builds_send_rate() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "callId": 9,
            "role": "caller",
            "localUid": 100,
            "peerUid": 200,
            "sessId": "sample-session-id-for-testing-x",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}],
            "config": {"startupBitrate": 300, "maxBitrate": 1500}
        }));
        let sr = session.send_rate.as_ref().expect("phải có thang bậc");
        assert!(!sr.ladder.is_empty());
        // Tick with a large `now` so logging runs (>= LOG_MS) without panicking.
        let _ = session.tick_send_rate(false, None, 10_000);
    }

    #[test]
    fn peer_key_request_is_one_pli_per_second() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let mut session = MediaSession::new();
        session.active = true;
        session.active_server = Some(Server {
            host: "127.0.0.1".to_owned(),
            port: 9,
        });
        session.rtcp = crate::rtcp::RtcpFeedback::new(7);
        let sent = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&sent);
        session.set_send(move |_, _, what| {
            assert_eq!(what, "rtcp");
            counter.fetch_add(1, Ordering::Relaxed);
        });
        assert!(session.request_peer_key_frame(Some(9), 1_000));
        assert!(!session.request_peer_key_frame(Some(9), 1_500));
        assert!(session.request_peer_key_frame(Some(9), 2_000));
        assert!(session.request_peer_key_frame(Some(10), 2_000));
        assert_eq!(sent.load(Ordering::Relaxed), 3);
        assert!(!session.request_peer_key_frame(None, 3_000));
    }

    #[test]
    fn status_reports_then_clears_received_audio() {
        let mut session = MediaSession::new();
        session.active = true;
        session.active_server = Some(Server {
            host: "192.0.2.10".to_owned(),
            port: 4200,
        });
        session.rx_second = 37;
        let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = got.clone();
        session.set_send(move |pkt, _, what| {
            if what == "ReqForward" {
                sink.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.send_status();
        session.send_status();
        let sent = got.lock().unwrap();
        assert_eq!(sent.len(), 2);
        let rx_of = |pkt: &[u8]| {
            let parsed = unwrap_packet(pkt).expect("status");
            assert_eq!(parsed.sub_cmd, Some(0x0a));
            u32::from_le_bytes(parsed.data[16..20].try_into().unwrap())
        };
        assert_eq!(rx_of(&sent[0]), 37);
        assert_eq!(rx_of(&sent[1]), 0);
    }

    fn group_session() -> MediaSession {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": true,
            "callId": 42,
            "hostCall": 100,
            "role": "caller",
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": ""}
        }));
        session
    }

    #[test]
    fn group_init_uses_subcmd_4_and_start_pings_the_sfu() {
        let sent = Arc::new(Mutex::new(Vec::<(Vec<u8>, String)>::new()));
        let record = sent.clone();
        let mut session = group_session();
        session.set_send(move |pkt, _, what| {
            record.lock().unwrap().push((pkt.to_vec(), what.to_owned()));
        });
        assert!(session.plain_media);
        assert!(session.p2p.is_none());
        session.send_init_zrtp("caller");
        session.send_udp = true;
        session.active_server = Some(session.servers[0].clone());
        session.server_token = 3;
        session.start("caller", None);
        let sent = sent.lock().unwrap();
        let init = unwrap_packet(&sent[0].0).expect("init");
        assert_eq!(init.sub_cmd, Some(0x04));
        assert_eq!(
            sent.iter().filter(|(_, what)| what == "ZaviPing").count(),
            1
        );
        assert_eq!(
            sent.iter()
                .filter(|(_, what)| what == "VidQualConfig")
                .count(),
            1
        );
        assert_eq!(sent.iter().filter(|(_, what)| what == "Ping").count(), 0);
        assert_eq!(
            sent.iter().filter(|(_, what)| what == "ReqForward").count(),
            0
        );
        let config = sent
            .iter()
            .find(|(_, what)| what == "VidQualConfig")
            .expect("config");
        let parsed = unwrap_packet(&config.0).expect("config packet");
        assert_eq!(parsed.sub_cmd, Some(0x0c));
        // u16 length, count 1, id 0, bitrate 500, 640x360.
        assert_eq!(parsed.data[2], 1);
        assert_eq!(parsed.data[3], 0);
        assert_eq!(
            u32::from_le_bytes(parsed.data[4..8].try_into().unwrap()),
            500
        );
    }

    #[test]
    fn group_video_asks_for_layer_zero_not_stop() {
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let record = sent.clone();
        let mut session = group_session();
        session.set_send(move |pkt, _, what| {
            if what == "VidQual" {
                record.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.send_udp = true;
        session.active = true;
        session.active_server = Some(session.servers[0].clone());
        session.server_token = 9;
        let annex_b = [0u8, 0, 0, 1, 0x65, 1, 2, 3, 4];
        let mut rtp = build_audio_rtp_header(1, 960, 77, 1, true, crate::video::KEY_PT);
        rtp.extend_from_slice(&annex_b);
        let mut packet = vec![MsgType::VIDEO_DOWN];
        packet.extend_from_slice(&rtp);
        session.receive_packet(&packet, None);
        let sent = sent.lock().unwrap();
        assert_eq!(
            sent.len(),
            1,
            "the first picture must ask the SFU for that member"
        );
        let parsed = unwrap_packet(&sent[0]).expect("vidqual");
        assert_eq!(parsed.sub_cmd, Some(0x0d));
        assert_eq!(parsed.data[2], 1);
        assert_eq!(
            u32::from_le_bytes(parsed.data[3..7].try_into().unwrap()),
            77
        );
        assert_eq!(parsed.data[7], 0, "quality 0xff would stop this member");
        assert!(session
            .notes
            .iter()
            .any(|line| line.contains("VidQualReq member 77")));
    }

    #[test]
    fn group_plain_audio_round_trips() {
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let record = sent.clone();
        let mut session = group_session();
        session.set_send(move |pkt, _, _| record.lock().unwrap().push(pkt.to_vec()));
        let heard = Arc::new(Mutex::new(Vec::<u8>::new()));
        let sink = heard.clone();
        session.set_on_audio_seq(move |_seq, ssrc, opus| {
            assert_eq!(ssrc, 55);
            sink.lock().unwrap().extend_from_slice(opus);
        });
        session.active = true;
        session.active_server = Some(session.servers[0].clone());
        assert!(session.send_audio_frame(b"group-opus", 40));
        let packet = sent.lock().unwrap()[0].clone();
        let parsed = unwrap_packet(&packet).expect("audio");
        assert_eq!(parsed.payload[16] >> 4, 2);
        assert_eq!(parsed.payload[17], 40);
        let mut down = vec![MsgType::AUDIO_RTP_DOWN];
        down.extend_from_slice(&parsed.payload);
        session.receive_packet(&down, None);
        assert!(
            heard.lock().unwrap().is_empty(),
            "our own uplink is not played"
        );
        let mut rtp = build_group_audio_rtp_header(3, 320, 55, 2, false, AUDIO_PT, 20, 0);
        rtp.extend_from_slice(b"group-opus");
        let mut other = vec![MsgType::AUDIO_RTP_DOWN];
        other.extend_from_slice(&rtp);
        session.receive_packet(&other, None);
        assert_eq!(heard.lock().unwrap().as_slice(), b"group-opus");
    }

    #[test]
    fn group_audio_stays_silent_when_the_key_does_not_authenticate() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": "k".repeat(40), "srtpNoSeq": 0}
        }));
        assert!(!session.plain_media);
        assert!(session.srtp_context.is_some());
        assert!(session
            .notes
            .iter()
            .any(|line| line.contains("key 40 bytes, plain 0")));
        assert!(session
            .notes
            .iter()
            .any(|line| line.contains("session 13 bytes, same 0")));
        let heard = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = heard.clone();
        session.set_on_audio_seq(move |_seq, _ssrc, opus| {
            sink.lock().unwrap().push(opus.to_vec());
        });
        session.active = true;
        for n in 0..4u16 {
            let mut rtp = build_group_audio_rtp_header(n, 320, 55, n, false, AUDIO_PT, 20, 0);
            rtp.extend_from_slice(b"plain-opus-frame");
            let mut down = vec![MsgType::AUDIO_RTP_DOWN];
            down.extend_from_slice(&rtp);
            session.receive_packet(&down, None);
        }
        assert!(
            !session.plain_media,
            "a bad tag must not play the ciphertext"
        );
        assert!(heard.lock().unwrap().is_empty());
        assert_eq!(session.rx_bad, 4);
        assert!(session.notes.iter().any(|line| {
            line.contains("group audio SRTP auth failed")
                && line.contains("pt 112")
                && line.contains("pad 0")
                && line.contains("header 32")
                && line.contains("hex 90")
        }));
        assert!(session
            .notes
            .iter()
            .all(|line| !line.contains("switching to plain")));
    }

    #[test]
    fn group_audio_accepts_the_call_session_when_srtp_key_fails() {
        let sess = "s".repeat(40);
        let master = master_from_sess_id(sess.as_bytes()).expect("session master");
        let session_keys = session_keys(&master).expect("session keys");
        let header = build_group_audio_rtp_header(1, 320, 55, 1, false, AUDIO_PT, 20, 0);
        let rtp = protect_rtp(&header, b"session-opus", &session_keys, 1, 55, 0).expect("protect");
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": sess,
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": "k".repeat(40), "srtpNoSeq": 0}
        }));
        assert!(session.notes.iter().any(|line| line.contains("same 0")));
        let heard = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = heard.clone();
        session.set_on_audio_seq(move |_seq, _ssrc, opus| {
            sink.lock().unwrap().push(opus.to_vec());
        });
        session.active = true;
        let mut down = vec![MsgType::AUDIO_RTP_DOWN];
        down.extend_from_slice(&rtp);
        session.receive_packet(&down, None);
        assert_eq!(heard.lock().unwrap().as_slice(), [b"session-opus".to_vec()]);
        assert!(!session.plain_media);
        assert_eq!(session.srtp_context.as_ref(), Some(&session_keys));
        assert!(session
            .notes
            .iter()
            .any(|line| line.contains("group srtp accepted the call session")));
    }

    #[test]
    fn group_audio_accepts_a_four_byte_tag() {
        let key = "k".repeat(40);
        let master = master_from_sess_id(key.as_bytes()).expect("master");
        let keys = session_keys(&master).expect("keys");
        let header = build_group_audio_rtp_header(1, 320, 55, 1, false, AUDIO_PT, 20, 0);
        let rtp =
            protect_rtp_profile(&header, b"short-opus", &keys, 1, 55, 0, 4, true).expect("protect");
        assert_eq!(rtp.len(), header.len() + b"short-opus".len() + 4);
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": key, "srtpNoSeq": 0}
        }));
        let heard = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = heard.clone();
        session.set_on_audio_seq(move |_seq, _ssrc, opus| {
            sink.lock().unwrap().push(opus.to_vec());
        });
        session.active = true;
        let mut down = vec![MsgType::AUDIO_RTP_DOWN];
        down.extend_from_slice(&rtp);
        session.receive_packet(&down, None);
        assert_eq!(heard.lock().unwrap().as_slice(), [b"short-opus".to_vec()]);
        assert!(!session.plain_media);
        assert!(session
            .notes
            .iter()
            .any(|line| line.contains("group srtp profile tag 4 roc 1")));
    }

    #[test]
    fn group_short_audio_does_not_drop_the_srtp_key() {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": "k".repeat(40), "srtpNoSeq": 0}
        }));
        session.active = true;
        for n in 0..4u16 {
            let mut rtp = build_group_audio_rtp_header(n, 320, 55, n, false, AUDIO_PT, 20, 0);
            rtp.extend_from_slice(&[0, 1, 2, 3]);
            assert_eq!(rtp.len(), 36);
            let mut down = vec![MsgType::AUDIO_RTP_DOWN];
            down.extend_from_slice(&rtp);
            session.receive_packet(&down, None);
        }
        assert!(!session.plain_media);
        assert_eq!(session.rx_bad, 0);
        assert!(session
            .notes
            .iter()
            .any(|line| line.contains("too short for SRTP")));
    }

    #[test]
    fn group_watch_asks_before_the_first_picture() {
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let record = sent.clone();
        let mut session = group_session();
        session.set_send(move |pkt, _, what| {
            if what == "VidQual" {
                record.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.active = true;
        session.active_server = Some(session.servers[0].clone());
        session.server_token = 9;
        session.watch_member(77);
        session.watch_member(77);
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        let parsed = unwrap_packet(&sent[0]).expect("vidqual");
        assert_eq!(parsed.sub_cmd, Some(0x0d));
        assert_eq!(
            u32::from_le_bytes(parsed.data[3..7].try_into().unwrap()),
            77
        );
        assert_eq!(parsed.data[7], 0);
    }

    #[test]
    fn group_audio_and_video_default_to_null_auth_no_seq() {
        let key = "k".repeat(30);
        let master = master_from_sess_id(key.as_bytes()).expect("master");
        let keys = session_keys(&master).expect("keys");
        let audio_hdr = build_group_audio_rtp_header(0x01f9, 320, 55, 2, false, AUDIO_PT, 20, 0);
        let audio_rtp = crate::srtp::protect_rtp_auth(
            &audio_hdr,
            b"8b-audio",
            &keys,
            0x01f9,
            55,
            3,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("protect audio");
        assert_eq!(audio_rtp.len(), audio_hdr.len() + 8);

        let video_hdr1 = build_audio_rtp_header(5, 900, 55, 2, false, crate::video::KEY_PT);
        let nal1 = vec![0x1c, 0x80, 0x00, 0x00, 0x00, 0x01, 0x67, 0x64];
        let video_rtp1 = crate::srtp::protect_rtp_auth(
            &video_hdr1,
            &nal1,
            &keys,
            5,
            55,
            1,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("protect video 1");
        let video_hdr2 = build_audio_rtp_header(6, 900, 55, 3, true, crate::video::KEY_PT);
        let nal2 = vec![0x1c, 0x40, 0x00, 0x1f];
        let video_rtp2 = crate::srtp::protect_rtp_auth(
            &video_hdr2,
            &nal2,
            &keys,
            6,
            55,
            1,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("protect video 2");
        let mut expected_sps = nal1[2..].to_vec();
        expected_sps.extend_from_slice(&nal2[2..]);

        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let rec = sent.clone();
        let heard = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = heard.clone();
        let frames = Arc::new(Mutex::new(Vec::<(u32, bool, Vec<u8>)>::new()));
        let video_sink = frames.clone();

        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": key}
        }));
        session.set_send(move |pkt, _, what| {
            if what == "audio" {
                rec.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.set_on_audio_seq(move |_seq, _ssrc, opus| {
            sink.lock().unwrap().push(opus.to_vec());
        });
        session.set_on_video(move |frame| {
            video_sink
                .lock()
                .unwrap()
                .push((frame.src, frame.key, frame.data.clone()));
        });
        session.active = true;
        session.active_server = Some(session.servers[0].clone());

        let mut down_audio = vec![MsgType::AUDIO_RTP_DOWN];
        down_audio.extend_from_slice(&audio_rtp);
        session.receive_packet(&down_audio, None);
        assert_eq!(heard.lock().unwrap().as_slice(), [b"8b-audio".to_vec()]);
        assert_eq!(session.rx_ok, 1);

        for rtp in [video_rtp1, video_rtp2] {
            let mut down_video = vec![MsgType::VIDEO_DOWN];
            down_video.extend_from_slice(&rtp);
            session.receive_packet(&down_video, None);
        }
        let frames = frames.lock().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0], (55, true, expected_sps));

        assert!(session.send_audio_frame(b"up-opus", 15));
        let pkt = sent.lock().unwrap()[0].clone();
        let parsed = unwrap_packet(&pkt).expect("uplink");
        let (_, _, up_plain) = crate::srtp::unprotect_rtp_auth(
            &parsed.payload,
            &keys,
            0,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("uplink no-seq");
        assert_eq!(up_plain, b"up-opus");
    }

    #[test]
    fn group_split_no_seq_respects_aud_srtp_no_seq() {
        let key = "k".repeat(30);
        let master = master_from_sess_id(key.as_bytes()).expect("master");
        let keys = session_keys(&master).expect("keys");
        let audio_hdr = build_group_audio_rtp_header(2, 320, 55, 2, false, AUDIO_PT, 20, 0);
        let audio_hmac = protect_rtp(&audio_hdr, b"hmac-audio", &keys, 2, 55, 0).expect("audio");
        let video_hdr = build_audio_rtp_header(4, 900, 55, 2, true, crate::video::KEY_PT);
        let sps = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1f];
        let video_no_seq = crate::srtp::protect_rtp_auth(
            &video_hdr,
            &sps,
            &keys,
            4,
            55,
            0,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("video");

        let heard = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let sink = heard.clone();
        let frames = Arc::new(Mutex::new(Vec::<(u32, bool, Vec<u8>)>::new()));
        let video_sink = frames.clone();

        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": key, "srtpNoSeq": 1, "audSrtpNoSeq": 0}
        }));
        session.set_on_audio_seq(move |_seq, _ssrc, opus| {
            sink.lock().unwrap().push(opus.to_vec());
        });
        session.set_on_video(move |frame| {
            video_sink
                .lock()
                .unwrap()
                .push((frame.src, frame.key, frame.data.clone()));
        });
        session.active = true;

        let mut down_audio = vec![MsgType::AUDIO_RTP_DOWN];
        down_audio.extend_from_slice(&audio_hmac);
        session.receive_packet(&down_audio, None);
        assert_eq!(heard.lock().unwrap().as_slice(), [b"hmac-audio".to_vec()]);

        let mut down_video = vec![MsgType::VIDEO_DOWN];
        down_video.extend_from_slice(&video_no_seq);
        session.receive_packet(&down_video, None);
        assert_eq!(frames.lock().unwrap().as_slice(), [(55, true, sps)]);
    }

    #[test]
    fn group_video_with_srtp_key_sends_sfu_header_and_null_auth_no_seq() {
        let key = "0123456789abcdef0123456789abcd";
        let master = master_from_sess_id(key.as_bytes()).expect("master");
        let keys = session_keys(&master).expect("keys");

        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let rec = sent.clone();

        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": true,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": key, "srtpNoSeq": 1, "audSrtpNoSeq": 1}
        }));
        session.set_send(move |pkt, _, what| {
            if what == "video" {
                rec.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.active = true;
        session.active_server = Some(session.servers[0].clone());
        session.server_token = 7;
        session.set_group_camera(true);

        let frame = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xe0, 0x1e];
        session.send_video_frame(true, &frame, false, 1234);
        let pkts = sent.lock().unwrap();
        assert_eq!(pkts.len(), 1);
        let parsed = unwrap_packet(&pkts[0]).expect("wrapped video");
        assert_eq!(parsed.msg_type, MsgType::VIDEO_RTP);

        let hdr = parse_rtp(&parsed.payload).expect("rtp header");
        // Extension bit (X=0x10) must be set and SFU header extension present,
        // followed immediately by the NullAuthNoSeq-encrypted payload (tag len 0).
        assert_ne!(parsed.payload[0] & 0x10, 0);
        assert!(hdr.header_len > 12);
        assert_eq!(parsed.payload[1] & 0x7f, crate::video::KEY_PT);
        assert_eq!(parsed.payload.len(), hdr.header_len + frame.len());

        let (_seq, ssrc, plain) = crate::srtp::unprotect_rtp_auth(
            &parsed.payload,
            &keys,
            0,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("decrypt group video");
        assert_eq!(ssrc, 100);
        assert_eq!(plain, frame);
    }

    #[test]
    fn routes_audio_fec_rtcp_and_paces_large_1v1_keyframe() {
        let sent = Arc::new(Mutex::new(Vec::<String>::new()));
        let rec = sent.clone();
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": false,
            "host": true,
            "callId": 7,
            "localUid": 100,
            "peerUid": 200,
            "sessId": "0123456789abcdef0123456789abcd",
            "servers": [{"rtpaddr": "192.0.2.10:4200"}],
            "config": {}
        }));
        session.set_send(move |_, _, what| rec.lock().unwrap().push(what.to_owned()));
        session.active = true;
        session.active_server = Some(session.servers[0].clone());

        // Inbound video PLI triggers local keyframe request flag.
        let pli = crate::rtcp::build_pli(200, 100);
        let wrapped_pli = wrap_media_packet(MsgType::VIDEO_FEC, 0, &pli);
        session.receive_packet(&wrapped_pli, None);
        assert!(session.take_key_frame_request());

        // Large 1-1 keyframe (~20 chunks) paces VIDEO_BURST=8 immediately and drains rest.
        let mut big = vec![0_u8, 0, 0, 1, 0x65];
        big.resize(20 * crate::video::MAX_CHUNK + 5, 0x11);
        session.send_video_frame(true, &big, false, 100);
        assert!(session.is_sending_video());
        let first_burst = sent.lock().unwrap().len();
        assert_eq!(first_burst, VIDEO_BURST);
        while session.drain_video_queue(VIDEO_BURST) {}
        assert_eq!(sent.lock().unwrap().len(), 21);
    }

    #[test]
    fn group_member_render_width_upgrades_vidqual_layer() {
        let asks = Arc::new(Mutex::new(Vec::<u8>::new()));
        let rec = asks.clone();
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "host": false,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {
                "srtpKey": "",
                "maxWidth": 1280,
                "maxHeight": 720,
                "groupcall": {
                    "temporalNum": 3,
                    "spatialLayers": [
                        {"factorNum": 1, "factorDen": 4, "bitrateKbps": 150, "fps": 15},
                        {"factorNum": 1, "factorDen": 2, "bitrateKbps": 500, "fps": 20},
                        {"factorNum": 1, "factorDen": 1, "bitrateKbps": 1000, "fps": 24}
                    ]
                }
            }
        }));
        session.set_send(move |pkt, _, what| {
            if what == "VidQual" {
                let parsed = unwrap_packet(pkt).unwrap();
                rec.lock().unwrap().push(parsed.data[7]);
            }
        });
        session.active = true;
        session.active_server = Some(session.servers[0].clone());
        session.watch_member(55);
        assert_eq!(asks.lock().unwrap().as_slice(), &[0]);
        session.set_member_render_width(55, 420);
        assert_eq!(asks.lock().unwrap().as_slice(), &[0, 3]);
    }

    fn zavi_camera(packet: &[u8]) -> u8 {
        let parsed = unwrap_packet(packet).expect("zavi");
        let sess_len = u16::from_le_bytes([parsed.data[8], parsed.data[9]]) as usize;
        let extra_at = 10 + sess_len;
        parsed.data[extra_at + 4]
    }

    fn group_video_session(video: bool) -> MediaSession {
        let mut session = MediaSession::new();
        session.on_negotiated(&serde_json::json!({
            "group": true,
            "video": video,
            "host": true,
            "callId": 42,
            "localUid": 100,
            "sessId": "group-session",
            "servers": [{"rtpaddr": "192.0.2.10:3000"}],
            "config": {"srtpKey": ""}
        }));
        session.active_server = Some(session.servers[0].clone());
        session.server_token = 3;
        session.active = true;
        session
    }

    #[test]
    fn group_video_call_pings_the_camera_on() {
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let rec = sent.clone();
        let mut session = group_video_session(true);
        assert!(session.group_camera_on);
        session.set_send(move |pkt, _, what| {
            if what == "ZaviPing" {
                rec.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.send_ping();
        let pkts = sent.lock().unwrap();
        assert_eq!(pkts.len(), 1);
        assert_eq!(zavi_camera(&pkts[0]), 1);
    }

    #[test]
    fn group_voice_call_pings_the_camera_off() {
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let rec = sent.clone();
        let mut session = group_video_session(false);
        assert!(!session.group_camera_on);
        session.set_send(move |pkt, _, what| {
            if what == "ZaviPing" {
                rec.lock().unwrap().push(pkt.to_vec());
            }
        });
        session.send_ping();
        let pkts = sent.lock().unwrap();
        assert_eq!(pkts.len(), 1);
        assert_eq!(zavi_camera(&pkts[0]), 0);
    }

    #[test]
    fn group_camera_uses_a_random_clock_and_paces_a_keyframe() {
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let rec = sent.clone();
        let mut session = group_video_session(true);
        session.set_send(move |pkt, _, what| {
            if what == "video" {
                rec.lock().unwrap().push(pkt.to_vec());
            }
        });
        let frame = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xe0, 0x1e];
        session.send_video_frame(true, &frame, false, 5_000);
        session.send_video_frame(true, &frame, false, 6_000);
        let notes = session.take_notes();
        assert!(notes.iter().any(|line| {
            line.contains("group video out")
                && line.contains("plain 1")
                && line.contains("sfu 1")
                && line.contains("payload 000000016742e01e")
        }));
        let pkts = sent.lock().unwrap();
        assert_eq!(pkts.len(), 2);
        let open = |pkt: &[u8]| unwrap_packet(pkt).expect("video").payload;
        let first = open(&pkts[0]);
        let second = open(&pkts[1]);
        let seq1 = u16::from_be_bytes([first[2], first[3]]);
        let seq2 = u16::from_be_bytes([second[2], second[3]]);
        let ts1 = u32::from_be_bytes([first[4], first[5], first[6], first[7]]);
        let ts2 = u32::from_be_bytes([second[4], second[5], second[6], second[7]]);
        assert!(seq1 < 0x8000);
        assert_eq!(seq2, seq1.wrapping_add(1));
        assert!(ts1 <= 0x7fff_ffff);
        assert_eq!(ts2, ts1.wrapping_add(90_000));
        assert_ne!(first[0] & 0x10, 0);
        drop(pkts);

        let mut big = vec![0_u8, 0, 0, 1, 0x65];
        big.resize(20 * crate::video::MAX_CHUNK + 5, 0x11);
        let before = sent.lock().unwrap().len();
        session.send_video_frame(true, &big, false, 7_000);
        assert_eq!(sent.lock().unwrap().len() - before, VIDEO_BURST);
        while session.drain_video_queue(VIDEO_BURST) {}
        assert_eq!(sent.lock().unwrap().len() - before, 21);
    }

    #[test]
    fn group_video_plan_uses_full_temporal_bitrate_and_safe_key_interval() {
        let plan = group_video_plan(&serde_json::json!({
            "maxWidth": 1280,
            "maxHeight": 720,
            "maxBitrate": 1500,
            "keyFrameIntervalMs": 0,
            "groupcall": {
                "temporalNum": 3,
                "spatialLayers": [
                    {"factorNum": 1, "factorDen": 4, "bitrateKbps": 150, "fps": 15},
                    {"factorNum": 1, "factorDen": 2, "bitrateKbps": 800, "fps": 20},
                    {"factorNum": 1, "factorDen": 1, "bitrateKbps": 1200, "fps": 24}
                ]
            }
        }));
        assert_eq!(plan.encode.key_ms, 10_000);
        assert_eq!(plan.encode.width, 640);
        assert_eq!(plan.encode.height, 360);
        assert_eq!(plan.encode.fps, 20);
        assert_eq!(plan.encode.bitrate, 800_000);
        assert_eq!(plan.layers.len(), 9);
    }
}
