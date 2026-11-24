//! Receiver-side RTCP feedback sent back to the phone: transport-cc, SR/RR + SDES,
//! NACK, and PLI. Ported directly from the reference call implementation (`rtcp.js`).
//!
//! The phone feeds this into its send-side bandwidth estimator; without it,
//! video stays stuck at the lowest profile. Wire formats follow RFC 3550/4585
//! and draft-holmer-rmcat-transport-wide-cc-01.

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Transport-wide sequence number header extension ID (one-byte form).
pub const TWCC_EXT_ID: u8 = 5;
/// NTP epoch offset (1900 -> 1970) in seconds.
pub const NTP_EPOCH_OFFSET: u32 = 2_208_988_800;
/// Maximum number of packets reported in a single TWCC feedback packet.
pub const MAX_TWCC_PACKETS: u64 = 400;
/// Maximum sequence gap requested in one NACK burst.
pub const NACK_MAX_HOLE: u16 = 64;
/// NACK retransmission interval in ms.
pub const NACK_RETRY_MS: u64 = 40;
/// Maximum number of NACK attempts per lost packet.
pub const NACK_MAX_TRIES: u32 = 5;
/// Age after which a lost packet is forgotten, in ms.
pub const NACK_DROP_MS: u64 = 400;
/// Maximum number of lost packets tracked per stream.
pub const NACK_KEEP: usize = 128;

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_be_bytes());
}

fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

/// Reads the transport-wide sequence number from an RTP packet (extension ID 5), if present.
pub fn read_tw_seq(rtp: &[u8]) -> Option<u16> {
    if rtp.len() < 16 || (rtp[0] & 0x10) == 0 {
        return None;
    }
    let mut o = 12 + 4 * usize::from(rtp[0] & 0x0f);
    if o + 4 > rtp.len() || be16(rtp, o) != 0xbede {
        return None;
    }
    let end = rtp.len().min(o + 4 + 4 * usize::from(be16(rtp, o + 2)));
    o += 4;
    while o < end {
        let b = rtp[o];
        if b == 0 {
            o += 1;
            continue;
        }
        let id = b >> 4;
        let len = usize::from(b & 0x0f) + 1;
        if id == 15 {
            return None;
        }
        if id == TWCC_EXT_ID && len == 2 && o + 3 <= end {
            return Some(be16(rtp, o + 1));
        }
        o += 1 + len;
    }
    None
}

/// Transport-CC arrival recorder.
#[derive(Default)]
pub struct TwccRecorder {
    arrivals: BTreeMap<u64, u64>,
    top: Option<u64>,
    next: Option<u64>,
    fb_count: u8,
    /// Number of feedback packets emitted.
    pub sent: u64,
}

impl TwccRecorder {
    /// Records a transport-wide sequence number arriving at `now_ms`.
    pub fn add(&mut self, seq: u16, now_ms: u64) {
        let u = self.unwrap(u64::from(seq));
        if self.next.is_none() {
            self.next = Some(u);
        }
        if let Some(next) = self.next {
            if u < next || self.arrivals.contains_key(&u) {
                return;
            }
        }
        self.arrivals.insert(u, now_ms * 4);
        if self.top.is_none_or(|t| u > t) {
            self.top = Some(u);
        }
    }

    /// Returns whether there are unacknowledged arrivals to report.
    pub fn pending(&self) -> bool {
        match (self.next, self.top) {
            (Some(n), Some(t)) => t >= n,
            _ => false,
        }
    }

    /// Builds feedback packets covering everything received since the previous call.
    pub fn build(&mut self, sender_ssrc: u32, media_ssrc: u32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while self.pending() {
            let base = self.next.expect("pending có next");
            let end = self
                .top
                .expect("pending có top")
                .min(base + MAX_TWCC_PACKETS - 1);
            let any = (base..=end).any(|s| self.arrivals.contains_key(&s));
            if !any {
                self.next = Some(end + 1);
                continue;
            }
            out.push(encode_twcc(
                sender_ssrc,
                media_ssrc,
                base,
                end,
                &self.arrivals,
                self.fb_count,
            ));
            self.fb_count = self.fb_count.wrapping_add(1);
            for s in base..=end {
                self.arrivals.remove(&s);
            }
            self.next = Some(end + 1);
            self.sent += 1;
        }
        out
    }

    fn unwrap(&self, seq: u64) -> u64 {
        let Some(top) = self.top else {
            return seq;
        };
        let mut u = seq + (top - (top & 0xffff));
        if u > top && u - top > 0x8000 {
            u -= 0x10000;
        } else if top > u && top - u > 0x8000 {
            u += 0x10000;
        }
        u
    }
}

/// Encodes a single transport-cc feedback packet.
pub fn encode_twcc(
    sender_ssrc: u32,
    media_ssrc: u32,
    base: u64,
    end: u64,
    arrivals: &BTreeMap<u64, u64>,
    fb_count: u8,
) -> Vec<u8> {
    let count = (end - base + 1) as u16;
    let first_q = (base..=end)
        .find_map(|s| arrivals.get(&s).copied())
        .unwrap_or(0);
    let ref_units = first_q / 256;
    let mut prev_q = ref_units * 256;
    let mut symbols: Vec<u8> = Vec::new();
    let mut deltas: Vec<i32> = Vec::new();
    for s in base..=end {
        match arrivals.get(&s) {
            None => symbols.push(0),
            Some(&q) => {
                let d = q as i64 - prev_q as i64;
                prev_q = q;
                if (0..=255).contains(&d) {
                    symbols.push(1);
                    deltas.push(d as i32);
                } else {
                    symbols.push(2);
                    deltas.push(d.clamp(-32768, 32767) as i32);
                }
            }
        }
    }
    let mut chunks: Vec<u16> = Vec::new();
    let mut i = 0usize;
    while i < symbols.len() {
        let mut run = 1usize;
        while i + run < symbols.len() && symbols[i + run] == symbols[i] && run < 0x1fff {
            run += 1;
        }
        if run >= 8 {
            chunks.push(((u16::from(symbols[i])) << 13) | (run as u16));
            i += run;
            continue;
        }
        let mut v: u16 = 0xc000;
        for k in 0..7 {
            let sym = symbols.get(i + k).copied().unwrap_or(0);
            v |= u16::from(sym) << (12 - 2 * k);
        }
        chunks.push(v);
        i += 7;
    }
    let delta_bytes: usize = symbols
        .iter()
        .map(|s| match s {
            1 => 1,
            2 => 2,
            _ => 0,
        })
        .sum();
    let len = 20 + 2 * chunks.len() + delta_bytes;
    let padded = (len + 3) & !3;
    let mut b = vec![0u8; padded];
    b[0] = 0x8f;
    b[1] = 205;
    put16(&mut b, 2, (padded / 4 - 1) as u16);
    put32(&mut b, 4, sender_ssrc);
    put32(&mut b, 8, media_ssrc);
    put16(&mut b, 12, (base & 0xffff) as u16);
    put16(&mut b, 14, count);
    let r = (ref_units & 0xff_ffff) as u32;
    b[16] = (r >> 16) as u8;
    b[17] = (r >> 8) as u8;
    b[18] = r as u8;
    b[19] = fb_count;
    let mut o = 20;
    for c in &chunks {
        put16(&mut b, o, *c);
        o += 2;
    }
    let mut di = 0usize;
    for s in &symbols {
        match s {
            1 => {
                b[o] = deltas[di] as u8;
                o += 1;
                di += 1;
            }
            2 => {
                put16(&mut b, o, deltas[di] as u16);
                o += 2;
                di += 1;
            }
            _ => {}
        }
    }
    b
}

/// Parsed transport-cc feedback packet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TwccFeedback {
    /// Sender SSRC.
    pub sender_ssrc: u32,
    /// Media SSRC.
    pub media_ssrc: u32,
    /// Base sequence number.
    pub base: u16,
    /// Number of packets covered.
    pub count: u16,
    /// Reference time in 64 ms units.
    pub ref_units: u32,
    /// Feedback packet counter.
    pub fb_count: u8,
    /// Per-packet status symbols (0 = not received, 1 = small delta, 2 = large delta).
    pub status: Vec<u8>,
    /// Receive deltas in quarter-millisecond units.
    pub delta_q: Vec<i32>,
}

/// Parses a transport-cc feedback packet.
pub fn parse_twcc(b: &[u8]) -> Option<TwccFeedback> {
    if b.len() < 20 || (b[0] & 0x1f) != 15 || b[1] != 205 {
        return None;
    }
    let count = be16(b, 14);
    let mut out = TwccFeedback {
        sender_ssrc: be32(b, 4),
        media_ssrc: be32(b, 8),
        base: be16(b, 12),
        count,
        ref_units: (u32::from(b[16]) << 16) | (u32::from(b[17]) << 8) | u32::from(b[18]),
        fb_count: b[19],
        status: Vec::new(),
        delta_q: Vec::new(),
    };
    let mut o = 20usize;
    while out.status.len() < usize::from(count) && o + 2 <= b.len() {
        let c = be16(b, o);
        o += 2;
        if (c & 0x8000) == 0 {
            let sym = ((c >> 13) & 3) as u8;
            let run = c & 0x1fff;
            for _ in 0..run {
                if out.status.len() >= usize::from(count) {
                    break;
                }
                out.status.push(sym);
            }
        } else if c & 0x4000 != 0 {
            for k in 0..7 {
                if out.status.len() >= usize::from(count) {
                    break;
                }
                out.status.push(((c >> (12 - 2 * k)) & 3) as u8);
            }
        } else {
            for k in 0..14 {
                if out.status.len() >= usize::from(count) {
                    break;
                }
                out.status.push(((c >> (13 - k)) & 1) as u8);
            }
        }
    }
    for s in out.status.clone() {
        match s {
            1 if o < b.len() => {
                out.delta_q.push(i32::from(b[o]));
                o += 1;
            }
            2 if o + 2 <= b.len() => {
                out.delta_q.push(i32::from(be16(b, o) as i16));
                o += 2;
            }
            _ => {}
        }
    }
    Some(out)
}

/// Converts Unix milliseconds into an NTP `(msw, lsw)` timestamp pair.
pub fn ntp_now(now_ms: u64) -> (u32, u32) {
    let sec = now_ms / 1000;
    let frac = ((now_ms % 1000) as u128 * 4_294_967_296u128 / 1000) as u32;
    ((sec as u32).wrapping_add(NTP_EPOCH_OFFSET), frac)
}

/// A single RTCP reception report block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportBlock {
    /// Source SSRC.
    pub ssrc: u32,
    /// Fraction lost (0..255).
    pub fraction_lost: u8,
    /// Cumulative number of packets lost (signed 24-bit).
    pub cum_lost: i32,
    /// Extended highest sequence number received.
    pub ext_high_seq: u32,
    /// Interarrival jitter estimate.
    pub jitter: u32,
    /// Last SR timestamp (LSR).
    pub lsr: u32,
    /// Delay since last SR (DLSR).
    pub dlsr: u32,
}

fn write_report_block(b: &mut [u8], o: usize, blk: &ReportBlock) {
    put32(b, o, blk.ssrc);
    b[o + 4] = blk.fraction_lost;
    let v = (blk.cum_lost as u32) & 0xff_ffff;
    b[o + 5] = (v >> 16) as u8;
    b[o + 6] = (v >> 8) as u8;
    b[o + 7] = v as u8;
    put32(b, o + 8, blk.ext_high_seq);
    put32(b, o + 12, blk.jitter);
    put32(b, o + 16, blk.lsr);
    put32(b, o + 20, blk.dlsr);
}

/// Sender report (PT 200).
pub fn build_sr(
    ssrc: u32,
    now_ms: u64,
    rtp_ts: u32,
    packets: u32,
    octets: u32,
    blocks: &[ReportBlock],
) -> Vec<u8> {
    let mut b = vec![0u8; 28 + 24 * blocks.len()];
    b[0] = 0x80 | (blocks.len() as u8 & 0x1f);
    b[1] = 200;
    let words = (b.len() / 4 - 1) as u16;
    put16(&mut b, 2, words);
    put32(&mut b, 4, ssrc);
    let (msw, lsw) = ntp_now(now_ms);
    put32(&mut b, 8, msw);
    put32(&mut b, 12, lsw);
    put32(&mut b, 16, rtp_ts);
    put32(&mut b, 20, packets);
    put32(&mut b, 24, octets);
    for (i, blk) in blocks.iter().enumerate() {
        write_report_block(&mut b, 28 + 24 * i, blk);
    }
    b
}

/// Receiver report (PT 201).
pub fn build_rr(ssrc: u32, blocks: &[ReportBlock]) -> Vec<u8> {
    let mut b = vec![0u8; 8 + 24 * blocks.len()];
    b[0] = 0x80 | (blocks.len() as u8 & 0x1f);
    b[1] = 201;
    let words = (b.len() / 4 - 1) as u16;
    put16(&mut b, 2, words);
    put32(&mut b, 4, ssrc);
    for (i, blk) in blocks.iter().enumerate() {
        write_report_block(&mut b, 8 + 24 * i, blk);
    }
    b
}

/// Builds an SDES packet with an empty CNAME (12 bytes, matching ZaloCall).
pub fn build_sdes(ssrc: u32) -> Vec<u8> {
    let mut b = vec![0u8; 12];
    b[0] = 0x81;
    b[1] = 202;
    put16(&mut b, 2, 2);
    put32(&mut b, 4, ssrc);
    b[8] = 1;
    b
}

/// PLI (PSFB fmt 1).
pub fn build_pli(sender_ssrc: u32, media_ssrc: u32) -> Vec<u8> {
    let mut b = vec![0u8; 12];
    b[0] = 0x81;
    b[1] = 206;
    put16(&mut b, 2, 2);
    put32(&mut b, 4, sender_ssrc);
    put32(&mut b, 8, media_ssrc);
    b
}

/// REMB (Receiver Estimated Maximum Bitrate, PSFB `PT=206, FMT=15`,
/// draft-alvestrand-rmcat-remb). Tells the peer/phone how much inbound video
/// bandwidth we can cleanly receive so it climbs to 720p smoothly.
pub fn build_remb(sender_ssrc: u32, media_ssrc: u32, bitrate_bps: u64) -> Vec<u8> {
    let mut mantissa = bitrate_bps;
    let mut exp: u8 = 0;
    while mantissa > 0x3_ffff && exp < 63 {
        mantissa >>= 1;
        exp += 1;
    }
    let mantissa = (mantissa & 0x3_ffff) as u32;
    let mut b = vec![0u8; 24];
    b[0] = 0x8f; // V=2, P=0, FMT=15 (AFB)
    b[1] = 206; // PSFB
    put16(&mut b, 2, 5); // 24 bytes / 4 - 1
    put32(&mut b, 4, sender_ssrc);
    put32(&mut b, 8, 0); // media SSRC always 0 per REMB spec
    b[12..16].copy_from_slice(b"REMB");
    b[16] = 1; // Num SSRCs
    b[17] = (exp << 2) | ((mantissa >> 16) as u8 & 0x03);
    b[18] = ((mantissa >> 8) & 0xff) as u8;
    b[19] = (mantissa & 0xff) as u8;
    put32(&mut b, 20, media_ssrc);
    b
}

/// Parses a single REMB packet (`PT=206, FMT=15`, `"REMB"`), returning
/// `(sender_ssrc, media_ssrcs, bitrate_bps)`.
pub fn parse_remb(pkt: &[u8]) -> Option<(u32, Vec<u32>, u64)> {
    if pkt.len() < 20 || (pkt[0] >> 6) != 2 || (pkt[0] & 0x1f) != 15 || pkt[1] != 206 {
        return None;
    }
    if &pkt[12..16] != b"REMB" {
        return None;
    }
    let count = usize::from(pkt[16]);
    if pkt.len() < 20 + count * 4 {
        return None;
    }
    let exp = u32::from(pkt[17] >> 2);
    let mantissa = (u64::from(pkt[17] & 0x03) << 16) | (u64::from(pkt[18]) << 8) | u64::from(pkt[19]);
    let bitrate_bps = mantissa.checked_shl(exp).unwrap_or(u64::MAX);
    let mut ssrcs = Vec::with_capacity(count);
    for i in 0..count {
        ssrcs.push(be32(pkt, 20 + i * 4));
    }
    Some((be32(pkt, 4), ssrcs, bitrate_bps))
}

/// Generic NACK (RTPFB fmt 1): encodes PID + BLP for each span of up to 17 sequence numbers.
pub fn build_nack(sender_ssrc: u32, media_ssrc: u32, seqs: &[u16]) -> Vec<u8> {
    let mut items: Vec<(u16, u16)> = Vec::new();
    let mut i = 0usize;
    while i < seqs.len() {
        let pid = seqs[i];
        let mut blp = 0u16;
        let mut j = i + 1;
        while j < seqs.len() {
            let d = seqs[j].wrapping_sub(pid);
            if d == 0 || d > 16 {
                break;
            }
            blp |= 1 << (d - 1);
            j += 1;
        }
        items.push((pid, blp));
        i = j;
    }
    let mut b = vec![0u8; 12 + 4 * items.len()];
    b[0] = 0x81;
    b[1] = 205;
    let words = (b.len() / 4 - 1) as u16;
    put16(&mut b, 2, words);
    put32(&mut b, 4, sender_ssrc);
    put32(&mut b, 8, media_ssrc);
    for (k, (pid, blp)) in items.iter().enumerate() {
        put16(&mut b, 12 + 4 * k, *pid);
        put16(&mut b, 14 + 4 * k, *blp);
    }
    b
}

/// Parsed RFC 4585 Generic NACK (`PT = 205`, `FMT = 1`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NackRequest {
    /// SSRC of the feedback sender.
    pub sender_ssrc: u32,
    /// SSRC of the media stream whose packets are requested.
    pub media_ssrc: u32,
    /// Requested RTP sequence numbers in FCI order.
    pub seqs: Vec<u16>,
}

/// Parses a single RTCP Generic NACK packet (`PT = 205`, `FMT = 1`).
pub fn parse_nack(pkt: &[u8]) -> Option<NackRequest> {
    if pkt.len() < 16 || (pkt[0] >> 6) != 2 || (pkt[0] & 0x1f) != 1 || pkt[1] != 205 {
        return None;
    }
    let words = usize::from(be16(pkt, 2));
    let len = (words + 1) * 4;
    if len < 16 || len > pkt.len() {
        return None;
    }
    let sender_ssrc = be32(pkt, 4);
    let media_ssrc = be32(pkt, 8);
    let mut seqs = Vec::new();
    let mut o = 12;
    while o + 4 <= len {
        let pid = be16(pkt, o);
        let blp = be16(pkt, o + 2);
        seqs.push(pid);
        for bit in 0..16u16 {
            if (blp & (1 << bit)) != 0 {
                seqs.push(pid.wrapping_add(bit + 1));
            }
        }
        o += 4;
    }
    Some(NackRequest {
        sender_ssrc,
        media_ssrc,
        seqs,
    })
}

/// Extracts all RFC 4585 Generic NACK packets from a compound RTCP buffer.
pub fn parse_nacks_in_compound(buf: &[u8]) -> Vec<NackRequest> {
    let mut out = Vec::new();
    let mut o = 0;
    while o + 4 <= buf.len() {
        if (buf[o] >> 6) != 2 {
            break;
        }
        let len = (usize::from(be16(buf, o + 2)) + 1) * 4;
        if len < 4 || o + len > buf.len() {
            break;
        }
        if buf[o + 1] == 205 && (buf[o] & 0x1f) == 1 {
            if let Some(req) = parse_nack(&buf[o..o + len]) {
                out.push(req);
            }
        }
        o += len;
    }
    out
}

#[derive(Default)]
struct Stream {
    highest: u16,
    miss: BTreeMap<u16, (u64, u32)>,
}

/// Tracks lost video packets per SSRC and schedules NACK requests.
#[derive(Default)]
pub struct NackTracker {
    streams: HashMap<u32, Stream>,
}

impl NackTracker {
    /// Records a received sequence number and returns any `(ssrc, seqs)` batches due for NACK.
    pub fn note(&mut self, ssrc: u32, seq: u16, now: u64) -> Vec<(u32, Vec<u16>)> {
        let st = self.streams.entry(ssrc).or_insert_with(|| Stream {
            highest: seq,
            miss: BTreeMap::new(),
        });
        let ahead = seq.wrapping_sub(st.highest);
        if ahead == 0 {
            return self.due(ssrc, now);
        }
        if ahead >= 0x8000 {
            st.miss.remove(&seq);
            return self.due(ssrc, now);
        }
        if ahead > NACK_MAX_HOLE {
            st.highest = seq;
            st.miss.clear();
            return Vec::new();
        }
        for d in 1..ahead {
            let missing = st.highest.wrapping_add(d);
            st.miss.entry(missing).or_insert((now, 0));
        }
        st.highest = seq;
        st.miss.remove(&seq);
        while st.miss.len() > NACK_KEEP {
            if let Some(first) = st.miss.keys().next().copied() {
                st.miss.remove(&first);
            }
        }
        self.due(ssrc, now)
    }

    fn due(&mut self, ssrc: u32, now: u64) -> Vec<(u32, Vec<u16>)> {
        let Some(st) = self.streams.get_mut(&ssrc) else {
            return Vec::new();
        };
        let mut seqs: Vec<u16> = Vec::new();
        let mut drop: Vec<u16> = Vec::new();
        for (&seq, info) in st.miss.iter_mut() {
            if now.saturating_sub(info.0) > NACK_DROP_MS {
                drop.push(seq);
                continue;
            }
            if info.1 >= NACK_MAX_TRIES {
                continue;
            }
            if now < info.0 + u64::from(info.1) * NACK_RETRY_MS {
                continue;
            }
            info.1 += 1;
            seqs.push(seq);
        }
        for seq in drop {
            st.miss.remove(&seq);
        }
        if seqs.is_empty() {
            return Vec::new();
        }
        let high = st.highest;
        seqs.sort_by_key(|s| std::cmp::Reverse(high.wrapping_sub(*s)));
        vec![(ssrc, seqs)]
    }
}

/// RTP reception statistics (RFC 3550 A.1/A.3/6.4.1).
pub struct RxStats {
    clock_hz: u64,
    /// Source SSRC.
    pub ssrc: u32,
    /// Whether the first packet has been received.
    pub started: bool,
    base_seq: u32,
    max_seq: u16,
    cycles: u32,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
    jitter: f64,
    transit: Option<f64>,
    seen: BTreeSet<u32>,
    lsr: u32,
    lsr_at: u64,
}

impl RxStats {
    /// Creates reception counters for the given RTP `clock_hz`.
    pub fn new(clock_hz: u64) -> Self {
        Self {
            clock_hz,
            ssrc: 0,
            started: false,
            base_seq: 0,
            max_seq: 0,
            cycles: 0,
            received: 0,
            expected_prior: 0,
            received_prior: 0,
            jitter: 0.0,
            transit: None,
            seen: BTreeSet::new(),
            lsr: 0,
            lsr_at: 0,
        }
    }

    /// Records an incoming RTP packet.
    pub fn on_packet(&mut self, rtp: &[u8], now_ms: u64) {
        if rtp.len() < 12 {
            return;
        }
        let seq = be16(rtp, 2);
        let ts = be32(rtp, 4);
        if !self.started {
            self.started = true;
            self.ssrc = be32(rtp, 8);
            self.base_seq = u32::from(seq);
            self.max_seq = seq;
        } else {
            let d = seq.wrapping_sub(self.max_seq);
            if d != 0 && d < 0x8000 {
                if seq < self.max_seq {
                    self.cycles += 0x10000;
                }
                self.max_seq = seq;
            }
        }
        let ext = self.cycles + u32::from(seq);
        if !self.seen.insert(ext) {
            return;
        }
        if self.seen.len() > 512 {
            if let Some(first) = self.seen.iter().next().copied() {
                self.seen.remove(&first);
            }
        }
        self.received += 1;
        let arrival = (now_ms as f64 * self.clock_hz as f64) / 1000.0;
        let transit = arrival - f64::from(ts);
        if let Some(prev) = self.transit {
            let dd = (transit - prev).abs();
            self.jitter += (dd - self.jitter) / 16.0;
        }
        self.transit = Some(transit);
    }

    /// Records an incoming peer Sender Report (SR).
    pub fn on_sr(&mut self, buf: &[u8], now_ms: u64) {
        if buf.len() < 28 {
            return;
        }
        let msw = be32(buf, 8) & 0xffff;
        self.lsr = (msw << 16) | (be32(buf, 12) >> 16);
        self.lsr_at = now_ms;
    }

    /// Builds the current reception report block.
    pub fn block(&mut self, now_ms: u64) -> Option<ReportBlock> {
        if !self.started {
            return None;
        }
        let ext_high = self.cycles + u32::from(self.max_seq);
        let expected = ext_high.wrapping_sub(self.base_seq).wrapping_add(1);
        let lost = expected as i64 - self.received as i64;
        let exp_int = expected.wrapping_sub(self.expected_prior);
        let rec_int = self.received.wrapping_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_int = exp_int as i64 - rec_int as i64;
        let fraction = if exp_int == 0 || lost_int <= 0 {
            0
        } else {
            ((lost_int * 256) / exp_int as i64).min(255) as u8
        };
        Some(ReportBlock {
            ssrc: self.ssrc,
            fraction_lost: fraction,
            cum_lost: lost.clamp(-0x80_0000, 0x7f_ffff) as i32,
            ext_high_seq: ext_high,
            jitter: self.jitter.round() as u32,
            lsr: self.lsr,
            // DLSR is the delay since the peer's sender report. The two
            // clocks have disagreed (unix time versus time since the socket
            // opened), and a plain subtract panics the call thread in debug.
            dlsr: if self.lsr_at != 0 {
                let delay = now_ms.saturating_sub(self.lsr_at);
                ((delay as u128 * 65536 / 1000).min(u128::from(u32::MAX))) as u32
            } else {
                0
            },
        })
    }

    /// Read-only `(expected, received)` snapshot for uplink adaptation without
    /// advancing the RR interval counters.
    pub fn reception_counts(&self) -> Option<(u32, u32)> {
        if !self.started {
            return None;
        }
        let ext_high = self.cycles + u32::from(self.max_seq);
        let expected = ext_high.wrapping_sub(self.base_seq).wrapping_add(1);
        Some((expected, self.received))
    }
}

/// Initial inbound REMB estimate (1.5 Mbps, allows 720p24/30 climb).
const REMB_INITIAL_BPS: u64 = 1_500_000;
/// Floor for inbound REMB estimate (250 kbps).
const REMB_MIN_BPS: u64 = 250_000;
/// Ceiling for inbound REMB estimate (2.5 Mbps).
const REMB_MAX_BPS: u64 = 2_500_000;

/// Aggregated RTCP feedback state for a single call.
pub struct RtcpFeedback {
    /// Local SSRC.
    pub local_ssrc: u32,
    twcc: TwccRecorder,
    audio: RxStats,
    video: RxStats,
    /// Peer media SSRC.
    pub peer_ssrc: u32,
    /// Pending PLI/FIR received from peer or SFU targeting our video SSRC.
    key_frame_requested: bool,
    /// Estimated receive-side video bitrate reported in RTCP REMB.
    remb_bps: u64,
    /// Feedback counters.
    pub counters: RtcpCounters,
}

/// RTCP feedback counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RtcpCounters {
    /// Number of transport-cc feedback packets built.
    pub twcc: u64,
    /// Number of SR/RR reports built.
    pub reports: u64,
    /// Number of PLI requests built.
    pub pli: u64,
    /// Number of NACK requests built.
    pub nack: u64,
    /// Number of REMB feedback packets built.
    pub remb: u64,
}

impl RtcpFeedback {
    /// Creates feedback state using our local SSRC.
    pub fn new(local_ssrc: u32) -> Self {
        Self {
            local_ssrc,
            twcc: TwccRecorder::default(),
            audio: RxStats::new(16_000),
            video: RxStats::new(90_000),
            peer_ssrc: 0,
            key_frame_requested: false,
            remb_bps: REMB_INITIAL_BPS,
            counters: RtcpCounters::default(),
        }
    }

    /// Records an incoming RTP packet (`video` = true for video streams).
    pub fn on_rtp(&mut self, video: bool, rtp: &[u8], now_ms: u64) {
        if rtp.len() < 12 {
            return;
        }
        if self.peer_ssrc == 0 {
            self.peer_ssrc = be32(rtp, 8);
        }
        if let Some(tw) = read_tw_seq(rtp) {
            self.twcc.add(tw, now_ms);
        }
        if video {
            self.video.on_packet(rtp, now_ms);
        } else {
            self.audio.on_packet(rtp, now_ms);
        }
    }

    /// Parses peer RTCP: records SR/LSR timestamps and notes PLI/FIR key-frame requests.
    pub fn on_rtcp(&mut self, video: bool, buf: &[u8], now_ms: u64) {
        let local_ssrc = self.local_ssrc;
        let st = if video {
            &mut self.video
        } else {
            &mut self.audio
        };
        let mut o = 0usize;
        while o + 4 <= buf.len() {
            if (buf[o] >> 6) != 2 {
                return;
            }
            let len = (usize::from(be16(buf, o + 2)) + 1) * 4;
            if len < 4 || o + len > buf.len() {
                return;
            }
            let pt = buf[o + 1];
            let fmt = buf[o] & 0x1f;
            if pt == 200 {
                st.on_sr(&buf[o..o + len], now_ms);
            } else if video && pt == 206 {
                let target = if fmt == 1 && len >= 12 {
                    Some(be32(buf, o + 8))
                } else if fmt == 4 && len >= 16 {
                    Some(be32(buf, o + 12))
                } else {
                    None
                };
                if let Some(media_ssrc) = target {
                    if media_ssrc == local_ssrc || media_ssrc == 0 || local_ssrc == 0 {
                        self.key_frame_requested = true;
                    }
                }
            }
            o += len;
        }
    }

    /// Consumes and clears a pending peer/SFU key-frame request (PLI or FIR).
    pub fn take_key_frame_request(&mut self) -> bool {
        std::mem::take(&mut self.key_frame_requested)
    }

    /// Read-only `(expected, received)` counters for peer audio packets.
    pub fn audio_reception_counts(&self) -> Option<(u32, u32)> {
        self.audio.reception_counts()
    }

    /// Builds transport-cc feedback packets ready to send.
    pub fn twcc_packets(&mut self) -> Vec<Vec<u8>> {
        if self.peer_ssrc == 0 {
            return Vec::new();
        }
        let pk = self.twcc.build(self.local_ssrc, self.peer_ssrc);
        self.counters.twcc += pk.len() as u64;
        pk
    }

    /// Builds an audio channel report: SR + SDES.
    pub fn audio_report(&mut self, now_ms: u64, rtp_ts: u32, packets: u32, octets: u32) -> Vec<u8> {
        let blk = self.audio.block(now_ms);
        self.counters.reports += 1;
        let blocks: Vec<ReportBlock> = blk.into_iter().collect();
        let mut out = build_sr(self.local_ssrc, now_ms, rtp_ts, packets, octets, &blocks);
        out.extend_from_slice(&build_sdes(self.local_ssrc));
        out
    }

    /// Builds a video channel report: RR + SDES + REMB, or `None` if no video has arrived yet.
    pub fn video_report(&mut self, now_ms: u64) -> Option<Vec<u8>> {
        let blk = self.video.block(now_ms)?;
        // Adapt inbound REMB estimate smoothly from interval packet loss.
        let loss_frac = f64::from(blk.fraction_lost) / 256.0;
        if loss_frac > 0.10 {
            self.remb_bps = ((self.remb_bps as f64 * (1.0 - 0.5 * loss_frac)) as u64)
                .clamp(REMB_MIN_BPS, REMB_MAX_BPS);
        } else if loss_frac < 0.02 {
            self.remb_bps = ((self.remb_bps as f64 * 1.08) as u64)
                .clamp(REMB_MIN_BPS, REMB_MAX_BPS);
        }
        self.counters.reports += 1;
        self.counters.remb += 1;
        let mut out = build_rr(self.local_ssrc, &[blk.clone()]);
        out.extend_from_slice(&build_sdes(self.local_ssrc));
        out.extend_from_slice(&build_remb(self.local_ssrc, blk.ssrc, self.remb_bps));
        Some(out)
    }

    /// Builds a PLI key-frame request, or `None` if no video has arrived yet.
    pub fn pli(&mut self) -> Option<Vec<u8>> {
        if !self.video.started {
            return None;
        }
        self.counters.pli += 1;
        Some(build_pli(self.local_ssrc, self.video.ssrc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtp_with_tw(seq_tw: u16) -> Vec<u8> {
        let mut rtp = vec![0x90, 0x70, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        rtp.extend_from_slice(&[0xbe, 0xde, 0x00, 0x01]);
        rtp.push(0x51); // id 5, len 2
        rtp.extend_from_slice(&seq_tw.to_be_bytes());
        rtp.push(0x00); // pad to 4-byte boundary
        rtp
    }

    #[test]
    fn reads_the_transport_wide_sequence() {
        assert_eq!(read_tw_seq(&rtp_with_tw(0x1234)), Some(0x1234));
        assert_eq!(read_tw_seq(&[0x80, 0x00]), None);
    }

    #[test]
    fn twcc_round_trips() {
        let mut arrivals = BTreeMap::new();
        arrivals.insert(10u64, 400u64);
        arrivals.insert(11u64, 404u64);
        arrivals.insert(13u64, 412u64);
        let bytes = encode_twcc(0x1111, 0x2222, 10, 13, &arrivals, 3);
        let parsed = parse_twcc(&bytes).expect("parse");
        assert_eq!(parsed.base, 10);
        assert_eq!(parsed.count, 4);
        assert_eq!(parsed.fb_count, 3);
        assert_eq!(parsed.status, vec![1, 1, 0, 1]);
        assert_eq!(parsed.delta_q, vec![144, 4, 8]);
    }

    #[test]
    fn recorder_reports_only_arrivals() {
        let mut rec = TwccRecorder::default();
        rec.add(5, 100);
        rec.add(6, 105);
        rec.add(9, 120);
        assert!(rec.pending());
        let frames = rec.build(1, 2);
        assert_eq!(frames.len(), 1);
        let parsed = parse_twcc(&frames[0]).expect("parse");
        assert_eq!(parsed.base, 5);
        assert_eq!(parsed.count, 5);
        assert_eq!(parsed.status, vec![1, 1, 0, 0, 1]);
        assert!(!rec.pending());
    }

    #[test]
    fn builds_reports_and_requests() {
        let sr = build_sr(7, 1000, 320, 10, 500, &[]);
        assert_eq!(sr.len(), 28);
        assert_eq!(sr[1], 200);
        let rr = build_rr(7, &[ReportBlock::default()]);
        assert_eq!(rr.len(), 32);
        assert_eq!(rr[1], 201);
        let sdes = build_sdes(7);
        assert_eq!(sdes.len(), 12);
        assert_eq!(sdes[1], 202);
        let pli = build_pli(7, 9);
        assert_eq!(pli[1], 206);
        assert_eq!(be32(&pli, 8), 9);
    }

    #[test]
    fn builds_nack_items() {
        let nack = build_nack(1, 2, &[10, 11, 13]);
        assert_eq!(nack.len(), 16); // 1 item
        assert_eq!(be16(&nack, 12), 10);
        assert_eq!(be16(&nack, 14), 0b101);
    }

    #[test]
    fn nack_tracker_detects_a_hole() {
        let mut tracker = NackTracker::default();
        assert!(tracker.note(5, 100, 0).is_empty());
        // Jump to 104: missing 101, 102, 103.
        let due = tracker.note(5, 104, 10);
        assert_eq!(due.len(), 1);
        let (ssrc, seqs) = &due[0];
        assert_eq!(*ssrc, 5);
        assert_eq!(seqs, &vec![101, 102, 103]);
        // Before 40 ms: do not re-request yet.
        assert!(tracker.note(5, 105, 20).is_empty());
        // After 40 ms: re-request.
        assert_eq!(tracker.note(5, 106, 60)[0].1.len(), 3);
    }

    #[test]
    fn rx_stats_counts_loss_and_jitter() {
        let mut stats = RxStats::new(16_000);
        for seq in 0..10u16 {
            let mut rtp = vec![0x80, 0x70];
            rtp.extend_from_slice(&seq.to_be_bytes());
            rtp.extend_from_slice(&(u32::from(seq) * 320).to_be_bytes());
            rtp.extend_from_slice(&0xabcd_u32.to_be_bytes());
            rtp.extend_from_slice(&[0; 4]);
            stats.on_packet(&rtp, u64::from(seq) * 20);
        }
        let blk = stats.block(200).expect("block");
        assert_eq!(blk.ssrc, 0xabcd);
        assert_eq!(blk.cum_lost, 0);
        assert_eq!(blk.fraction_lost, 0);
    }

    #[test]
    fn sender_report_from_the_future_does_not_panic() {
        let mut stats = RxStats::new(90_000);
        let mut rtp = vec![0x80, 0x7a];
        rtp.extend_from_slice(&1u16.to_be_bytes());
        rtp.extend_from_slice(&90_000u32.to_be_bytes());
        rtp.extend_from_slice(&0x1111u32.to_be_bytes());
        stats.on_packet(&rtp, 5_000);
        stats.on_sr(&[0_u8; 28], 1_700_000_000_000);
        let early = stats.block(5_000).expect("block");
        assert_eq!(early.dlsr, 0);
        let later = stats.block(1_700_000_001_000).expect("block");
        assert_eq!(later.dlsr, 65_536);
    }

    #[test]
    fn consumes_inbound_pli_and_fir_for_local_ssrc() {
        let mut fb = RtcpFeedback::new(42);
        let other_pli = build_pli(99, 777);
        fb.on_rtcp(true, &other_pli, 100);
        assert!(!fb.take_key_frame_request());

        let ours_pli = build_pli(99, 42);
        fb.on_rtcp(true, &ours_pli, 101);
        assert!(fb.take_key_frame_request());
        assert!(!fb.take_key_frame_request());

        // Compound RR + PSFB FIR (fmt 4, len=3 => 16 bytes)
        let mut compound = build_rr(99, &[]);
        compound.extend_from_slice(&[0x84, 206, 0, 3]);
        compound.extend_from_slice(&99_u32.to_be_bytes());
        compound.extend_from_slice(&0_u32.to_be_bytes());
        compound.extend_from_slice(&42_u32.to_be_bytes());
        fb.on_rtcp(true, &compound, 102);
        assert!(fb.take_key_frame_request());
    }

    #[test]
    fn parses_generic_nack_and_compound_round_trip() {
        let seqs = vec![65534_u16, 65535, 0, 2, 25];
        let nack = build_nack(0x11, 0x22, &seqs);
        let parsed = parse_nack(&nack).expect("parse nack");
        assert_eq!(parsed.sender_ssrc, 0x11);
        assert_eq!(parsed.media_ssrc, 0x22);
        assert_eq!(parsed.seqs, seqs);

        let mut compound = build_rr(0x11, &[]);
        compound.extend_from_slice(&nack);
        let all = parse_nacks_in_compound(&compound);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0], parsed);
    }

    #[test]
    fn remb_round_trips_and_attaches_to_video_report() {
        let pkt = build_remb(0x1122, 0x3344, 1_500_000);
        let (sender, ssrcs, bps) = parse_remb(&pkt).expect("parse remb");
        assert_eq!(sender, 0x1122);
        assert_eq!(ssrcs, vec![0x3344]);
        assert!((bps as i64 - 1_500_000).abs() <= 32, "got {bps}");

        let mut fb = RtcpFeedback::new(7);
        let mut rtp = vec![0x80, 98, 0, 1, 0, 0, 0, 90, 0, 0, 0, 99];
        rtp.extend_from_slice(&[0; 8]);
        fb.on_rtp(true, &rtp, 1_000);
        let report = fb.video_report(2_000).expect("video report");
        assert!(report.windows(4).any(|w| w == b"REMB"));
        assert_eq!(fb.counters.remb, 1);
    }
}
