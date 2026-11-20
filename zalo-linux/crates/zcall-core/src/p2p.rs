//! P2P state machine (NAT hole punching between two clients), ported from the
//! `_p2pStart` / `_onP2p` / `_p2pUp` logic of `zrtc-media.js`. Packet builders
//! live in `zrtp`; this module handles counters, send schedules, and routing.
//!
//! The phone automatically switches audio to P2P during a call; if we do not
//! punch through NAT, our NAT drops packets and relayed audio fades out and stops.

use crate::media::Server;
use crate::zrtp::{
    build_p2p_bind, build_p2p_bind_answer, build_p2p_pong, wrap_p2p_audio, wrap_p2p_rtcp,
    wrap_p2p_video,
};
use std::collections::BTreeMap;

/// Fast NAT hole-punching interval during the first second, in ms.
pub const P2P_BIND_FAST_MS: u64 = 100;
/// Subsequent NAT hole-punching interval, in ms.
pub const P2P_BIND_SLOW_MS: u64 = 400;
/// Timeout before giving up on NAT hole punching, in ms.
pub const P2P_BIND_GIVE_UP_MS: u64 = 15_000;
/// P2P packet kinds.
pub mod kind {
    /// Outgoing NAT bind request.
    pub const BIND: u8 = 1;
    /// Incoming NAT bind request from the peer.
    pub const BIND_PEER: u8 = 2;
    /// NAT bind response.
    pub const BIND_ANSWER: u8 = 3;
    /// Keepalive.
    pub const KEEPALIVE: u8 = 4;
    /// Audio RTP.
    pub const AUDIO: u8 = 6;
    /// Audio RTCP.
    pub const AUDIO_RTCP: u8 = 7;
    /// Video RTP.
    pub const VIDEO: u8 = 8;
    /// Video RTCP.
    pub const VIDEO_RTCP: u8 = 9;
    /// P2P header length in bytes.
    pub const HEADER_LEN: usize = 9;
}

/// Action to take after processing a P2P packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum P2pAction {
    /// Send this packet to the specified peer address.
    Send(Vec<u8>, Server),
    /// Audio RTP received over P2P.
    Audio(Vec<u8>),
    /// Video RTP received over P2P.
    Video(Vec<u8>),
    /// RTCP received over P2P (`video` = true for the video channel).
    Rtcp {
        /// Whether this packet belongs to the video channel.
        video: bool,
        /// Raw RTCP packet bytes.
        data: Vec<u8>,
    },
    /// P2P path is now established.
    PathUp(Server),
}

/// Per-call P2P state.
pub struct P2p {
    callee: bool,
    call_id: u32,
    candidates: Vec<Server>,
    seq: u16,
    path: Option<Server>,
    started_at: u64,
    next_bind_at: u64,
    active: bool,
    first_seen: bool,
    /// Received packet count by kind, for diagnostics.
    pub rx_kinds: BTreeMap<u8, u64>,
    /// Number of audio packets received over P2P.
    pub rx_audio: u64,
}

impl P2p {
    /// Creates the state machine with the peer's P2P `candidates`.
    pub fn new(callee: bool, call_id: u32, candidates: Vec<Server>, now: u64) -> Self {
        Self {
            callee,
            call_id,
            candidates,
            seq: 0,
            path: None,
            started_at: now,
            next_bind_at: now,
            active: true,
            first_seen: false,
            rx_kinds: BTreeMap::new(),
            rx_audio: 0,
        }
    }

    /// Returns whether there are any candidates available for NAT hole punching.
    pub fn has_candidates(&self) -> bool {
        !self.candidates.is_empty()
    }

    /// Returns the established P2P peer path, if any.
    pub fn path(&self) -> Option<&Server> {
        self.path.as_ref()
    }

    /// Stops NAT hole punching.
    pub fn stop(&mut self) {
        self.active = false;
    }

    /// Returns packets to send at `now` according to the NAT bind schedule.
    pub fn poll(&mut self, now: u64) -> Vec<(Vec<u8>, Server)> {
        if !self.active || self.path.is_some() || self.candidates.is_empty() {
            return Vec::new();
        }
        if now.saturating_sub(self.started_at) > P2P_BIND_GIVE_UP_MS {
            self.active = false;
            return Vec::new();
        }
        if now < self.next_bind_at {
            return Vec::new();
        }
        let fast = now.saturating_sub(self.started_at) < 1000;
        self.next_bind_at = now
            + if fast {
                P2P_BIND_FAST_MS
            } else {
                P2P_BIND_SLOW_MS
            };
        let packet = build_p2p_bind(self.callee, self.call_id, self.seq, 0);
        self.seq = self.seq.wrapping_add(1);
        self.candidates
            .iter()
            .map(|c| (packet.clone(), c.clone()))
            .collect()
    }

    /// Processes an incoming P2P packet.
    pub fn on_packet(&mut self, msg: &[u8], from: &Server, now: u64) -> Vec<P2pAction> {
        let _ = now;
        if msg.len() < kind::HEADER_LEN {
            return Vec::new();
        }
        let packet_kind = msg[7];
        *self.rx_kinds.entry(packet_kind).or_insert(0) += 1;
        if !self.first_seen {
            self.first_seen = true;
        }
        let mut out = Vec::new();
        match packet_kind {
            kind::BIND_PEER if msg.len() >= 19 => {
                let peer = format!("{}|{}", from.host, from.port);
                let answer = build_p2p_bind_answer(self.callee, self.call_id, msg, peer.as_bytes());
                out.push(P2pAction::Send(answer, from.clone()));
            }
            kind::KEEPALIVE if msg.len() > 9 && msg[9] == 1 => {
                let pong = build_p2p_pong(self.callee, msg);
                out.push(P2pAction::Send(pong, from.clone()));
                out.extend(self.mark_path(from));
            }
            kind::VIDEO if msg.len() > kind::HEADER_LEN + 12 => {
                out.extend(self.mark_path(from));
                out.push(P2pAction::Video(msg[kind::HEADER_LEN..].to_vec()));
            }
            kind::VIDEO_RTCP if msg.len() > kind::HEADER_LEN => {
                out.push(P2pAction::Rtcp {
                    video: true,
                    data: msg[kind::HEADER_LEN..].to_vec(),
                });
            }
            kind::AUDIO_RTCP if msg.len() > kind::HEADER_LEN => {
                out.push(P2pAction::Rtcp {
                    video: false,
                    data: msg[kind::HEADER_LEN..].to_vec(),
                });
            }
            kind::AUDIO if msg.len() > kind::HEADER_LEN + 12 => {
                self.rx_audio += 1;
                out.extend(self.mark_path(from));
                out.push(P2pAction::Audio(msg[kind::HEADER_LEN..].to_vec()));
            }
            _ => {}
        }
        out
    }

    fn mark_path(&mut self, from: &Server) -> Vec<P2pAction> {
        if self
            .path
            .as_ref()
            .is_some_and(|p| p.host == from.host && p.port == from.port)
        {
            return Vec::new();
        }
        self.path = Some(from.clone());
        vec![P2pAction::PathUp(from.clone())]
    }

    /// Wraps an audio RTP packet for sending over P2P.
    pub fn wrap_audio(&self, rtp: &[u8]) -> Option<Vec<u8>> {
        self.path
            .as_ref()
            .map(|_| wrap_p2p_audio(self.callee, self.call_id, rtp))
    }

    /// Wraps a video RTP packet for sending over P2P.
    pub fn wrap_video(&self, rtp: &[u8]) -> Option<Vec<u8>> {
        self.path
            .as_ref()
            .map(|_| wrap_p2p_video(self.callee, self.call_id, rtp))
    }

    /// Wraps an RTCP packet for sending over P2P (`video` = true for kind 9, false for kind 7).
    pub fn wrap_rtcp(&self, video: bool, rtcp: &[u8]) -> Option<Vec<u8>> {
        self.path
            .as_ref()
            .map(|_| wrap_p2p_rtcp(self.callee, self.call_id, video, rtcp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(port: u16) -> Server {
        Server {
            host: "192.0.2.5".to_owned(),
            port,
        }
    }

    #[test]
    fn binds_on_a_schedule_then_gives_up() {
        let mut p2p = P2p::new(true, 42, vec![server(5000)], 0);
        assert_eq!(p2p.poll(0).len(), 1);
        assert!(p2p.poll(50).is_empty()); // before 100 ms
        assert_eq!(p2p.poll(100).len(), 1);
        let pkt = p2p.poll(1_000);
        assert_eq!(pkt.len(), 1);
        assert_eq!(pkt[0].0[7], kind::BIND);
        // After 15 s: give up.
        assert!(p2p.poll(P2P_BIND_GIVE_UP_MS + 1).is_empty());
        assert!(p2p.poll(P2P_BIND_GIVE_UP_MS + 500).is_empty());
    }

    #[test]
    fn answers_a_peer_bind_and_marks_the_path() {
        let mut p2p = P2p::new(true, 42, vec![server(5000)], 0);
        // kind BIND_PEER (2), length >= 19.
        let mut msg = vec![0x7f, 0, 0, 0, 0, 0, 0, kind::BIND_PEER];
        msg.extend_from_slice(&[0u8; 11]);
        let actions = p2p.on_packet(&msg, &server(5000), 10);
        assert!(matches!(actions[0], P2pAction::Send(_, _)));
        // No P2P path established from this bind request alone.
        assert!(p2p.path().is_none());
    }

    #[test]
    fn keepalive_pong_raises_the_path() {
        let mut p2p = P2p::new(false, 7, vec![server(6000)], 0);
        let mut msg = vec![0x7f, 0, 0, 0, 0, 0, 0, kind::KEEPALIVE, 0, 1];
        msg.extend_from_slice(&[0u8; 8]);
        let actions = p2p.on_packet(&msg, &server(6000), 20);
        assert!(matches!(actions[0], P2pAction::Send(_, _)));
        assert!(actions.iter().any(|a| matches!(a, P2pAction::PathUp(_))));
        assert_eq!(p2p.path().map(|s| s.port), Some(6000));
        assert!(p2p.wrap_audio(&[1, 2, 3]).is_some());
    }

    #[test]
    fn routes_p2p_audio_and_video() {
        let mut p2p = P2p::new(false, 7, vec![server(6000)], 0);
        let mut audio = vec![0x7f, 0, 0, 0, 0, 0, 0, kind::AUDIO];
        audio.extend_from_slice(&[0u8; 9]);
        audio.extend_from_slice(&[0x80, 0x70, 0, 1]);
        audio.extend_from_slice(&[0u8; 12]);
        let actions = p2p.on_packet(&audio, &server(6000), 30);
        assert!(actions.iter().any(|a| matches!(a, P2pAction::Audio(_))));
        assert_eq!(p2p.rx_audio, 1);

        let mut video = vec![0x7f, 0, 0, 0, 0, 0, 0, kind::VIDEO];
        video.extend_from_slice(&[0u8; 9]);
        video.extend_from_slice(&[0x80, 0x62, 0, 1]);
        video.extend_from_slice(&[0u8; 12]);
        let actions = p2p.on_packet(&video, &server(6000), 40);
        assert!(actions.iter().any(|a| matches!(a, P2pAction::Video(_))));

        let mut rtcp = vec![0x7f, 0, 0, 0, 0, 0, 0, kind::VIDEO_RTCP];
        rtcp.extend_from_slice(&[0u8; 4]);
        let actions = p2p.on_packet(&rtcp, &server(6000), 50);
        assert!(matches!(actions[0], P2pAction::Rtcp { video: true, .. }));
    }

    #[test]
    fn no_candidates_means_no_binding() {
        let mut p2p = P2p::new(true, 1, Vec::new(), 0);
        assert!(!p2p.has_candidates());
        assert!(p2p.poll(0).is_empty());
    }
}
