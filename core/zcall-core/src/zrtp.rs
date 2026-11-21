//! ZRTP packet wire format for Zalo's zrtc stack.
//!
//! Ported from the reference call implementation (`zrtp-packet.js`),
//! which was checked byte-for-byte against the real engine's captures.

/// ZRTP message types on the wire.
pub struct MsgType;

impl MsgType {
    /// Control request (client -> server).
    pub const CONTROL: u8 = 0x01;
    /// Server -> client reply.
    pub const SERVER_REPLY: u8 = 0x02;
    /// Client -> server audio RTP.
    pub const AUDIO_RTP: u8 = 0x03;
    /// Server -> client audio RTP (1-byte header, no token).
    pub const AUDIO_RTP_DOWN: u8 = 0x04;
    /// Audio RTCP (legacy name).
    pub const AUDIO_FEC: u8 = 0x05;
    /// Audio RTCP.
    pub const AUDIO_RTCP: u8 = 0x07;
    /// Client -> server video RTP.
    pub const VIDEO_RTP: u8 = 0x0d;
    /// Server -> client video RTP (1-byte header, no token).
    pub const VIDEO_DOWN: u8 = 0x0e;
    /// Video RTCP.
    pub const VIDEO_FEC: u8 = 0x0f;
    /// Peer-to-peer extension.
    pub const P2P_EXT: u8 = 0x7f;
}

/// Control commands carried in the 21-byte header.
pub struct Command;

impl Command {
    /// Keepalive ping.
    pub const PING: u16 = 2;
    /// Hang up on the chosen server.
    pub const END_CALL: u16 = 3;
    /// Server RTT probe.
    pub const ECHO: u16 = 5;
    /// InitZRTP sent by the caller.
    pub const INIT_CALL_CALLER: u16 = 11;
    /// InitZRTP sent by the callee.
    pub const INIT_CALL_CALLEE: u16 = 12;
    /// Address change.
    pub const CHANGE_ADDRESS: u16 = 14;
    /// Relay request forwarded to the peer.
    pub const REQ_FORWARD: u16 = 32;
    /// Group message (server -> client).
    pub const GROUP_MESSAGE: u16 = 33;
    /// Room membership update.
    pub const ROOM_UPDATED: u16 = 50;
    /// Zavi ping (group).
    pub const INIT_ZAVI_PING: u16 = 0x33;
}

/// Magic byte in a control header.
pub const MAGIC_BYTE: u8 = 0x7e;

/// Length of a control header in bytes.
pub const CONTROL_HEADER_LEN: usize = 21;

/// Length of a media header (type + token) in bytes.
pub const MEDIA_HEADER_LEN: usize = 5;

/// A parsed ZRTP packet. Optional fields depend on the packet kind.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParsedPacket {
    /// Whether this is a media packet.
    pub is_media: bool,
    /// The message type byte.
    pub msg_type: u8,
    /// Control header magic byte.
    pub magic: Option<u8>,
    /// Control header field at offset 2.
    pub field02: Option<u32>,
    /// Control header sequence number.
    pub seq: Option<u32>,
    /// Control header local UID.
    pub uid: Option<u32>,
    /// Control header server token.
    pub token: Option<u32>,
    /// Control command.
    pub cmd: Option<u16>,
    /// Control sub-command.
    pub sub_cmd: Option<u8>,
    /// Bytes after the header.
    pub data: Vec<u8>,
    /// Media payload (after the 5-byte media header, or the 1-byte downlink header).
    pub payload: Vec<u8>,
    /// Whether a media packet was truncated.
    pub truncated: bool,
    /// The raw packet when it does not match a known shape.
    pub raw: Vec<u8>,
    /// Server reply result code.
    pub res: Option<u32>,
    /// Call id parsed from an init/end packet or server reply.
    pub call_id: Option<u32>,
    /// Server token handed back in a server reply.
    pub server_token: Option<u32>,
    /// Our public address in a server reply ("ip|port").
    pub public_addr: Option<String>,
    /// Peer UID parsed from an init packet.
    pub peer_uid: Option<u32>,
    /// Session id parsed from an init or ping packet.
    pub sess_id: Option<Vec<u8>>,
    /// Extra string parsed from an init packet.
    pub extra: Option<Vec<u8>>,
    /// P2P role byte.
    pub b2: Option<u8>,
    /// P2P field at offset 3.
    pub field38: Option<u32>,
}

/// An entry in a group video quality request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VidQualEntry {
    /// Member UID.
    pub uid: u32,
    /// Quality id.
    pub quality_id: u8,
}

/// A layer announced in a group video quality config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VidQualLayer {
    /// Layer id.
    pub id: u8,
    /// Bitrate on the wire (kbps).
    pub a: u32,
    /// Width.
    pub b: u16,
    /// Height.
    pub c: u16,
}

fn is_media_type(t: u8) -> bool {
    matches!(t, 0x03 | 0x04 | 0x05 | 0x07 | 0x0d | 0x0e | 0x0f)
}

/// Wraps a media payload with a 5-byte header (type + token LE).
pub fn wrap_media_packet(msg_type: u8, token: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MEDIA_HEADER_LEN + payload.len());
    out.push(msg_type);
    out.extend_from_slice(&token.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Builds a 21-byte control header.
#[allow(clippy::too_many_arguments)]
pub fn build_control_header(
    msg_type: u8,
    magic: u8,
    field02: u32,
    seq: u32,
    uid: u32,
    token: u32,
    cmd: u16,
    sub_cmd: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(CONTROL_HEADER_LEN);
    out.push(msg_type);
    out.push(magic);
    out.extend_from_slice(&field02.to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&uid.to_le_bytes());
    out.extend_from_slice(&token.to_le_bytes());
    out.extend_from_slice(&cmd.to_le_bytes());
    out.push(sub_cmd);
    out
}

fn u16_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Builds an InitZRTP packet (cmd 11 caller / 12 callee).
#[allow(clippy::too_many_arguments)]
pub fn build_init_zrtp_packet(
    caller: bool,
    uid: u32,
    peer_uid: u32,
    call_id: u32,
    sess_id: &[u8],
    extra: &[u8],
    sub_cmd: u8,
) -> Vec<u8> {
    let cmd = if caller {
        Command::INIT_CALL_CALLER
    } else {
        Command::INIT_CALL_CALLEE
    };
    let mut out = build_control_header(MsgType::CONTROL, MAGIC_BYTE, 0, 0, uid, 0, cmd, sub_cmd);
    out.extend_from_slice(&call_id.to_le_bytes());
    out.extend_from_slice(&peer_uid.to_le_bytes());
    out.extend_from_slice(&u16_string(sess_id));
    if !extra.is_empty() {
        out.extend_from_slice(&u16_string(extra));
    }
    out
}

/// Which group InitZRTP body to try. The SFU accepts one of these; a refusal
/// moves on to the next, in the same order as `zrtc-media.js`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GroupInitLayout {
    /// u32 0, u32 0, session, extra. subCmd 4.
    Zero,
    /// subCmd 6: call id, then host / 5 / 0.
    Sub6,
    /// u32 callId, u32 0, session, extra. subCmd 4.
    CallId,
    /// u32 callId, u32 hostCall, session, extra. subCmd 4.
    CallIdHost,
}

impl GroupInitLayout {
    /// The order Electron tries when none has been saved.
    pub const ALL: [Self; 4] = [Self::Zero, Self::Sub6, Self::CallId, Self::CallIdHost];

    /// Short name for the call log.
    pub fn name(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::Sub6 => "sub6",
            Self::CallId => "callId",
            Self::CallIdHost => "callIdHost",
        }
    }
}

/// Four bytes on InitZRTP, three on ZaviPing: `[0, 5, camera, 1?]`.
///
/// Byte 2 is 1 while this client is sending video. A 0 there made the SFU
/// stop forwarding the camera at the next ping.
fn group_extra(video_on: bool, init_zrtp: bool) -> Vec<u8> {
    let mut extra = vec![0, 5, u8::from(video_on)];
    if init_zrtp {
        extra.push(1);
    }
    extra
}

/// Builds a group InitZRTP packet (cmd 11 host / 12 others, subCmd 4 or 6).
pub fn build_group_init_zrtp_packet(
    host: bool,
    uid: u32,
    sess_id: &[u8],
    video_on: bool,
    call_id: u32,
    host_call: u32,
    layout: GroupInitLayout,
) -> Vec<u8> {
    let cmd = if host {
        Command::INIT_CALL_CALLER
    } else {
        Command::INIT_CALL_CALLEE
    };
    let sub = if layout == GroupInitLayout::Sub6 {
        0x06
    } else {
        0x04
    };
    let mut out = build_control_header(MsgType::CONTROL, MAGIC_BYTE, 0, 0, uid, 0, cmd, sub);
    let extra = u16_string(&group_extra(video_on, true));
    if layout == GroupInitLayout::Sub6 {
        out.extend_from_slice(&call_id.to_le_bytes());
        out.push(u8::from(host));
        out.push(5);
        out.push(0);
        out.extend_from_slice(&u16_string(sess_id));
        out.extend_from_slice(&extra);
        return out;
    }
    let first = if layout == GroupInitLayout::Zero {
        0
    } else {
        call_id
    };
    let second = if layout == GroupInitLayout::CallIdHost {
        host_call
    } else {
        0
    };
    out.extend_from_slice(&first.to_le_bytes());
    out.extend_from_slice(&second.to_le_bytes());
    out.extend_from_slice(&u16_string(sess_id));
    out.extend_from_slice(&extra);
    out
}

/// Builds a group ZaviPing (cmd 0x33, sub 1). `video_on` is the camera bit.
pub fn build_zavi_ping_packet(uid: u32, token: u32, sess_id: &[u8], video_on: bool) -> Vec<u8> {
    let mut out = build_control_header(
        MsgType::CONTROL,
        MAGIC_BYTE,
        0,
        0,
        uid,
        token,
        Command::INIT_ZAVI_PING,
        0x01,
    );
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&u16_string(sess_id));
    out.extend_from_slice(&u16_string(&group_extra(video_on, false)));
    out
}

/// Builds a group screen-share peer InitZRTP packet (`ShareScreenPeer` on macOS/Windows).
/// Uses `uid = shareScreenId`, `cmd = INIT_CALL_CALLEE`, `subCmd = 0x04`, and extra `[0, 0xff, 1, 0]`.
pub fn build_share_init_zrtp_packet(uid: u32, sess_id: &[u8]) -> Vec<u8> {
    let mut out = build_control_header(
        MsgType::CONTROL,
        MAGIC_BYTE,
        0,
        0,
        uid,
        0,
        Command::INIT_CALL_CALLEE,
        0x04,
    );
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&u16_string(sess_id));
    out.extend_from_slice(&u16_string(&[0, 0xff, 1, 0]));
    out
}

/// Builds a group screen-share peer ZaviPing packet (`cmd = 0x33`, `subCmd = 0x01`, no camera extra).
pub fn build_share_zavi_ping_packet(uid: u32, token: u32, sess_id: &[u8]) -> Vec<u8> {
    let mut out = build_control_header(
        MsgType::CONTROL,
        MAGIC_BYTE,
        0,
        0,
        uid,
        token,
        Command::INIT_ZAVI_PING,
        0x01,
    );
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&u16_string(sess_id));
    out
}

/// Builds a ping packet (cmd 2, magic overwritten with 1).
pub fn build_ping_packet(uid: u32, token: u32, sess_id: &[u8]) -> Vec<u8> {
    let mut out = build_control_header(
        MsgType::CONTROL,
        0x01,
        0,
        0,
        uid,
        token,
        Command::PING,
        0x01,
    );
    out.extend_from_slice(&u16_string(sess_id));
    out
}

/// Builds an end-call packet (cmd 3).
pub fn build_end_call_packet(uid: u32, token: u32, call_id: u32) -> Vec<u8> {
    let mut out = build_control_header(
        MsgType::CONTROL,
        MAGIC_BYTE,
        0,
        0,
        uid,
        token,
        Command::END_CALL,
        0,
    );
    out.extend_from_slice(&call_id.to_le_bytes());
    out
}

/// Builds a ReqForward packet (cmd 32, magic 1).
pub fn build_req_forward_packet(
    uid: u32,
    token: u32,
    seq: u32,
    sub_cmd: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut out = build_control_header(
        MsgType::CONTROL,
        0x01,
        0,
        seq,
        uid,
        token,
        Command::REQ_FORWARD,
        sub_cmd,
    );
    out.extend_from_slice(payload);
    out
}

/// Builds the 24-byte ReqForward status body.
pub fn req_forward_status(rx_per_second: u32, a: u32, b5: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    for value in [20_u32, a, 1, 1, rx_per_second, b5] {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn vid_qual_body(entries_len: usize, stride: usize, write: impl Fn(&mut [u8])) -> Vec<u8> {
    let mut body = vec![0_u8; 1 + entries_len * stride];
    body[0] = entries_len as u8;
    write(&mut body[1..]);
    body
}

/// Builds a group video quality request (cmd 32, sub 13).
pub fn build_group_vid_qual_req(
    uid: u32,
    token: u32,
    seq: u32,
    entries: &[VidQualEntry],
) -> Vec<u8> {
    let body = vid_qual_body(entries.len(), 5, |b| {
        for (i, e) in entries.iter().enumerate() {
            let o = i * 5;
            b[o..o + 4].copy_from_slice(&e.uid.to_le_bytes());
            b[o + 4] = e.quality_id;
        }
    });
    let mut out = build_control_header(
        MsgType::CONTROL,
        0x01,
        0,
        seq,
        uid,
        token,
        Command::REQ_FORWARD,
        0x0d,
    );
    out.extend_from_slice(&u16_string(&body));
    out
}

/// Builds a group video quality config (cmd 32, sub 12).
pub fn build_group_vid_qual_config(
    uid: u32,
    token: u32,
    seq: u32,
    layers: &[VidQualLayer],
) -> Vec<u8> {
    let body = vid_qual_body(layers.len(), 9, |b| {
        for (i, l) in layers.iter().enumerate() {
            let o = i * 9;
            b[o] = l.id;
            b[o + 1..o + 5].copy_from_slice(&l.a.to_le_bytes());
            b[o + 5..o + 7].copy_from_slice(&l.b.to_le_bytes());
            b[o + 7..o + 9].copy_from_slice(&l.c.to_le_bytes());
        }
    });
    let mut out = build_control_header(
        MsgType::CONTROL,
        0x01,
        0,
        seq,
        uid,
        token,
        Command::REQ_FORWARD,
        0x0c,
    );
    out.extend_from_slice(&u16_string(&body));
    out
}

/// Builds a group sync packet (cmd 33, no body).
pub fn build_group_sync_packet(uid: u32, token: u32, seq: u32, sub_cmd: u8) -> Vec<u8> {
    build_control_header(
        MsgType::CONTROL,
        0x01,
        0,
        seq,
        uid,
        token,
        Command::GROUP_MESSAGE,
        sub_cmd,
    )
}

/// Builds a 9-byte P2P header.
pub fn p2p_header(callee: bool, call_id: u32, kind: u8) -> Vec<u8> {
    let mut out = vec![0_u8; 9];
    out[0] = MsgType::P2P_EXT;
    out[2] = u8::from(callee);
    out[3..7].copy_from_slice(&call_id.to_le_bytes());
    out[7] = kind;
    out
}

/// Builds a P2P bind packet (kind 1).
pub fn build_p2p_bind(callee: bool, call_id: u32, seq: u16, index: u32) -> Vec<u8> {
    let mut out = p2p_header(callee, call_id, 1);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&index.to_be_bytes());
    out
}

/// Builds a P2P bind answer (kind 3) from a peer request.
pub fn build_p2p_bind_answer(
    callee: bool,
    call_id: u32,
    request: &[u8],
    peer_addr: &[u8],
) -> Vec<u8> {
    let mut out = p2p_header(callee, call_id, 3);
    if request.len() >= 15 {
        out.extend_from_slice(&request[9..15]);
    }
    out.extend_from_slice(&(peer_addr.len() as u32).to_le_bytes());
    out.extend_from_slice(peer_addr);
    out
}

/// Builds a P2P pong from a ping.
pub fn build_p2p_pong(callee: bool, ping: &[u8]) -> Vec<u8> {
    let mut pong = ping.to_vec();
    if pong.len() > 9 {
        pong[2] = u8::from(callee);
        pong[9] = 2;
    }
    pong
}

/// Wraps audio RTP for P2P (kind 6).
pub fn wrap_p2p_audio(callee: bool, call_id: u32, rtp: &[u8]) -> Vec<u8> {
    let mut out = p2p_header(callee, call_id, 6);
    out.extend_from_slice(rtp);
    out
}

/// Wraps video RTP for P2P (kind 8).
pub fn wrap_p2p_video(callee: bool, call_id: u32, rtp: &[u8]) -> Vec<u8> {
    let mut out = p2p_header(callee, call_id, 8);
    out.extend_from_slice(rtp);
    out
}

/// Wraps RTCP for P2P (kind 7 audio, 9 video).
pub fn wrap_p2p_rtcp(callee: bool, call_id: u32, video: bool, rtcp: &[u8]) -> Vec<u8> {
    let kind = if video { 9 } else { 7 };
    let mut out = p2p_header(callee, call_id, kind);
    out.extend_from_slice(rtcp);
    out
}

/// Parses a ZRTP packet.
pub fn unwrap_packet(buf: &[u8]) -> Option<ParsedPacket> {
    let msg_type = *buf.first()?;

    if msg_type == MsgType::AUDIO_RTP_DOWN || msg_type == MsgType::VIDEO_DOWN {
        return Some(ParsedPacket {
            is_media: true,
            msg_type,
            payload: buf[1..].to_vec(),
            ..ParsedPacket::default()
        });
    }
    if is_media_type(msg_type) {
        if buf.len() < MEDIA_HEADER_LEN {
            return Some(ParsedPacket {
                is_media: true,
                msg_type,
                truncated: true,
                raw: buf.to_vec(),
                ..ParsedPacket::default()
            });
        }
        return Some(ParsedPacket {
            is_media: true,
            msg_type,
            token: Some(read_u32_le(buf, 1)),
            payload: buf[5..].to_vec(),
            ..ParsedPacket::default()
        });
    }

    if (msg_type == MsgType::CONTROL || msg_type == MsgType::SERVER_REPLY)
        && buf.len() >= CONTROL_HEADER_LEN
    {
        let data = buf[CONTROL_HEADER_LEN..].to_vec();
        let mut parsed = ParsedPacket {
            is_media: false,
            msg_type,
            magic: Some(buf[1]),
            field02: Some(read_u32_le(buf, 2)),
            seq: Some(read_u32_le(buf, 6)),
            uid: Some(read_u32_le(buf, 10)),
            token: Some(read_u32_le(buf, 14)),
            cmd: Some(read_u16_le(buf, 18)),
            sub_cmd: Some(buf[20]),
            data,
            ..ParsedPacket::default()
        };
        let is_init = parsed.cmd == Some(Command::INIT_CALL_CALLER)
            || parsed.cmd == Some(Command::INIT_CALL_CALLEE);
        if is_init && msg_type == MsgType::SERVER_REPLY && parsed.data.len() >= 4 {
            parsed.res = Some(read_u32_le(&parsed.data, 0));
            if parsed.res == Some(0) && parsed.data.len() >= 12 {
                parsed.call_id = Some(read_u32_le(&parsed.data, 4));
                parsed.server_token = Some(read_u32_le(&parsed.data, 8));
                if parsed.data.len() >= 14 {
                    let n = read_u16_le(&parsed.data, 12) as usize;
                    let end = 14 + n.min(parsed.data.len() - 14);
                    parsed.public_addr =
                        Some(String::from_utf8_lossy(&parsed.data[14..end]).into_owned());
                }
            }
        } else if is_init && parsed.data.len() >= 10 {
            parsed.call_id = Some(read_u32_le(&parsed.data, 0));
            parsed.peer_uid = Some(read_u32_le(&parsed.data, 4));
            let n1 = read_u16_le(&parsed.data, 8) as usize;
            let end = 10 + n1.min(parsed.data.len() - 10);
            parsed.sess_id = Some(parsed.data[10..end].to_vec());
            let off = 10 + n1;
            if parsed.data.len() >= off + 2 {
                let extra_len = read_u16_le(&parsed.data, off) as usize;
                let extra_end = off + 2 + extra_len.min(parsed.data.len() - off - 2);
                parsed.extra = Some(parsed.data[off + 2..extra_end].to_vec());
            }
        } else if parsed.cmd == Some(Command::PING) && parsed.data.len() >= 2 {
            let n = read_u16_le(&parsed.data, 0) as usize;
            let end = 2 + n.min(parsed.data.len() - 2);
            parsed.sess_id = Some(parsed.data[2..end].to_vec());
        } else if parsed.cmd == Some(Command::END_CALL) && parsed.data.len() >= 4 {
            parsed.call_id = Some(read_u32_le(&parsed.data, 0));
        }
        return Some(parsed);
    }

    if msg_type == MsgType::P2P_EXT && buf.len() >= 9 {
        return Some(ParsedPacket {
            is_media: false,
            msg_type,
            b2: Some(buf[2]),
            field38: Some(read_u32_le(buf, 3)),
            cmd: Some(read_u16_le(buf, 7)),
            data: buf[9..].to_vec(),
            ..ParsedPacket::default()
        });
    }

    Some(ParsedPacket {
        is_media: false,
        msg_type,
        raw: buf.to_vec(),
        ..ParsedPacket::default()
    })
}

fn read_u16_le(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn read_u32_le(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps_media() {
        let payload = vec![0x80, 0xe0, 0x00, 0x01, 0xde, 0xad, 0xbe, 0xef];
        let wrapped = wrap_media_packet(MsgType::AUDIO_RTP, 1830974409, &payload);
        assert_eq!(wrapped.len(), 5 + payload.len());
        assert_eq!(wrapped[0], 0x03);
        assert_eq!(read_u32_le(&wrapped, 1), 1830974409);
        let parsed = unwrap_packet(&wrapped).expect("parsed");
        assert!(parsed.is_media);
        assert_eq!(parsed.msg_type, MsgType::AUDIO_RTP);
        assert_eq!(parsed.token, Some(1830974409));
        assert_eq!(parsed.payload, payload);

        // Downlink media has a 1-byte header: unwrap treats everything after
        // byte 0 as payload (the token written by wrap_media_packet included).
        let down = wrap_media_packet(MsgType::AUDIO_RTP_DOWN, 0, &payload);
        let parsed = unwrap_packet(&down).expect("parsed");
        assert_eq!(parsed.msg_type, MsgType::AUDIO_RTP_DOWN);
        assert_eq!(parsed.payload, [vec![0_u8; 4], payload.clone()].concat());
    }

    #[test]
    fn builds_and_unwraps_init_caller() {
        let sess_id = b"sample-session-id-for-testing";
        let packet = build_init_zrtp_packet(true, 67890, 22, 12345, sess_id, b"", 0x02);
        assert_eq!(packet[0], MsgType::CONTROL);
        assert_eq!(packet[1], MAGIC_BYTE);
        assert_eq!(packet.len(), CONTROL_HEADER_LEN + 8 + 2 + sess_id.len());
        assert_eq!(read_u32_le(&packet, 10), 67890);
        assert_eq!(read_u32_le(&packet, 14), 0);
        assert_eq!(read_u16_le(&packet, 18), Command::INIT_CALL_CALLER);
        assert_eq!(packet[20], 0x02);
        assert_eq!(read_u32_le(&packet, 21), 12345);
        assert_eq!(read_u32_le(&packet, 25), 22);
        assert_eq!(read_u16_le(&packet, 29), sess_id.len() as u16);

        let parsed = unwrap_packet(&packet).expect("parsed");
        assert_eq!(parsed.cmd, Some(Command::INIT_CALL_CALLER));
        assert_eq!(parsed.uid, Some(67890));
        assert_eq!(parsed.call_id, Some(12345));
        assert_eq!(parsed.peer_uid, Some(22));
        assert_eq!(parsed.sess_id.as_deref(), Some(sess_id.as_slice()));
    }

    #[test]
    fn builds_and_unwraps_init_callee_with_extra() {
        let packet = build_init_zrtp_packet(false, 1, 0, 100, b"callee-sess", b"x", 0x02);
        let parsed = unwrap_packet(&packet).expect("parsed");
        assert_eq!(parsed.cmd, Some(Command::INIT_CALL_CALLEE));
        assert_eq!(parsed.sess_id.as_deref(), Some(b"callee-sess".as_slice()));
        assert_eq!(parsed.extra.as_deref(), Some(b"x".as_slice()));

        let sess = vec![b's'; 152];
        let plain = build_init_zrtp_packet(false, 1, 0, 100, &sess, b"", 0x02);
        assert_eq!(plain.len(), 183);
    }

    #[test]
    fn unwraps_server_reply() {
        let addr = b"192.0.2.1|51735";
        let mut reply = build_control_header(
            MsgType::SERVER_REPLY,
            MAGIC_BYTE,
            0,
            0,
            67890,
            0,
            Command::INIT_CALL_CALLER,
            2,
        );
        reply.extend_from_slice(&0_u32.to_le_bytes());
        reply.extend_from_slice(&12345_u32.to_le_bytes());
        reply.extend_from_slice(&0x12345678_u32.to_le_bytes());
        reply.extend_from_slice(&(addr.len() as u16).to_le_bytes());
        reply.extend_from_slice(addr);

        let parsed = unwrap_packet(&reply).expect("parsed");
        assert_eq!(parsed.msg_type, MsgType::SERVER_REPLY);
        assert_eq!(parsed.res, Some(0));
        assert_eq!(parsed.call_id, Some(12345));
        assert_eq!(parsed.server_token, Some(0x12345678));
        assert_eq!(parsed.public_addr.as_deref(), Some("192.0.2.1|51735"));
    }

    #[test]
    fn builds_and_unwraps_ping_and_end_call() {
        let ping = build_ping_packet(5678, 1234, b"abc");
        assert_eq!(ping[0], MsgType::CONTROL);
        assert_eq!(ping[1], 1);
        let parsed = unwrap_packet(&ping).expect("parsed");
        assert_eq!(parsed.cmd, Some(Command::PING));
        assert_eq!(parsed.uid, Some(5678));
        assert_eq!(parsed.token, Some(1234));
        assert_eq!(parsed.sess_id.as_deref(), Some(b"abc".as_slice()));

        let end = build_end_call_packet(9, 8, 7);
        assert_eq!(end.len(), 25);
        let parsed = unwrap_packet(&end).expect("parsed");
        assert_eq!(parsed.cmd, Some(Command::END_CALL));
        assert_eq!(parsed.call_id, Some(7));
    }

    #[test]
    fn group_init_zero_is_subcmd_4_with_camera_extra() {
        let packet =
            build_group_init_zrtp_packet(true, 42, b"sess", true, 99, 7, GroupInitLayout::Zero);
        assert_eq!(packet[20], 0x04);
        assert_eq!(read_u16_le(&packet, 18), Command::INIT_CALL_CALLER);
        assert_eq!(read_u32_le(&packet, 21), 0);
        assert_eq!(read_u32_le(&packet, 25), 0);
        let parsed = unwrap_packet(&packet).expect("parsed");
        assert_eq!(parsed.extra.as_deref(), Some([0, 5, 1, 1].as_slice()));
    }

    #[test]
    fn zavi_ping_carries_the_camera_bit() {
        let on = build_zavi_ping_packet(42, 7, b"sess", true);
        let parsed = unwrap_packet(&on).expect("parsed");
        assert_eq!(parsed.cmd, Some(Command::INIT_ZAVI_PING));
        assert_eq!(parsed.magic, Some(MAGIC_BYTE));
        assert_eq!(parsed.token, Some(7));
        assert_eq!(on[1], MAGIC_BYTE);
        let off = build_zavi_ping_packet(42, 7, b"sess", false);
        assert_ne!(on, off);
        assert_eq!(*on.last().expect("extra"), 1);
        assert_eq!(*off.last().expect("extra"), 0);
    }
}
