//! RTP header of outgoing audio, ported from the reference call
//! implementation (`rtp.js`).
//!
//! Layout (capture 2026-09-29): `90 70 | seq | ts | ssrc | be de 00 01 | 51 <tw
//! seq hi> <tw seq lo> 00` — V=2, X=1, PT 112, one-byte extension id 5 len 2
//! (transport-cc), padded to 4 bytes.

/// Opus PT on the wire.
pub const AUDIO_PT: u8 = 112;

/// Samples per 20 ms frame at 16 kHz.
pub const FRAME_SAMPLES: u32 = 320;

const TWCC_EXT_ID: u8 = 5;
/// RFC 6464 audio level. Group audio only; 1-1 stays a 20-byte header.
const AUDIO_LEVEL_EXT_ID: u8 = 2;

/// Builds the 20-byte 1-1 audio RTP header.
pub fn build_audio_rtp_header(
    seq: u16,
    ts: u32,
    ssrc: u32,
    tw_seq: u16,
    marker: bool,
    pt: u8,
) -> Vec<u8> {
    let mut header = vec![0_u8; 20];
    header[0] = 0x90;
    header[1] = (if marker { 0x80 } else { 0 }) | (pt & 0x7f);
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&ts.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    header[12..14].copy_from_slice(&0xbede_u16.to_be_bytes());
    header[14..16].copy_from_slice(&1_u16.to_be_bytes());
    header[16] = (TWCC_EXT_ID << 4) | 1;
    header[17..19].copy_from_slice(&tw_seq.to_be_bytes());
    header[19] = 0;
    header
}

/// Builds the group audio RTP header: level, transport-cc, capture NTP.
///
/// 1-1 must keep [`build_audio_rtp_header`]. The level byte is what the SFU
/// uses to mark who is speaking.
pub fn build_group_audio_rtp_header(
    seq: u16,
    ts: u32,
    ssrc: u32,
    tw_seq: u16,
    marker: bool,
    pt: u8,
    level: u8,
    capture_ms: u64,
) -> Vec<u8> {
    let mut header = vec![0_u8; 32];
    header[0] = 0x90;
    header[1] = (if marker { 0x80 } else { 0 }) | (pt & 0x7f);
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&ts.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    header[12..14].copy_from_slice(&0xbede_u16.to_be_bytes());
    header[14..16].copy_from_slice(&4_u16.to_be_bytes());
    header[16] = (AUDIO_LEVEL_EXT_ID << 4) | 0;
    header[17] = level.min(127);
    header[18] = (TWCC_EXT_ID << 4) | 1;
    header[19..21].copy_from_slice(&tw_seq.to_be_bytes());
    write_capture_ntp(&mut header, 21, capture_ms);
    header
}

/// Fields read back from an audio RTP header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioRtpHeader {
    /// Marker bit.
    pub marker: bool,
    /// Payload type.
    pub pt: u8,
    /// Sequence number.
    pub seq: u16,
    /// Timestamp.
    pub ts: u32,
    /// Synchronization source.
    pub ssrc: u32,
    /// Transport-wide sequence, when the id 5 extension is present.
    pub tw_seq: Option<u16>,
}

/// Id of the abs-send-time extension.
const ABS_SEND_TIME_EXT_ID: u8 = 3;
/// Id of the capture-time NTP extension.
const CAPTURE_NTP_EXT_ID: u8 = 13;
/// Seconds between the NTP epoch and the Unix epoch.
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;
/// Short SFU extension id (CallType 2).
const SFU_SHORT_ID: u8 = 12;
/// Layer byte the macOS client sends for its lowest layer.
const SFU_BYTE0: u8 = 0x03;

/// One SFU layer description carried on group video.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SfuLayer {
    /// Extension id, 12 (short) or 14 (long).
    pub id: u8,
    /// Long form uses 7 data bytes; short uses 3.
    pub long: bool,
}

/// Builds the group-video RTP header (abs-send-time, transport-cc, SFU, capture NTP).
///
/// Ported from `buildGroupVideoRtpHeader` in `rtp.js`. `layer_seq` is the
/// per-layer packet counter (it must increase on every packet of a frame).
#[allow(clippy::too_many_arguments)]
pub fn build_group_video_rtp_header(
    seq: u16,
    ts: u32,
    ssrc: u32,
    tw_seq: u16,
    marker: bool,
    pt: u8,
    sfu: SfuLayer,
    layer_seq: u16,
    capture_ms: u64,
    send_ms: u64,
) -> Vec<u8> {
    let data_len: usize = if sfu.long { 7 } else { 3 };
    let ext_bytes = 4 + 3 + (1 + data_len) + 8;
    let padded = (ext_bytes + 3) & !3;
    let mut header = vec![0_u8; 12 + 4 + padded];
    header[0] = 0x90;
    header[1] = (if marker { 0x80 } else { 0 }) | (pt & 0x7f);
    header[2..4].copy_from_slice(&seq.to_be_bytes());
    header[4..8].copy_from_slice(&ts.to_be_bytes());
    header[8..12].copy_from_slice(&ssrc.to_be_bytes());
    header[12..14].copy_from_slice(&0xbede_u16.to_be_bytes());
    header[14..16].copy_from_slice(&((padded / 4) as u16).to_be_bytes());
    let mut o = 16;
    header[o] = (ABS_SEND_TIME_EXT_ID << 4) | 2;
    o += 1;
    let abs = ((send_ms as u128 * 262_144) / 1000) % 0x100_0000;
    header[o] = ((abs >> 16) & 0xff) as u8;
    header[o + 1] = ((abs >> 8) & 0xff) as u8;
    header[o + 2] = (abs & 0xff) as u8;
    o += 3;
    header[o] = (TWCC_EXT_ID << 4) | 1;
    o += 1;
    header[o..o + 2].copy_from_slice(&tw_seq.to_be_bytes());
    o += 2;
    let id = if sfu.id == 0 { SFU_SHORT_ID } else { sfu.id };
    header[o] = ((id & 0x0f) << 4) | if sfu.long { 6 } else { 2 };
    o += 1;
    header[o] = SFU_BYTE0;
    o += 1;
    header[o..o + 2].copy_from_slice(&layer_seq.to_be_bytes());
    o += 2;
    if sfu.long {
        o += 4;
    }
    write_capture_ntp(&mut header, o, capture_ms);
    header
}

/// Writes the id-13 capture NTP element at `offset`.
fn write_capture_ntp(header: &mut [u8], mut offset: usize, capture_ms: u64) -> usize {
    header[offset] = (CAPTURE_NTP_EXT_ID << 4) | 6;
    offset += 1;
    let sec = (capture_ms / 1000) + NTP_UNIX_OFFSET;
    let frac = ((capture_ms % 1000) as u128 * 0x1_0000_0000) / 1000;
    header[offset] = ((sec >> 16) & 0xff) as u8;
    header[offset + 1] = ((sec >> 8) & 0xff) as u8;
    header[offset + 2] = (sec & 0xff) as u8;
    offset += 3;
    header[offset..offset + 4].copy_from_slice(&(frac as u32).to_be_bytes());
    offset + 4
}

/// Reads the fields back (for checks against captured packets).
pub fn read_audio_rtp_header(rtp: &[u8]) -> Option<AudioRtpHeader> {
    if rtp.len() < 20 || rtp[0] != 0x90 || u16::from_be_bytes([rtp[12], rtp[13]]) != 0xbede {
        return None;
    }
    Some(AudioRtpHeader {
        marker: rtp[1] & 0x80 != 0,
        pt: rtp[1] & 0x7f,
        seq: u16::from_be_bytes([rtp[2], rtp[3]]),
        ts: u32::from_be_bytes([rtp[4], rtp[5], rtp[6], rtp[7]]),
        ssrc: u32::from_be_bytes([rtp[8], rtp[9], rtp[10], rtp[11]]),
        tw_seq: if (rtp[16] >> 4) == TWCC_EXT_ID {
            Some(u16::from_be_bytes([rtp[17], rtp[18]]))
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_video_header_carries_sfu_and_capture_time() {
        let header = build_group_video_rtp_header(
            7,
            90_000,
            100,
            3,
            true,
            98,
            SfuLayer {
                id: 12,
                long: false,
            },
            1,
            0,
            0,
        );
        assert_eq!(header.len(), 36);
        assert_eq!(header[0], 0x90);
        assert_eq!(header[1], 0x80 | 98);
        assert_eq!(u16::from_be_bytes([header[12], header[13]]), 0xbede);
        assert_eq!(header[16] >> 4, 3);
        assert_eq!(header[20] >> 4, 5);
        assert_eq!(header[23] >> 4, 12);
        assert_eq!(header[24], 0x03);
        assert_eq!(u16::from_be_bytes([header[25], header[26]]), 1);
        assert_eq!(header[27] >> 4, 13);
    }

    #[test]
    fn group_audio_header_carries_the_level() {
        let header = build_group_audio_rtp_header(1, 320, 9, 4, false, AUDIO_PT, 200, 0);
        assert_eq!(header.len(), 32);
        assert_eq!(header[16] >> 4, 2);
        assert_eq!(header[17], 127);
        assert_eq!(header[18] >> 4, 5);
        assert_eq!(u16::from_be_bytes([header[19], header[20]]), 4);
        assert_eq!(header[21] >> 4, 13);
    }

    #[test]
    fn audio_header_round_trips() {
        let header = build_audio_rtp_header(5683, 0x0010_0000, 0x0f03_6f9c, 1, false, AUDIO_PT);
        assert_eq!(header.len(), 20);
        assert_eq!(header[0], 0x90);
        assert_eq!(header[1], 112);
        assert_eq!(header[16], 0x51);

        let parsed = read_audio_rtp_header(&header).expect("parsed");
        assert_eq!(parsed.seq, 5683);
        assert_eq!(parsed.ts, 0x0010_0000);
        assert_eq!(parsed.ssrc, 0x0f03_6f9c);
        assert_eq!(parsed.tw_seq, Some(1));
        assert!(!parsed.marker);
        assert_eq!(parsed.pt, 112);
    }

    #[test]
    fn audio_header_marker_and_pt() {
        let header = build_audio_rtp_header(7, 0, 1, 2, true, 112);
        let parsed = read_audio_rtp_header(&header).expect("parsed");
        assert!(parsed.marker);
        assert_eq!(parsed.pt, 112);
        assert_eq!(read_audio_rtp_header(&[0_u8; 12]), None);
    }
}
