//! SRTP AES_CM_128_HMAC_SHA1_80 as used by Zalo zrtc (libsrtp2).
//!
//! The master key + salt are the first 30 bytes of the call sessId. Session
//! keys follow the RFC 3711 KDF. The wire packet is a 5-byte ZRTP header,
//! then an RTP header, encrypted Opus, and a 10-byte HMAC-SHA1 tag.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use hmac::{Hmac, Mac};
use sha1::Sha1;
use thiserror::Error;

type HmacSha1 = Hmac<Sha1>;

/// Session keys derived from a 30-byte master key+salt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionKeys {
    /// AES-CM encryption key (16 bytes).
    pub enc: Vec<u8>,
    /// HMAC-SHA1 key (20 bytes; only the first 10 are used on the wire).
    pub auth: Vec<u8>,
    /// Session salt for the packet IV (14 bytes).
    pub salt: Vec<u8>,
}

/// Why an SRTP operation failed.
#[derive(Debug, Error)]
pub enum SrtpError {
    /// The master key+salt must be at least 30 bytes.
    #[error("master key+salt must be 30 bytes, got {0}")]
    ShortMaster(usize),
    /// The AES key length is invalid.
    #[error("AES-128 key must be 16 bytes")]
    KeyLength,
    /// The RTP packet is shorter than its header.
    #[error("RTP packet is truncated")]
    TruncatedRtp,
    /// The SRTP authentication tag did not match.
    #[error("SRTP authentication failed")]
    AuthFailed,
}

/// Authentication coverage and tag profile for SRTP.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SrtpAuthProfile {
    /// Standard RFC 3711 HMAC-SHA1-80: full header (with extension) + body + ROC (BE), 10-byte tag.
    Sha1_80,
    /// HMAC-SHA1-32: full header + body + ROC (BE), 4-byte tag.
    Sha1_32,
    /// HMAC-SHA1-80 without ROC in HMAC, 10-byte tag.
    Sha1_80NoRoc,
    /// HMAC-SHA1-32 without ROC in HMAC, 4-byte tag.
    Sha1_32NoRoc,
    /// HMAC-SHA1-80 covering base header (12 + 4*CC, X bit kept) + body + ROC (BE), 10-byte tag.
    Sha1_80NoExt,
    /// HMAC-SHA1-80 covering base header (12 + 4*CC, X bit cleared) + body + ROC (BE), 10-byte tag.
    Sha1_80NoExtClearX,
    /// HMAC-SHA1-80 with full header + body + ROC (little-endian), 10-byte tag.
    Sha1_80RocLe,
    /// AES-CM-128 with 0-byte authentication tag and IV `(seq = 0, roc = 0)` (`srtpNoSeq`).
    NullAuthNoSeq,
}

impl SrtpAuthProfile {
    /// Returns the authentication tag length in bytes.
    pub const fn tag_len(self) -> usize {
        match self {
            Self::NullAuthNoSeq => 0,
            Self::Sha1_32 | Self::Sha1_32NoRoc => 4,
            Self::Sha1_80
            | Self::Sha1_80NoRoc
            | Self::Sha1_80NoExt
            | Self::Sha1_80NoExtClearX
            | Self::Sha1_80RocLe => 10,
        }
    }

    /// Returns a short human-readable name for logs.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sha1_80 => "tag 10 roc 1",
            Self::Sha1_32 => "tag 4 roc 1",
            Self::Sha1_80NoRoc => "tag 10 roc 0",
            Self::Sha1_32NoRoc => "tag 4 roc 0",
            Self::Sha1_80NoExt => "tag 10 no-ext",
            Self::Sha1_80NoExtClearX => "tag 10 no-ext-clear-x",
            Self::Sha1_80RocLe => "tag 10 roc-le",
            Self::NullAuthNoSeq => "null-auth no-seq",
        }
    }

    const fn uses_zero_seq(self) -> bool {
        matches!(self, Self::NullAuthNoSeq)
    }

    fn from_legacy(tag_len: usize, with_roc: bool) -> Result<Self, SrtpError> {
        match (tag_len, with_roc) {
            (10, true) => Ok(Self::Sha1_80),
            (4, true) => Ok(Self::Sha1_32),
            (10, false) => Ok(Self::Sha1_80NoRoc),
            (4, false) => Ok(Self::Sha1_32NoRoc),
            _ => Err(SrtpError::AuthFailed),
        }
    }
}

/// Derives the RFC 3711 session keys from a 30-byte master key+salt.
///
/// # Errors
///
/// Returns [`SrtpError::ShortMaster`] when fewer than 30 bytes are given.
pub fn session_keys(master: &[u8]) -> Result<SessionKeys, SrtpError> {
    if master.len() < 30 {
        return Err(SrtpError::ShortMaster(master.len()));
    }
    let master_key = &master[..16];
    let master_salt = &master[16..30];
    Ok(SessionKeys {
        enc: kdf(master_key, master_salt, 0x00, 16)?,
        auth: kdf(master_key, master_salt, 0x01, 20)?,
        salt: kdf(master_key, master_salt, 0x02, 14)?,
    })
}

/// Returns the first 30 bytes of the call sessId as the SRTP master.
///
/// # Errors
///
/// Returns [`SrtpError::ShortMaster`] when the sessId is shorter than 30.
pub fn master_from_sess_id(sess_id: &[u8]) -> Result<Vec<u8>, SrtpError> {
    if sess_id.len() < 30 {
        return Err(SrtpError::ShortMaster(sess_id.len()));
    }
    Ok(sess_id[..30].to_vec())
}

/// Builds the 16-byte AES-CM IV for a packet.
pub fn packet_iv(salt: &[u8], ssrc: u32, seq: u16, roc: u32) -> [u8; 16] {
    let idx = (u64::from(roc) << 16) | u64::from(seq);
    let shifted = idx << 16;
    let mut iv = [0_u8; 16];
    iv[4..8].copy_from_slice(&ssrc.to_be_bytes());
    iv[8..12].copy_from_slice(&((shifted >> 32) as u32).to_be_bytes());
    iv[12..16].copy_from_slice(&(shifted as u32).to_be_bytes());
    for (i, byte) in iv.iter_mut().enumerate() {
        *byte ^= salt.get(i).copied().unwrap_or(0);
    }
    iv
}

/// Encrypts an RTP payload and appends the SRTP tag.
///
/// # Errors
///
/// Returns [`SrtpError`] when the HMAC or AES key is invalid.
pub fn protect_rtp(
    header_and_ext: &[u8],
    opus: &[u8],
    keys: &SessionKeys,
    seq: u16,
    ssrc: u32,
    roc: u32,
) -> Result<Vec<u8>, SrtpError> {
    protect_rtp_profile(header_and_ext, opus, keys, seq, ssrc, roc, 10, true)
}

/// Encrypts RTP and appends an HMAC-SHA1 tag of `tag_len` bytes (4 or 10).
///
/// `with_roc` appends the rollover counter to the HMAC, which SHA1-80 does.
/// SHA1-32 is the 4-byte tag. 1-1 stays on [`protect_rtp`].
///
/// # Errors
///
/// Returns [`SrtpError`] when the HMAC or AES key is invalid.
pub fn protect_rtp_profile(
    header_and_ext: &[u8],
    opus: &[u8],
    keys: &SessionKeys,
    seq: u16,
    ssrc: u32,
    roc: u32,
    tag_len: usize,
    with_roc: bool,
) -> Result<Vec<u8>, SrtpError> {
    let profile = SrtpAuthProfile::from_legacy(tag_len, with_roc)?;
    protect_rtp_auth(header_and_ext, opus, keys, seq, ssrc, roc, profile)
}

/// Encrypts RTP and appends an HMAC-SHA1 tag using the selected [`SrtpAuthProfile`].
///
/// # Errors
///
/// Returns [`SrtpError`] when the header is truncated or the key is invalid.
pub fn protect_rtp_auth(
    header_and_ext: &[u8],
    opus: &[u8],
    keys: &SessionKeys,
    seq: u16,
    ssrc: u32,
    roc: u32,
    profile: SrtpAuthProfile,
) -> Result<Vec<u8>, SrtpError> {
    if header_and_ext.len() < 12 {
        return Err(SrtpError::TruncatedRtp);
    }
    let tag_len = profile.tag_len();
    let (iv_seq, iv_roc) = if profile.uses_zero_seq() {
        (0, 0)
    } else {
        (seq, roc)
    };
    let body = aes_cm_xor(keys, ssrc, iv_seq, iv_roc, opus)?;

    let mut out = Vec::with_capacity(header_and_ext.len() + body.len() + tag_len);
    out.extend_from_slice(header_and_ext);
    out.extend_from_slice(&body);
    if tag_len > 0 {
        let tag = compute_srtp_hmac(&keys.auth, header_and_ext, &body, roc, profile)?;
        out.extend_from_slice(&tag[..tag_len]);
    }
    Ok(out)
}

/// Decrypts and authenticates an SRTP packet, returning `(seq, ssrc, opus)`.
///
/// # Errors
///
/// Returns [`SrtpError`] when the packet is truncated or the tag is wrong.
pub fn unprotect_rtp(
    packet: &[u8],
    keys: &SessionKeys,
    roc: u32,
) -> Result<(u16, u32, Vec<u8>), SrtpError> {
    unprotect_rtp_auth(packet, keys, roc, SrtpAuthProfile::Sha1_80)
}

/// Decrypts SRTP with a 4-byte or 10-byte HMAC-SHA1 tag.
///
/// # Errors
///
/// Returns [`SrtpError`] when the packet is truncated or the tag is wrong.
pub fn unprotect_rtp_profile(
    packet: &[u8],
    keys: &SessionKeys,
    roc: u32,
    tag_len: usize,
    with_roc: bool,
) -> Result<(u16, u32, Vec<u8>), SrtpError> {
    let profile = SrtpAuthProfile::from_legacy(tag_len, with_roc)?;
    unprotect_rtp_auth(packet, keys, roc, profile)
}

/// Decrypts SRTP using the selected [`SrtpAuthProfile`].
///
/// # Errors
///
/// Returns [`SrtpError`] when the packet is truncated or the tag is wrong.
pub fn unprotect_rtp_auth(
    packet: &[u8],
    keys: &SessionKeys,
    roc: u32,
    profile: SrtpAuthProfile,
) -> Result<(u16, u32, Vec<u8>), SrtpError> {
    let header = parse_rtp(packet).ok_or(SrtpError::TruncatedRtp)?;
    let tag_len = profile.tag_len();
    if packet.len() < header.header_len + tag_len {
        return Err(SrtpError::TruncatedRtp);
    }
    let body_end = packet.len() - tag_len;
    let body = &packet[header.header_len..body_end];
    if tag_len > 0 {
        let tag = &packet[body_end..];
        let expected =
            compute_srtp_hmac(&keys.auth, &packet[..header.header_len], body, roc, profile)?;
        if !constant_time_eq(&expected[..tag_len], tag) {
            return Err(SrtpError::AuthFailed);
        }
    }

    let (iv_seq, iv_roc) = if profile.uses_zero_seq() {
        (0, 0)
    } else {
        (header.seq, roc)
    };
    let opus = aes_cm_xor(keys, header.ssrc, iv_seq, iv_roc, body)?;
    Ok((header.seq, header.ssrc, opus))
}

/// Decrypts the RTP body with AES-CM without verifying the authentication tag.
///
/// Used strictly for one-time diagnostic classification; never feed the output
/// to audio or video decoders.
///
/// # Errors
///
/// Returns [`SrtpError`] when the packet is truncated or the AES key is invalid.
pub fn decrypt_unverified(
    packet: &[u8],
    keys: &SessionKeys,
    roc: u32,
    tag_len: usize,
) -> Result<(u8, Vec<u8>), SrtpError> {
    let header = parse_rtp(packet).ok_or(SrtpError::TruncatedRtp)?;
    if packet.len() < header.header_len + tag_len {
        return Err(SrtpError::TruncatedRtp);
    }
    let body = &packet[header.header_len..packet.len() - tag_len];
    let plain = aes_cm_xor(keys, header.ssrc, header.seq, roc, body)?;
    Ok((packet[1] & 0x7f, plain))
}

fn aes_cm_xor(
    keys: &SessionKeys,
    ssrc: u32,
    seq: u16,
    roc: u32,
    input: &[u8],
) -> Result<Vec<u8>, SrtpError> {
    let iv = packet_iv(&keys.salt, ssrc, seq, roc);
    let key_stream = aes_cm_expand(&keys.enc, &iv, input.len())?;
    Ok(input
        .iter()
        .zip(key_stream.iter())
        .map(|(a, b)| a ^ b)
        .collect())
}

fn compute_srtp_hmac(
    auth_key: &[u8],
    header_and_ext: &[u8],
    body: &[u8],
    roc: u32,
    profile: SrtpAuthProfile,
) -> Result<[u8; 20], SrtpError> {
    let mut mac = <HmacSha1 as Mac>::new_from_slice(auth_key).map_err(|_| SrtpError::KeyLength)?;
    match profile {
        SrtpAuthProfile::Sha1_80NoExt => {
            let base_len =
                (12 + 4 * usize::from(header_and_ext[0] & 0x0f)).min(header_and_ext.len());
            mac.update(&header_and_ext[..base_len]);
        }
        SrtpAuthProfile::Sha1_80NoExtClearX => {
            let base_len =
                (12 + 4 * usize::from(header_and_ext[0] & 0x0f)).min(header_and_ext.len());
            let mut base = header_and_ext[..base_len].to_vec();
            base[0] &= 0xef;
            mac.update(&base);
        }
        _ => mac.update(header_and_ext),
    }
    mac.update(body);
    match profile {
        SrtpAuthProfile::Sha1_80NoRoc | SrtpAuthProfile::Sha1_32NoRoc => {}
        SrtpAuthProfile::Sha1_80RocLe => mac.update(&roc.to_le_bytes()),
        _ => mac.update(&roc.to_be_bytes()),
    }
    Ok(mac.finalize().into_bytes().into())
}

/// The fields SRTP needs from an RTP header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtpHeader {
    /// Sequence number.
    pub seq: u16,
    /// Synchronization source.
    pub ssrc: u32,
    /// Total header length including CSRC and any extension.
    pub header_len: usize,
}

/// Parses the RTP header, following the optional CSRC list and extension.
pub fn parse_rtp(packet: &[u8]) -> Option<RtpHeader> {
    if packet.len() < 12 {
        return None;
    }
    let extension = (packet[0] >> 4) & 1;
    let csrc_count = (packet[0] & 0x0f) as usize;
    let seq = u16::from_be_bytes([packet[2], packet[3]]);
    let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
    let mut offset = 12 + 4 * csrc_count;
    if extension == 1 {
        if packet.len() < offset + 4 {
            return None;
        }
        let words = u16::from_be_bytes([packet[offset + 2], packet[offset + 3]]) as usize;
        offset += 4 + 4 * words;
    }
    if offset > packet.len() {
        return None;
    }
    Some(RtpHeader {
        seq,
        ssrc,
        header_len: offset,
    })
}

fn kdf(
    master_key: &[u8],
    master_salt: &[u8],
    label: u8,
    out_len: usize,
) -> Result<Vec<u8>, SrtpError> {
    let mut x = [0_u8; 16];
    let n = master_salt.len().min(14);
    x[..n].copy_from_slice(&master_salt[..n]);
    x[7] ^= label;
    aes_cm_expand(master_key, &x, out_len)
}

fn aes_cm_expand(key: &[u8], iv: &[u8; 16], out_len: usize) -> Result<Vec<u8>, SrtpError> {
    let cipher = Aes128::new_from_slice(key).map_err(|_| SrtpError::KeyLength)?;
    let mut out = Vec::with_capacity(out_len);
    let mut counter = 0_u16;
    while out.len() < out_len {
        let mut block = *iv;
        block[14] = (counter >> 8) as u8;
        block[15] = counter as u8;
        let mut encrypted = GenericArray::clone_from_slice(&block);
        cipher.encrypt_block(&mut encrypted);
        let take = (out_len - out.len()).min(16);
        out.extend_from_slice(&encrypted[..take]);
        counter = counter.wrapping_add(1);
    }
    Ok(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0_u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> Vec<u8> {
        hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d")
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn derives_the_reference_session_keys() {
        let keys = session_keys(&master()).expect("keys");
        assert_eq!(hex_encode(&keys.enc), "7e52987945787ea107d93f0d54631a6f");
        assert_eq!(
            hex_encode(&keys.auth),
            "d407ce49f85990a04c3fb0b59c3e86dc951517aa"
        );
        assert_eq!(hex_encode(&keys.salt), "117507eab2655d2c31d1b1b3c454");
    }

    #[test]
    fn builds_the_reference_packet_iv() {
        let keys = session_keys(&master()).expect("keys");
        let iv = packet_iv(&keys.salt, 0x0102_0304, 5, 0);
        assert_eq!(hex_encode(&iv), "117507eab3675e2831d1b1b3c4510000");
    }

    #[test]
    fn protect_matches_the_reference_vector() {
        let keys = session_keys(&master()).expect("keys");
        let header = hex("800000050000000001020304");
        let opus = hex("01020304");
        let protected = protect_rtp(&header, &opus, &keys, 5, 0x0102_0304, 0).expect("protect");
        assert_eq!(
            hex_encode(&protected),
            "80000005000000000102030487305d7c84c99a86b0643ffc7361"
        );
    }

    #[test]
    fn round_trips_opus_through_protect_and_unprotect() {
        let keys = session_keys(&master()).expect("keys");
        let header = hex("800000050000000001020304");
        let opus = hex("deadbeef01020304");
        let protected = protect_rtp(&header, &opus, &keys, 5, 0x0102_0304, 0).expect("protect");
        let (seq, ssrc, decoded) = unprotect_rtp(&protected, &keys, 0).expect("unprotect");
        assert_eq!(seq, 5);
        assert_eq!(ssrc, 0x0102_0304);
        assert_eq!(decoded, opus);
    }

    #[test]
    fn rejects_a_tampered_packet() {
        let keys = session_keys(&master()).expect("keys");
        let header = hex("800000050000000001020304");
        let opus = hex("01020304");
        let mut protected = protect_rtp(&header, &opus, &keys, 5, 0x0102_0304, 0).expect("protect");
        let last = protected.len() - 1;
        protected[last] ^= 0xff;
        assert!(matches!(
            unprotect_rtp(&protected, &keys, 0),
            Err(SrtpError::AuthFailed)
        ));
    }

    #[test]
    fn round_trips_no_ext_and_roc_le_profiles() {
        let keys = session_keys(&master()).expect("keys");
        // RTP header with X=1 and 1-word BEDE extension (16 bytes total).
        let header = hex("907000050000000001020304bede000112345678");
        let payload = hex("000000016742e01e");
        for profile in [
            SrtpAuthProfile::Sha1_80NoExt,
            SrtpAuthProfile::Sha1_80NoExtClearX,
            SrtpAuthProfile::Sha1_80RocLe,
        ] {
            let mut pkt = protect_rtp_auth(&header, &payload, &keys, 5, 0x0102_0304, 2, profile)
                .expect("protect");
            if matches!(
                profile,
                SrtpAuthProfile::Sha1_80NoExt | SrtpAuthProfile::Sha1_80NoExtClearX
            ) {
                // Simulate SFU modifying the extension payload after signing.
                pkt[16] ^= 0xff;
            }
            let (seq, ssrc, decoded) =
                unprotect_rtp_auth(&pkt, &keys, 2, profile).expect("unprotect");
            assert_eq!(seq, 5);
            assert_eq!(ssrc, 0x0102_0304);
            assert_eq!(decoded, payload);
        }
        let pkt = protect_rtp_auth(
            &header,
            &payload,
            &keys,
            5,
            0x0102_0304,
            0,
            SrtpAuthProfile::Sha1_80,
        )
        .expect("protect");
        let (pt, plain) = decrypt_unverified(&pkt, &keys, 0, 10).expect("probe");
        assert_eq!(pt, 0x70);
        assert_eq!(plain, payload);
    }

    #[test]
    fn round_trips_null_auth_no_seq() {
        let keys = session_keys(&master()).expect("keys");
        // 28-byte group audio header with seq=0x01f9 and 8-byte payload (36 bytes total on wire).
        let header = hex("907001f90000014000000037bede00031102200a2100023200010200");
        assert_eq!(header.len(), 28);
        let payload = hex("f8fffe0102030405");
        let pkt = protect_rtp_auth(
            &header,
            &payload,
            &keys,
            0x01f9,
            0x37,
            7,
            SrtpAuthProfile::NullAuthNoSeq,
        )
        .expect("protect");
        assert_eq!(pkt.len(), 36);
        assert_eq!(&pkt[..28], header.as_slice());

        // Ciphertext uses (seq=0, roc=0) IV even though wire seq=0x01f9 and caller passed roc=7.
        let zero_seq_stream =
            aes_cm_expand(&keys.enc, &packet_iv(&keys.salt, 0x37, 0, 0), 8).expect("stream");
        let expected_cipher: Vec<u8> = payload
            .iter()
            .zip(zero_seq_stream.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        assert_eq!(&pkt[28..], expected_cipher.as_slice());

        let (seq, ssrc, decoded) =
            unprotect_rtp_auth(&pkt, &keys, 99, SrtpAuthProfile::NullAuthNoSeq).expect("unprotect");
        assert_eq!(seq, 0x01f9);
        assert_eq!(ssrc, 0x37);
        assert_eq!(decoded, payload);
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
