//! Outgoing video bitrate ladder selector (send-side bandwidth estimator).
//! Ported directly from the reference call implementation (`send-rate.js`).
//!
//! Uses three GoogCc-style signals—queuing delay from transport-cc, packet loss,
//! and missing feedback—plus receive-side packet loss (the phone's audio) since
//! both directions share Wi-Fi.

use crate::rtcp::parse_twcc;
use serde_json::Value;
use std::collections::BTreeMap;

/// Landscape dimensions for each rung (`res` is the short edge).
pub const SIZES: [(u32, [u32; 2]); 4] = [
    (240, [426, 240]),
    (360, [640, 360]),
    (480, [848, 480]),
    (720, [1280, 720]),
];
/// Default bitrate ladder when the server omits `bwProfiles`.
pub const DEFAULT_LADDER: [(u32, u32, u32, u32); 7] = [
    (1, 240, 8, 30),
    (2, 240, 10, 100),
    (3, 360, 15, 300),
    (4, 360, 20, 500),
    (5, 360, 24, 700),
    (6, 480, 24, 900),
    (7, 720, 24, 1100),
];
/// Default starting bitrate when `startupBitrate` is omitted.
pub const START_KBPS: u32 = 300;
/// Floor below which video quality is unusable.
pub const MIN_KBPS: u32 = 100;
/// Fallback key-frame interval when the server configures 0.
pub const SAFETY_KEY_MS: u32 = 10_000;
/// Minimum interval between consecutive step-downs, in ms.
pub const DOWN_GAP_MS: u64 = 2_000;
/// Consecutive clean seconds required before stepping up.
pub const UP_HOLD_S: u32 = 4;
/// Minimum backoff delay after stepping down, in ms.
pub const BACKOFF_MIN_MS: u64 = 10_000;
/// Maximum backoff delay after stepping down, in ms.
pub const BACKOFF_MAX_MS: u64 = 60_000;
/// Seconds without transport-cc while sending video before stepping down.
pub const FEEDBACK_TIMEOUT_S: u32 = 2;
/// Queuing delay threshold that blocks stepping up, in ms.
pub const QUEUE_HOLD_MS: f64 = 40.0;
/// Queuing delay threshold that forces stepping down, in ms.
pub const QUEUE_DOWN_MS: f64 = 150.0;
/// Receive-side packet loss threshold that blocks stepping up.
pub const DOWNLINK_HOLD: f64 = 0.02;
/// Receive-side packet loss threshold that forces stepping down.
pub const DOWNLINK_DOWN: f64 = 0.10;
/// Sliding window for minimum baseline delay, in seconds.
pub const BASE_WINDOW_S: u64 = 15;
/// Minimum packets covered in a one-second TWCC window to consider feedback valid.
pub const MIN_TWCC_PACKETS: u32 = 10;
/// Maximum number of sent-packet timestamps retained.
pub const SENT_KEEP: usize = 4000;
/// Diagnostic log interval, in ms.
pub const LOG_MS: u64 = 5000;

/// A single rung in the bitrate ladder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rung {
    /// Rung ID (server profile ID or spatial layer index).
    pub id: u32,
    /// Short edge for 1-1 rungs; 0 for group spatial layers.
    pub res: u32,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Target frame rate in frames per second.
    pub fps: u32,
    /// Target bitrate in kbps.
    pub kbps: u32,
}

/// Active outgoing video profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Target frame rate in frames per second.
    pub fps: u32,
    /// Target bitrate in bps.
    pub bitrate: u32,
    /// Key-frame interval in ms.
    pub key_ms: u32,
    /// Whether the camera must capture above 640 px wide to avoid upscaling.
    pub capture_big: bool,
    /// Active rung ID.
    pub rung: u32,
}

/// Result of a single `tick` evaluation.
#[derive(Clone, Debug, Default)]
pub struct Tick {
    /// New profile if the rung changed during this tick.
    pub profile: Option<Profile>,
    /// Diagnostic log lines emitted during this tick.
    pub logs: Vec<String>,
}

/// Cumulative adaptation counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Total packets reported via transport-cc.
    pub twcc_pkts: u64,
    /// Total lost packets reported via transport-cc.
    pub twcc_lost: u64,
    /// Total receiver report blocks processed.
    pub rr_blocks: u64,
    /// Total rung changes.
    pub changes: u64,
    /// Total step-downs.
    pub downs: u64,
}

#[derive(Default)]
struct Window {
    twcc_recv: u32,
    twcc_lost: u32,
    rr: Vec<f64>,
    delays: Vec<f64>,
}

fn num(v: Option<&Value>, d: f64) -> f64 {
    v.and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(d)
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Send-rate controller.
pub struct SendRate {
    /// Bitrate ladder rungs.
    pub ladder: Vec<Rung>,
    level: usize,
    ceiling: usize,
    arm_timeout: bool,
    fixed: bool,
    high_loss: f64,
    low_loss: f64,
    key_ms: u32,
    capture_big: bool,
    sent: BTreeMap<u16, u64>,
    win: Window,
    min_delays: Vec<(u64, f64)>,
    types: BTreeMap<String, u64>,
    good: u32,
    no_feedback: u32,
    last_down_at: Option<u64>,
    last_up_at: Option<u64>,
    backoff_ms: u64,
    last_log_at: u64,
    history: Vec<String>,
    seen_twcc: bool,
    last_loss: Option<f64>,
    /// Cumulative counters.
    pub totals: Totals,
    /// Local SSRC.
    pub local_ssrc: u32,
}

impl SendRate {
    fn build(ladder: Vec<Rung>, level: usize, ceiling: usize, arm_timeout: bool) -> Self {
        Self {
            capture_big: ladder.iter().any(|p| Self::dims(p).0 > 640),
            ladder,
            level,
            ceiling,
            arm_timeout,
            fixed: std::env::var("ZCALL_VIDEO_ADAPT").as_deref() == Ok("0"),
            high_loss: 0.10,
            low_loss: 0.02,
            key_ms: SAFETY_KEY_MS,
            sent: BTreeMap::new(),
            win: Window::default(),
            min_delays: Vec::new(),
            types: BTreeMap::new(),
            good: 0,
            no_feedback: 0,
            last_down_at: None,
            last_up_at: None,
            backoff_ms: BACKOFF_MIN_MS,
            last_log_at: 0,
            history: Vec::new(),
            seen_twcc: false,
            last_loss: None,
            totals: Totals::default(),
            local_ssrc: 0,
        }
    }

    /// Builds a 1-1 ladder from `zrtc_config`.
    ///
    /// # Errors
    ///
    /// Returns an error if the resulting ladder is empty (cannot happen with valid config).
    pub fn new_1v1(local_ssrc: u32, config: &Value) -> Result<(Self, Profile), String> {
        let max_w = num(config.get("maxWidth"), 1280.0);
        let max_h = num(config.get("maxHeight"), 720.0);
        let max_fps = num(config.get("codecMaxFps"), 24.0);
        let max_kbps = num(config.get("maxBitrate"), 1500.0);
        let profiles = config
            .get("bwProfiles")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty());
        let source: Vec<(u32, f64, f64, f64)> = match profiles {
            Some(list) => list
                .iter()
                .map(|p| {
                    (
                        p.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                        num(p.get("res"), 0.0),
                        num(p.get("fps"), 15.0),
                        num(p.get("bitrate"), 0.0),
                    )
                })
                .collect(),
            None => DEFAULT_LADDER
                .iter()
                .map(|(id, res, fps, kbps)| {
                    (*id, f64::from(*res), f64::from(*fps), f64::from(*kbps))
                })
                .collect(),
        };
        let mut ladder: Vec<Rung> = source
            .into_iter()
            .filter_map(|(id, res, fps, kbps)| {
                let size = SIZES.iter().find(|(r, _)| f64::from(*r) == res)?.1;
                if kbps < f64::from(MIN_KBPS)
                    || kbps > max_kbps
                    || f64::from(size[0]) > max_w
                    || f64::from(size[1]) > max_h
                {
                    return None;
                }
                Some(Rung {
                    id,
                    res: res as u32,
                    width: size[0],
                    height: size[1],
                    fps: fps.min(max_fps) as u32,
                    kbps: kbps as u32,
                })
            })
            .collect();
        ladder.sort_by_key(|p| p.kbps);
        if ladder.is_empty() {
            ladder = DEFAULT_LADDER
                .iter()
                .filter(|(_, _, _, kbps)| *kbps >= MIN_KBPS)
                .map(|(id, res, fps, kbps)| Rung {
                    id: *id,
                    res: *res,
                    width: SIZES.iter().find(|(r, _)| r == res).map_or(0, |s| s.1[0]),
                    height: SIZES.iter().find(|(r, _)| r == res).map_or(0, |s| s.1[1]),
                    fps: *fps,
                    kbps: *kbps,
                })
                .collect();
        }
        if ladder.is_empty() {
            return Err("send-rate ladder is empty".into());
        }
        let start_kbps = num(config.get("startupBitrate"), f64::from(START_KBPS));
        let mut start = 0usize;
        for (i, p) in ladder.iter().enumerate() {
            if f64::from(p.kbps) <= start_kbps {
                start = i;
            }
        }
        if let Ok(want) = std::env::var("ZCALL_VIDEO_RUNG") {
            if let Some(i) = ladder.iter().position(|p| p.id.to_string() == want) {
                start = i;
            }
        }
        let ceiling = ladder.len() - 1;
        let mut sr = Self::build(ladder, start, ceiling, false);
        sr.local_ssrc = local_ssrc;
        sr.high_loss = num(config.get("bweSendSideHighLoss"), 10.0) / 100.0;
        sr.low_loss = num(config.get("bweSendSideLowLoss"), 2.0) / 100.0;
        sr.key_ms = num(config.get("keyFrameIntervalMs"), f64::from(SAFETY_KEY_MS)) as u32;
        let profile = sr.profile();
        Ok((sr, profile))
    }

    /// Builds a group ladder from spatial layer descriptors.
    ///
    /// # Errors
    ///
    /// Returns an error if no valid layers are provided.
    pub fn new_group(
        local_ssrc: u32,
        layers: &[Value],
        start_index: usize,
    ) -> Result<(Self, Profile), String> {
        let mut ladder: Vec<Rung> = Vec::new();
        for (i, p) in layers.iter().enumerate() {
            let width = p.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
            let height = p.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
            let kbps = num(p.get("kbps"), 0.0);
            if width == 0 || height == 0 || kbps <= 0.0 {
                continue;
            }
            ladder.push(Rung {
                id: p.get("id").and_then(Value::as_u64).unwrap_or(i as u64) as u32,
                res: 0,
                width,
                height,
                fps: num(p.get("fps"), 15.0) as u32,
                kbps: kbps as u32,
            });
        }
        if ladder.is_empty() {
            return Err("group send-rate ladder is empty".into());
        }
        let level = start_index.min(ladder.len() - 1);
        let ceiling = level;
        let mut sr = Self::build(ladder, level, ceiling, true);
        sr.local_ssrc = local_ssrc;
        let profile = sr.profile();
        Ok((sr, profile))
    }

    fn dims(p: &Rung) -> (u32, u32) {
        if p.width > 0 && p.height > 0 {
            return (p.width, p.height);
        }
        SIZES
            .iter()
            .find(|(r, _)| *r == p.res)
            .map_or((0, 0), |s| (s.1[0], s.1[1]))
    }

    /// Human-readable label for the current rung.
    pub fn name(&self) -> String {
        self.name_of(&self.ladder[self.level])
    }

    fn name_of(&self, p: &Rung) -> String {
        let (w, h) = Self::dims(p);
        format!("id {} {}x{} {} fps {} kbps", p.id, w, h, p.fps, p.kbps)
    }

    /// Returns the active outgoing video profile.
    pub fn profile(&self) -> Profile {
        let p = &self.ladder[self.level];
        let (width, height) = Self::dims(p);
        Profile {
            width,
            height,
            fps: p.fps,
            bitrate: p.kbps * 1000,
            key_ms: self.key_ms,
            capture_big: self.capture_big,
            rung: p.id,
        }
    }

    /// Returns the current ladder index.
    pub fn level(&self) -> usize {
        self.level
    }

    /// Returns the most recently observed packet loss ratio (`0..1`) from TWCC/RR or downlink.
    pub fn observed_loss(&self) -> Option<f64> {
        self.last_loss
    }

    /// Records the send timestamp for an outgoing packet (transport-wide sequence number).
    pub fn on_sent(&mut self, tw_seq: u16, ms: u64) {
        self.sent.insert(tw_seq, ms);
        while self.sent.len() > SENT_KEEP {
            if let Some(first) = self.sent.keys().next().copied() {
                self.sent.remove(&first);
            }
        }
    }

    /// Processes compound RTCP packets received from the phone.
    pub fn on_rtcp(&mut self, kind: &str, buf: &[u8]) {
        let prefix = kind.chars().next().unwrap_or('a');
        let mut o = 0usize;
        while o + 4 <= buf.len() {
            if (buf[o] >> 6) != 2 {
                return;
            }
            let len = (usize::from(be16(buf, o + 2)) + 1) * 4;
            if o + len > buf.len() {
                return;
            }
            let pt = buf[o + 1];
            let fmt = buf[o] & 0x1f;
            *self.types.entry(format!("{prefix}{pt}/{fmt}")).or_insert(0) += 1;
            let pkt = &buf[o..o + len];
            if pt == 205 && fmt == 15 {
                self.on_twcc(pkt);
            } else if pt == 200 || pt == 201 {
                let first = if pt == 200 { 28 } else { 8 };
                for i in 0..usize::from(fmt) {
                    let b = first + 24 * i;
                    if b + 24 > len {
                        break;
                    }
                    if be32(pkt, b) == self.local_ssrc {
                        self.win.rr.push(f64::from(pkt[b + 4]) / 256.0);
                    }
                }
            }
            o += len;
        }
    }

    fn on_twcc(&mut self, pkt: &[u8]) {
        let Some(fb) = parse_twcc(pkt) else {
            return;
        };
        if fb.sender_ssrc == self.local_ssrc {
            return;
        }
        let mut arrival_ms = f64::from(fb.ref_units) * 64.0;
        let mut d = 0usize;
        for (i, st) in fb.status.iter().enumerate() {
            if *st == 0 {
                self.win.twcc_lost += 1;
                continue;
            }
            self.win.twcc_recv += 1;
            if *st == 1 {
                if let Some(delta) = fb.delta_q.get(d) {
                    arrival_ms += f64::from(*delta) / 4.0;
                }
                d += 1;
            } else if let Some(delta) = fb.delta_q.get(d) {
                arrival_ms += f64::from(*delta) / 4.0;
                d += 1;
            }
            let seq = fb.base.wrapping_add(i as u16);
            if let Some(&sent_ms) = self.sent.get(&seq) {
                self.win.delays.push(arrival_ms - sent_ms as f64);
            }
        }
    }

    /// Advances by one second: recomputes loss/queuing delay and adjusts the rung if needed.
    ///
    /// `down_loss` is the fraction of the phone's packets that we failed to receive (`0..1`).
    pub fn tick(&mut self, sending: bool, down_loss: Option<f64>, now: u64) -> Tick {
        let mut out = Tick::default();
        let w = std::mem::take(&mut self.win);
        let total = w.twcc_recv + w.twcc_lost;
        self.totals.twcc_pkts += u64::from(total);
        self.totals.twcc_lost += u64::from(w.twcc_lost);
        self.totals.rr_blocks += w.rr.len() as u64;
        let twcc = total >= MIN_TWCC_PACKETS;
        let (mut loss, mut src) = (None, "");
        if twcc {
            loss = Some(f64::from(w.twcc_lost) / f64::from(total));
            src = "twcc";
        } else if !w.rr.is_empty() {
            loss =
                w.rr.iter()
                    .copied()
                    .fold(None, |acc, v| Some(acc.map_or(v, |a: f64| a.max(v))));
            src = "rr";
        }
        let mut queue = None;
        if !w.delays.is_empty() {
            let mut sorted = w.delays.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            self.min_delays.push((now, sorted[0]));
            self.min_delays
                .retain(|(t, _)| now.saturating_sub(*t) <= BASE_WINDOW_S * 1000);
            let base = self
                .min_delays
                .iter()
                .map(|(_, v)| *v)
                .fold(f64::INFINITY, f64::min);
            let median = sorted[sorted.len() / 2];
            queue = Some((median - base).max(0.0).round());
        }
        self.no_feedback = if twcc { 0 } else { self.no_feedback + 1 };
        if twcc {
            self.seen_twcc = true;
        }
        let fmt_loss = |l: Option<f64>| match l {
            Some(v) => format!("{}", (v * 1000.0).round() / 10.0),
            None => "-".to_owned(),
        };
        let fmt_dl = match down_loss {
            Some(v) => format!("{}", (v * 1000.0).round() / 10.0),
            None => "-".to_owned(),
        };
        self.last_loss = match (loss, down_loss) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.history.push(format!(
            "{}/{:?}/{}",
            fmt_loss(loss),
            queue.map(|q| q as i64),
            fmt_dl
        ));

        if !self.fixed && sending {
            let pct = |x: f64| format!("{:.1} %", x * 100.0);
            let mut down: Option<String> = None;
            let mut hold = false;
            if self.no_feedback >= FEEDBACK_TIMEOUT_S && !(self.arm_timeout && !self.seen_twcc) {
                down = Some(format!("no transport-cc for {} s", self.no_feedback));
            } else if let Some(l) = loss {
                if l > self.high_loss {
                    down = Some(format!(
                        "loss {} ({}) > {} %",
                        pct(l),
                        src,
                        self.high_loss * 100.0
                    ));
                }
            }
            if down.is_none() {
                if let Some(q) = queue {
                    if q > QUEUE_DOWN_MS {
                        down = Some(format!("queue {q} ms > {QUEUE_DOWN_MS} ms"));
                    }
                }
            }
            if down.is_none() {
                if let Some(dl) = down_loss {
                    if dl > DOWNLINK_DOWN {
                        down = Some(format!("we lose {} of the phone's packets", pct(dl)));
                    }
                }
            }
            if down.is_none() {
                let low_loss = loss.is_some_and(|l| l >= self.low_loss);
                let high_queue = queue.is_some_and(|q| q > QUEUE_HOLD_MS);
                let downlink = down_loss.is_some_and(|d| d > DOWNLINK_HOLD);
                if !twcc || low_loss || high_queue || downlink {
                    hold = true;
                }
            }

            if let Some(why) = down {
                self.good = 0;
                if self.level > 0
                    && self
                        .last_down_at
                        .is_none_or(|t| now.saturating_sub(t) >= DOWN_GAP_MS)
                {
                    self.backoff_ms = if self
                        .last_up_at
                        .is_some_and(|t| now.saturating_sub(t) < 15_000)
                    {
                        (self.backoff_ms * 2).min(BACKOFF_MAX_MS)
                    } else {
                        BACKOFF_MIN_MS
                    };
                    self.last_down_at = Some(now);
                    self.totals.downs += 1;
                    self.set(self.level - 1, &why, &mut out);
                }
            } else if hold {
                self.good = 0;
            } else {
                self.good += 1;
                if self.good >= UP_HOLD_S
                    && self.level < self.ceiling
                    && self
                        .last_down_at
                        .is_none_or(|t| now.saturating_sub(t) >= self.backoff_ms)
                {
                    self.good = 0;
                    self.last_up_at = Some(now);
                    let why = format!(
                        "clean for {UP_HOLD_S} s (loss {}, queue {})",
                        pct(loss.unwrap_or(0.0)),
                        queue.map_or("-".to_owned(), |q| format!("{q} ms"))
                    );
                    self.set(self.level + 1, &why, &mut out);
                }
            }
        }

        if now.saturating_sub(self.last_log_at) >= LOG_MS {
            self.last_log_at = now;
            let types = if self.types.is_empty() {
                "none".to_owned()
            } else {
                self.types
                    .iter()
                    .map(|(k, v)| format!("{k}:{v}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            self.types.clear();
            let hist = self.history.join(" ");
            self.history.clear();
            let cam = if sending { "" } else { " (camera off)" };
            out.logs.push(format!(
                "send-rate: {}{cam}; loss%/queue ms/downlink% per s [{hist}]; rtcp from phone {types}",
                self.name()
            ));
        }
        out
    }

    fn set(&mut self, level: usize, why: &str, out: &mut Tick) {
        let from = self.name();
        self.level = level;
        self.totals.changes += 1;
        out.logs
            .push(format!("send-rate: {} -> {} ({why})", from, self.name()));
        out.profile = Some(self.profile());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builds_the_default_ladder() {
        let (sr, profile) = SendRate::new_1v1(7, &json!({})).expect("ladder");
        assert_eq!(sr.ladder.len(), 6); // id 1 (30 kbps) filtered out because < MIN_KBPS
                                        // default startupBitrate 300 -> 300 kbps rung (id 3).
        assert_eq!(profile.rung, 3);
        assert_eq!(profile.width, 640);
        assert_eq!(profile.height, 360);
        assert_eq!(profile.bitrate, 300_000);
        assert!(profile.capture_big);
    }

    #[test]
    fn filters_the_ladder_by_ceiling() {
        let config =
            json!({ "maxWidth": 640, "maxHeight": 360, "maxBitrate": 500, "startupBitrate": 100 });
        let (sr, profile) = SendRate::new_1v1(7, &config).expect("ladder");
        assert!(sr.ladder.iter().all(|p| p.kbps <= 500 && p.width <= 640));
        assert_eq!(profile.rung, 2); // 100 kbps
        assert!(!profile.capture_big);
    }

    fn twcc_feedback(present: &[u64]) -> Vec<u8> {
        let mut arrivals = std::collections::BTreeMap::new();
        for s in present {
            arrivals.insert(*s, 1000 + s * 4);
        }
        crate::rtcp::encode_twcc(0x99, 7, 0, 19, &arrivals, 0)
    }

    #[test]
    fn steps_down_on_loss() {
        let (mut sr, _) = SendRate::new_1v1(7, &json!({})).expect("ladder");
        assert_eq!(sr.level(), 1); // 300 kbps
                                   // 20 packets, only 12 arrived -> 40% > 10%.
        let present: Vec<u64> = (0..12).collect();
        sr.on_rtcp("audio", &twcc_feedback(&present));
        let tick = sr.tick(true, None, 2_000);
        assert_eq!(sr.level(), 0);
        assert!(tick.profile.is_some());
        assert!(tick.logs.iter().any(|l| l.contains("loss")));
    }

    #[test]
    fn climbs_after_clean_seconds() {
        let (mut sr, _) = SendRate::new_1v1(7, &json!({ "startupBitrate": 100 })).expect("ladder");
        let start = sr.level();
        let present: Vec<u64> = (0..20).collect();
        let mut climbed = false;
        for second in 0..12u64 {
            sr.on_rtcp("audio", &twcc_feedback(&present));
            let tick = sr.tick(true, None, second * 1000);
            if tick.profile.is_some() {
                climbed = true;
            }
        }
        assert!(climbed, "phải lên bậc sau vài giây sạch");
        assert!(sr.level() > start);
    }

    #[test]
    fn climbs_within_up_hold_when_clock_starts_at_zero() {
        let (mut sr, _) = SendRate::new_1v1(7, &json!({})).expect("ladder");
        let start = sr.level();
        let present: Vec<u64> = (0..20).collect();
        for second in 0..=u64::from(UP_HOLD_S) {
            sr.on_rtcp("audio", &twcc_feedback(&present));
            let _ = sr.tick(true, None, second * 1000);
        }
        assert!(sr.level() > start);
    }
}
