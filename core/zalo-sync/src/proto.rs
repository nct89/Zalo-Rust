//! Wire-exact protobuf codec for Electron's Sync 2 chunks
//! (`proto.Sync2.Conversation.SyncChunk` / module `Ho1Y` and
//! `proto.Sync2.Message.SyncChunk` / module `1y5R`), plus 4-byte LE block framing
//! and zstd stream decompression.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// Errors decoding framed Sync 2 payloads, zstd streams, or protobuf chunks.
#[derive(Debug, Error)]
pub enum ProtoError {
    /// Truncated varint, string, or length-delimited field.
    #[error("truncated protobuf payload: {0}")]
    Truncated(&'static str),
    /// Invalid UTF-8 inside a protobuf string field.
    #[error("invalid utf-8 in protobuf string: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    /// Unsupported wire type.
    #[error("unsupported protobuf wire type {0}")]
    WireType(u8),
    /// Zstd compression or decompression failed.
    #[error("zstd error: {0}")]
    Zstd(#[from] std::io::Error),
    /// Framed binary stream had an invalid length prefix.
    #[error("truncated framed block: need {need} bytes, have {have}")]
    BadFrame {
        /// Declared length in the u32 LE header.
        need: usize,
        /// Remaining bytes in the buffer.
        have: usize,
    },
}

/// `proto.Sync2.Shared.PlatformType` (module `N3ek`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum PlatformType {
    /// `PLATFORM_TYPE_UNSPECIFIED = 0`
    #[default]
    Unspecified = 0,
    /// `PLATFORM_TYPE_MOBILE = 1`
    Mobile = 1,
    /// `PLATFORM_TYPE_WEB = 2`
    Web = 2,
    /// `PLATFORM_TYPE_PC = 3`
    Pc = 3,
    /// `PLATFORM_TYPE_SYSTEM = 4`
    System = 4,
}

impl PlatformType {
    /// Converts a protobuf enum value.
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Mobile,
            2 => Self::Web,
            3 => Self::Pc,
            4 => Self::System,
            _ => Self::Unspecified,
        }
    }
}

/// `proto.Sync2.Conversation.ConvType` (module `Ho1Y`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum ConvType {
    /// `CONV_TYPE_UNSPECIFIED = 0`
    #[default]
    Unspecified = 0,
    /// `CONV_TYPE_ONEONE = 1`
    OneOne = 1,
    /// `CONV_TYPE_GROUP = 2`
    Group = 2,
}

impl ConvType {
    /// Converts a protobuf enum value.
    #[must_use]
    pub const fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::OneOne,
            2 => Self::Group,
            _ => Self::Unspecified,
        }
    }
}

/// `proto.Sync2.Conversation.SyncChunk`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSyncChunk {
    /// Field 1: `platform`.
    pub platform: PlatformType,
    /// Field 2: repeated `conversations`.
    pub conversations: Vec<SyncConversation>,
}

/// `proto.Sync2.Conversation.Conversation`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncConversation {
    /// Field 1: `conv_id` (global noised id).
    pub conv_id: String,
    /// Field 2: `conv_type`.
    pub conv_type: ConvType,
    /// Field 3: `responded_by_me`.
    pub responded_by_me: bool,
    /// Field 4: `last_ts` (Unix ms).
    pub last_ts: u64,
    /// Field 5: `last_global_id` (uint64 rendered as decimal string).
    pub last_global_id: String,
    /// Field 6: `last_client_id` (uint64).
    pub last_client_id: u64,
}

/// `proto.Sync2.Message.SyncChunk`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSyncChunk {
    /// Field 1: `platform`.
    pub platform: PlatformType,
    /// Field 2: repeated `partitions`.
    pub partitions: Vec<MessagePartition>,
}

/// `proto.Sync2.Message.Partition`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePartition {
    /// Field 1: `id` (`"group/{globalId}"` or `"oneone/{globalId}"`).
    pub id: String,
    /// Field 2: repeated `messages`.
    pub messages: Vec<SyncMessage>,
}

/// `proto.Sync2.Message.Message`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMessage {
    /// Field 1: `sender_id` (global noised id).
    pub sender_id: String,
    /// Field 2: `client_id`.
    pub client_id: u64,
    /// Field 3: `msg_status`.
    pub msg_status: i32,
    /// Field 4: `msg_type` (enum `xi`).
    pub msg_type: i32,
    /// Field 5: `timestamp` (Unix ms).
    pub timestamp: u64,
    /// Field 6: `global_id` (int64 rendered as decimal string).
    pub global_id: String,
    /// Field 7: `content`.
    pub content: String,
    /// Field 8: optional `meta`.
    pub meta: Option<SyncMetaData>,
}

/// `proto.Sync2.Message.MetaData`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMetaData {
    /// Field 1: repeated `attachs`.
    pub attachs: Vec<SyncAttach>,
    /// Field 2: repeated `mentions`.
    pub mentions: Vec<SyncMention>,
    /// Field 3: optional `reference`.
    pub reference: Option<SyncReference>,
    /// Field 4: optional `quote`.
    pub quote: Option<SyncQuote>,
    /// Field 5: optional `property`.
    pub property: Option<SyncProperties>,
    /// Field 6: optional `ttl`.
    pub ttl: Option<u64>,
}

/// `proto.Sync2.Message.Attach`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAttach {
    /// Field 1: `type`.
    pub attach_type: Option<String>,
    /// Field 2: `ext_info`.
    pub ext_info: Option<String>,
    /// Field 3: `action`.
    pub action: Option<String>,
    /// Field 4: `params`.
    pub params: Option<String>,
    /// Field 5: `title`.
    pub title: Option<String>,
    /// Field 6: `href`.
    pub href: Option<String>,
    /// Field 7: `thumb`.
    pub thumb: Option<String>,
    /// Field 8: `description`.
    pub description: Option<String>,
    /// Field 9: `cat_id`.
    pub cat_id: Option<i32>,
    /// Field 10: `id`.
    pub id: Option<i32>,
    /// Field 11: `child_number`.
    pub child_number: Option<i32>,
    /// Field 12: `remains`.
    pub remains: Option<String>,
    /// Field 13: `zinstant_data`.
    pub zinstant_data: Option<String>,
    /// Field 14: `zinstant_msg`.
    pub zinstant_msg: Option<String>,
}

/// `proto.Sync2.Message.Mention`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMention {
    /// Field 1: `uid` (global noised id or `"-1"` for mention-all).
    pub uid: String,
    /// Field 2: `pos`.
    pub pos: i32,
    /// Field 3: `len`.
    pub len: i32,
    /// Field 4: `type`.
    pub mention_type: i32,
}

/// `proto.Sync2.Message.Reference`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReference {
    /// Field 1: `type`.
    pub ref_type: i32,
    /// Field 2: `data`.
    pub data: String,
}

/// `proto.Sync2.Message.Quote`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncQuote {
    /// Field 1: `attach`.
    pub attach: String,
    /// Field 2: `owner_id`.
    pub owner_id: String,
    /// Field 3: `ts`.
    pub ts: i64,
    /// Field 4: `from_d`.
    pub from_d: String,
    /// Field 5: `cli_msg_id`.
    pub cli_msg_id: u64,
    /// Field 6: `global_msg_id`.
    pub global_msg_id: String,
    /// Field 7: `msg`.
    pub msg: String,
    /// Field 8: `cli_msg_type`.
    pub cli_msg_type: i32,
    /// Field 9: optional `ttl`.
    pub ttl: Option<u64>,
    /// Field 10: optional `partial_quote`.
    pub partial_quote: Option<String>,
}

/// `proto.Sync2.Message.Properties`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncProperties {
    /// Field 1: `size`.
    pub size: Option<i32>,
    /// Field 2: `color`.
    pub color: Option<i32>,
    /// Field 3: `type`.
    pub prop_type: Option<i32>,
    /// Field 4: `sub_type`.
    pub sub_type: Option<i32>,
    /// Field 5: `ext`.
    pub ext: Option<String>,
}

/// Reads Electron's 4-byte little-endian length-prefixed blocks from a downloaded
/// `msgUrl` payload (`shared-worker` lines 9581–9588).
pub fn read_framed_blocks(bytes: &[u8]) -> Result<Vec<Vec<u8>>, ProtoError> {
    let mut blocks = Vec::new();
    let mut pos = 0usize;
    while pos < bytes.len() {
        if bytes.len() - pos < 4 {
            return Err(ProtoError::BadFrame {
                need: 4,
                have: bytes.len() - pos,
            });
        }
        let len = u32::from_le_bytes([
            bytes[pos],
            bytes[pos + 1],
            bytes[pos + 2],
            bytes[pos + 3],
        ]) as usize;
        pos += 4;
        if bytes.len() - pos < len {
            return Err(ProtoError::BadFrame {
                need: len,
                have: bytes.len() - pos,
            });
        }
        blocks.push(bytes[pos..pos + len].to_vec());
        pos += len;
    }
    Ok(blocks)
}

/// Frames blocks with Electron's 4-byte little-endian length prefix.
#[must_use]
pub fn write_framed_blocks(blocks: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for block in blocks {
        out.extend_from_slice(&(block.len() as u32).to_le_bytes());
        out.extend_from_slice(block);
    }
    out
}

/// Decompresses a zstd stream (`ZstdStream.decompress`).
pub fn zstd_decompress(data: &[u8]) -> Result<Vec<u8>, ProtoError> {
    Ok(zstd::stream::decode_all(data)?)
}

/// Compresses a buffer into a zstd stream (`ZstdStream.compress`).
pub fn zstd_compress(data: &[u8]) -> Result<Vec<u8>, ProtoError> {
    Ok(zstd::stream::encode_all(data, 3)?)
}

// ---------------------------------------------------------------------------
// Protobuf primitive reader & writer (proto3 wire format)
// ---------------------------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn has_more(&self) -> bool {
        self.pos < self.buf.len()
    }

    fn read_varint(&mut self) -> Result<u64, ProtoError> {
        let mut val = 0u64;
        let mut shift = 0u32;
        while self.pos < self.buf.len() {
            let b = self.buf[self.pos];
            self.pos += 1;
            if shift < 64 {
                val |= u64::from(b & 0x7f) << shift;
            }
            if b & 0x80 == 0 {
                return Ok(val);
            }
            shift += 7;
            if shift >= 70 {
                return Err(ProtoError::Truncated("varint overflow"));
            }
        }
        Err(ProtoError::Truncated("varint"))
    }

    fn read_tag(&mut self) -> Result<(u32, u8), ProtoError> {
        let tag = self.read_varint()?;
        Ok(((tag >> 3) as u32, (tag & 0x7) as u8))
    }

    fn read_bytes(&mut self) -> Result<&'a [u8], ProtoError> {
        let len = self.read_varint()? as usize;
        if self.buf.len() - self.pos < len {
            return Err(ProtoError::Truncated("length-delimited"));
        }
        let slice = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn read_string(&mut self) -> Result<String, ProtoError> {
        let bytes = self.read_bytes()?;
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    fn skip(&mut self, wire: u8) -> Result<(), ProtoError> {
        match wire {
            0 => {
                let _ = self.read_varint()?;
                Ok(())
            }
            1 => {
                if self.buf.len() - self.pos < 8 {
                    return Err(ProtoError::Truncated("64-bit"));
                }
                self.pos += 8;
                Ok(())
            }
            2 => {
                let _ = self.read_bytes()?;
                Ok(())
            }
            5 => {
                if self.buf.len() - self.pos < 4 {
                    return Err(ProtoError::Truncated("32-bit"));
                }
                self.pos += 4;
                Ok(())
            }
            other => Err(ProtoError::WireType(other)),
        }
    }
}

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn write_varint(&mut self, mut v: u64) {
        loop {
            let mut b = (v & 0x7f) as u8;
            v >>= 7;
            if v != 0 {
                b |= 0x80;
            }
            self.buf.push(b);
            if v == 0 {
                break;
            }
        }
    }

    fn write_tag(&mut self, field: u32, wire: u8) {
        self.write_varint((u64::from(field) << 3) | u64::from(wire));
    }

    fn write_string(&mut self, field: u32, s: &str) {
        self.write_tag(field, 2);
        self.write_varint(s.len() as u64);
        self.buf.extend_from_slice(s.as_bytes());
    }

    fn write_bytes_field(&mut self, field: u32, data: &[u8]) {
        self.write_tag(field, 2);
        self.write_varint(data.len() as u64);
        self.buf.extend_from_slice(data);
    }

    fn write_u64(&mut self, field: u32, v: u64) {
        self.write_tag(field, 0);
        self.write_varint(v);
    }

    fn write_i64(&mut self, field: u32, v: i64) {
        self.write_tag(field, 0);
        self.write_varint(v as u64);
    }

    fn write_i32(&mut self, field: u32, v: i32) {
        self.write_tag(field, 0);
        self.write_varint(i64::from(v) as u64);
    }

    fn write_bool(&mut self, field: u32, v: bool) {
        self.write_tag(field, 0);
        self.write_varint(u64::from(v));
    }
}

// ---------------------------------------------------------------------------
// Conversation SyncChunk codec (`Ho1Y`)
// ---------------------------------------------------------------------------

impl ConversationSyncChunk {
    /// Decodes `proto.Sync2.Conversation.SyncChunk`.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 0) => out.platform = PlatformType::from_i32(r.read_varint()? as i32),
                (2, 2) => out.conversations.push(SyncConversation::decode(r.read_bytes()?)?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    /// Encodes `proto.Sync2.Conversation.SyncChunk`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if self.platform as i32 != 0 {
            w.write_i32(1, self.platform as i32);
        }
        for c in &self.conversations {
            w.write_bytes_field(2, &c.encode());
        }
        w.buf
    }
}

impl SyncConversation {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self {
            last_global_id: "0".to_owned(),
            ..Self::default()
        };
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.conv_id = r.read_string()?,
                (2, 0) => out.conv_type = ConvType::from_i32(r.read_varint()? as i32),
                (3, 0) => out.responded_by_me = r.read_varint()? != 0,
                (4, 0) => out.last_ts = r.read_varint()?,
                (5, 0) => out.last_global_id = r.read_varint()?.to_string(),
                (6, 0) => out.last_client_id = r.read_varint()?,
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if !self.conv_id.is_empty() {
            w.write_string(1, &self.conv_id);
        }
        if self.conv_type as i32 != 0 {
            w.write_i32(2, self.conv_type as i32);
        }
        if self.responded_by_me {
            w.write_bool(3, true);
        }
        if self.last_ts != 0 {
            w.write_u64(4, self.last_ts);
        }
        if let Ok(v) = self.last_global_id.parse::<u64>() {
            if v != 0 {
                w.write_u64(5, v);
            }
        }
        if self.last_client_id != 0 {
            w.write_u64(6, self.last_client_id);
        }
        w.buf
    }
}

// ---------------------------------------------------------------------------
// Message SyncChunk codec (`1y5R`)
// ---------------------------------------------------------------------------

impl MessageSyncChunk {
    /// Decodes `proto.Sync2.Message.SyncChunk`.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 0) => out.platform = PlatformType::from_i32(r.read_varint()? as i32),
                (2, 2) => out.partitions.push(MessagePartition::decode(r.read_bytes()?)?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    /// Encodes `proto.Sync2.Message.SyncChunk`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if self.platform as i32 != 0 {
            w.write_i32(1, self.platform as i32);
        }
        for p in &self.partitions {
            w.write_bytes_field(2, &p.encode());
        }
        w.buf
    }
}

impl MessagePartition {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.id = r.read_string()?,
                (2, 2) => out.messages.push(SyncMessage::decode(r.read_bytes()?)?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if !self.id.is_empty() {
            w.write_string(1, &self.id);
        }
        for m in &self.messages {
            w.write_bytes_field(2, &m.encode());
        }
        w.buf
    }
}

impl SyncMessage {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self {
            global_id: "0".to_owned(),
            ..Self::default()
        };
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.sender_id = r.read_string()?,
                (2, 0) => out.client_id = r.read_varint()?,
                (3, 0) => out.msg_status = r.read_varint()? as i32,
                (4, 0) => out.msg_type = r.read_varint()? as i32,
                (5, 0) => out.timestamp = r.read_varint()?,
                (6, 0) => out.global_id = (r.read_varint()? as i64).to_string(),
                (7, 2) => out.content = r.read_string()?,
                (8, 2) => out.meta = Some(SyncMetaData::decode(r.read_bytes()?)?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if !self.sender_id.is_empty() {
            w.write_string(1, &self.sender_id);
        }
        if self.client_id != 0 {
            w.write_u64(2, self.client_id);
        }
        if self.msg_status != 0 {
            w.write_i32(3, self.msg_status);
        }
        if self.msg_type != 0 {
            w.write_i32(4, self.msg_type);
        }
        if self.timestamp != 0 {
            w.write_u64(5, self.timestamp);
        }
        if let Ok(v) = self.global_id.parse::<i64>() {
            if v != 0 {
                w.write_i64(6, v);
            }
        }
        if !self.content.is_empty() {
            w.write_string(7, &self.content);
        }
        if let Some(meta) = &self.meta {
            w.write_bytes_field(8, &meta.encode());
        }
        w.buf
    }
}

impl SyncMetaData {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.attachs.push(SyncAttach::decode(r.read_bytes()?)?),
                (2, 2) => out.mentions.push(SyncMention::decode(r.read_bytes()?)?),
                (3, 2) => out.reference = Some(SyncReference::decode(r.read_bytes()?)?),
                (4, 2) => out.quote = Some(SyncQuote::decode(r.read_bytes()?)?),
                (5, 2) => out.property = Some(SyncProperties::decode(r.read_bytes()?)?),
                (6, 0) => out.ttl = Some(r.read_varint()?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        for a in &self.attachs {
            w.write_bytes_field(1, &a.encode());
        }
        for m in &self.mentions {
            w.write_bytes_field(2, &m.encode());
        }
        if let Some(rf) = &self.reference {
            w.write_bytes_field(3, &rf.encode());
        }
        if let Some(q) = &self.quote {
            w.write_bytes_field(4, &q.encode());
        }
        if let Some(p) = &self.property {
            w.write_bytes_field(5, &p.encode());
        }
        if let Some(ttl) = self.ttl {
            w.write_u64(6, ttl);
        }
        w.buf
    }
}

impl SyncAttach {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.attach_type = Some(r.read_string()?),
                (2, 2) => out.ext_info = Some(r.read_string()?),
                (3, 2) => out.action = Some(r.read_string()?),
                (4, 2) => out.params = Some(r.read_string()?),
                (5, 2) => out.title = Some(r.read_string()?),
                (6, 2) => out.href = Some(r.read_string()?),
                (7, 2) => out.thumb = Some(r.read_string()?),
                (8, 2) => out.description = Some(r.read_string()?),
                (9, 0) => out.cat_id = Some(r.read_varint()? as i32),
                (10, 0) => out.id = Some(r.read_varint()? as i32),
                (11, 0) => out.child_number = Some(r.read_varint()? as i32),
                (12, 2) => out.remains = Some(r.read_string()?),
                (13, 2) => out.zinstant_data = Some(r.read_string()?),
                (14, 2) => out.zinstant_msg = Some(r.read_string()?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if let Some(v) = &self.attach_type {
            w.write_string(1, v);
        }
        if let Some(v) = &self.ext_info {
            w.write_string(2, v);
        }
        if let Some(v) = &self.action {
            w.write_string(3, v);
        }
        if let Some(v) = &self.params {
            w.write_string(4, v);
        }
        if let Some(v) = &self.title {
            w.write_string(5, v);
        }
        if let Some(v) = &self.href {
            w.write_string(6, v);
        }
        if let Some(v) = &self.thumb {
            w.write_string(7, v);
        }
        if let Some(v) = &self.description {
            w.write_string(8, v);
        }
        if let Some(v) = self.cat_id {
            w.write_i32(9, v);
        }
        if let Some(v) = self.id {
            w.write_i32(10, v);
        }
        if let Some(v) = self.child_number {
            w.write_i32(11, v);
        }
        if let Some(v) = &self.remains {
            w.write_string(12, v);
        }
        if let Some(v) = &self.zinstant_data {
            w.write_string(13, v);
        }
        if let Some(v) = &self.zinstant_msg {
            w.write_string(14, v);
        }
        w.buf
    }

    /// Converts an attachment into the JSON object expected by `parse_item` / `syncMessage2SVMessage`.
    #[must_use]
    pub fn to_server_json(&self) -> Value {
        let mut obj = serde_json::Map::new();
        if let Some(v) = &self.attach_type {
            obj.insert("type".into(), json!(v));
        }
        if let Some(v) = &self.ext_info {
            obj.insert("extInfo".into(), json!(v));
        }
        if let Some(v) = &self.action {
            obj.insert("action".into(), json!(v));
        }
        if let Some(v) = &self.params {
            obj.insert("params".into(), json!(v));
        }
        if let Some(v) = &self.title {
            obj.insert("title".into(), json!(v));
        }
        if let Some(v) = &self.href {
            obj.insert("href".into(), json!(v));
        }
        if let Some(v) = &self.thumb {
            obj.insert("thumb".into(), json!(v));
        }
        if let Some(v) = &self.description {
            obj.insert("description".into(), json!(v));
        }
        if let Some(v) = self.cat_id {
            obj.insert("catId".into(), json!(v));
        }
        if let Some(v) = self.id {
            obj.insert("id".into(), json!(v));
        }
        if let Some(v) = self.child_number {
            obj.insert("childnumber".into(), json!(v));
        }
        if let Some(v) = &self.remains {
            obj.insert("remains".into(), json!(v));
        }
        if let Some(v) = &self.zinstant_data {
            obj.insert("zinstant_data".into(), json!(v));
        }
        if let Some(v) = &self.zinstant_msg {
            obj.insert("zinstant_msg".into(), json!(v));
        }
        Value::Object(obj)
    }
}

impl SyncMention {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.uid = r.read_string()?,
                (2, 0) => out.pos = r.read_varint()? as i32,
                (3, 0) => out.len = r.read_varint()? as i32,
                (4, 0) => out.mention_type = r.read_varint()? as i32,
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if !self.uid.is_empty() {
            w.write_string(1, &self.uid);
        }
        if self.pos != 0 {
            w.write_i32(2, self.pos);
        }
        if self.len != 0 {
            w.write_i32(3, self.len);
        }
        if self.mention_type != 0 {
            w.write_i32(4, self.mention_type);
        }
        w.buf
    }
}

impl SyncReference {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 0) => out.ref_type = r.read_varint()? as i32,
                (2, 2) => out.data = r.read_string()?,
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if self.ref_type != 0 {
            w.write_i32(1, self.ref_type);
        }
        if !self.data.is_empty() {
            w.write_string(2, &self.data);
        }
        w.buf
    }
}

impl SyncQuote {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self {
            global_msg_id: "0".to_owned(),
            ..Self::default()
        };
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 2) => out.attach = r.read_string()?,
                (2, 2) => out.owner_id = r.read_string()?,
                (3, 0) => out.ts = r.read_varint()? as i64,
                (4, 2) => out.from_d = r.read_string()?,
                (5, 0) => out.cli_msg_id = r.read_varint()?,
                (6, 0) => out.global_msg_id = r.read_varint()?.to_string(),
                (7, 2) => out.msg = r.read_string()?,
                (8, 0) => out.cli_msg_type = r.read_varint()? as i32,
                (9, 0) => out.ttl = Some(r.read_varint()?),
                (10, 2) => out.partial_quote = Some(r.read_string()?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if !self.attach.is_empty() {
            w.write_string(1, &self.attach);
        }
        if !self.owner_id.is_empty() {
            w.write_string(2, &self.owner_id);
        }
        if self.ts != 0 {
            w.write_i64(3, self.ts);
        }
        if !self.from_d.is_empty() {
            w.write_string(4, &self.from_d);
        }
        if self.cli_msg_id != 0 {
            w.write_u64(5, self.cli_msg_id);
        }
        if let Ok(v) = self.global_msg_id.parse::<u64>() {
            if v != 0 {
                w.write_u64(6, v);
            }
        }
        if !self.msg.is_empty() {
            w.write_string(7, &self.msg);
        }
        if self.cli_msg_type != 0 {
            w.write_i32(8, self.cli_msg_type);
        }
        if let Some(ttl) = self.ttl {
            w.write_u64(9, ttl);
        }
        if let Some(pq) = &self.partial_quote {
            w.write_string(10, pq);
        }
        w.buf
    }
}

impl SyncProperties {
    fn decode(bytes: &[u8]) -> Result<Self, ProtoError> {
        let mut r = Reader::new(bytes);
        let mut out = Self::default();
        while r.has_more() {
            let (field, wire) = r.read_tag()?;
            match (field, wire) {
                (1, 0) => out.size = Some(r.read_varint()? as i32),
                (2, 0) => out.color = Some(r.read_varint()? as i32),
                (3, 0) => out.prop_type = Some(r.read_varint()? as i32),
                (4, 0) => out.sub_type = Some(r.read_varint()? as i32),
                (5, 2) => out.ext = Some(r.read_string()?),
                _ => r.skip(wire)?,
            }
        }
        Ok(out)
    }

    fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        if let Some(v) = self.size {
            w.write_i32(1, v);
        }
        if let Some(v) = self.color {
            w.write_i32(2, v);
        }
        if let Some(v) = self.prop_type {
            w.write_i32(3, v);
        }
        if let Some(v) = self.sub_type {
            w.write_i32(4, v);
        }
        if let Some(v) = &self.ext {
            w.write_string(5, v);
        }
        w.buf
    }
}

/// Maps Electron's Sync 2 `msgType` integer (`xi` enum) to server `msgType` string (`ji` map).
#[must_use]
pub fn sync_msg_type_to_server_str(msg_type: i32) -> Option<&'static str> {
    match msg_type {
        0 => Some("webchat"),
        2 => Some("chat.doodle"),
        3 | 4 | 31 => Some("chat.photo"),
        6 => Some("chat.voice"),
        10 => Some("chat.sticker"),
        12 => Some("chat.recommended"),
        15 => Some("chat.list.action"),
        18 => Some("chat.location.new"),
        19 => Some("chat.video.msg"),
        20 => Some("chat.info"),
        21 => Some("chat.ecard"),
        22 => Some("share.file"),
        23 => Some("chat.gif"),
        24 => Some("chat.webcontent"),
        25 => Some("chat.video.live.msg"),
        26 => Some("group.poll"),
        33 => Some("chat.delete"),
        36 => Some("chat.undo"),
        53 => Some("chat.webcontent.v2"),
        _ => None,
    }
}

/// Converts a decoded [`SyncMessage`] into Electron's server-message JSON shape
/// (`yl.syncMessage2SVMessage` in `shared-worker.d6af5465eb38c618bfe0.js`),
/// ready to feed into the client's existing message parser & `LocalStore`.
///
/// `resolve_user` converts a global noised user id into a PC user id.
#[must_use]
pub fn sync_message_to_server_json<F>(
    msg: &SyncMessage,
    pc_conv_id: &str,
    self_uid: &str,
    mut resolve_user: F,
) -> Option<Value>
where
    F: FnMut(&str) -> String,
{
    let mut server_type = sync_msg_type_to_server_str(msg.msg_type)?.to_owned();
    let sender_pc = resolve_user(&msg.sender_id);
    let uid_from = if sender_pc == self_uid || sender_pc == "0" {
        "0".to_owned()
    } else {
        sender_pc
    };

    let meta = msg.meta.as_ref();
    let first_attach = meta.and_then(|m| m.attachs.first());

    let content: Value = match msg.msg_type {
        // Text (or RTF text when attach.action == "rtf")
        0 => {
            if let Some(a) = first_attach {
                if a.action.as_deref() == Some("rtf") {
                    let mut obj = a.to_server_json();
                    if a.title.as_deref().unwrap_or("").is_empty() {
                        if let Some(m) = obj.as_object_mut() {
                            m.insert("title".into(), json!(msg.content));
                        }
                    }
                    obj
                } else {
                    json!(msg.content)
                }
            } else {
                json!(msg.content)
            }
        }
        // Photo / Photo_V2 / PhotoV2 / Gift / Sticker / Voice / Video / Location / Doodle / File / ECard / Poll
        2 | 3 | 4 | 6 | 10 | 18 | 19 | 21 | 22 | 23 | 25 | 26 | 31 => first_attach
            .map(SyncAttach::to_server_json)
            .unwrap_or_else(|| json!({})),
        // Contact: denoise params when action is recommened.user / recommened.vip
        12 => {
            let mut obj = first_attach
                .map(SyncAttach::to_server_json)
                .unwrap_or_else(|| json!({}));
            if let Some(m) = obj.as_object_mut() {
                let act = m.get("action").and_then(Value::as_str).unwrap_or("");
                if matches!(act, "recommened.user" | "recommened.vip") {
                    if let Some(p) = m.get("params").and_then(Value::as_str).map(str::to_owned) {
                        if !p.is_empty() {
                            m.insert("params".into(), json!(resolve_user(&p)));
                        }
                    }
                }
            }
            obj
        }
        // OA: array of attachments
        15 => Value::Array(
            meta.map(|m| m.attachs.iter().map(SyncAttach::to_server_json).collect())
                .unwrap_or_default(),
        ),
        // Undo
        36 => json!(""),
        // Webcontent: skip if missing pcItem
        24 => {
            let a = first_attach?;
            if !a
                .params
                .as_deref()
                .is_some_and(|p| p.contains("pcItem"))
            {
                return None;
            }
            a.to_server_json()
        }
        // WebcontentV2
        53 => {
            let a = first_attach;
            json!({
                "zinstant_msg": a.and_then(|x| x.zinstant_msg.as_deref()).unwrap_or(""),
                "zinstant_data": a.and_then(|x| x.zinstant_data.as_deref()).unwrap_or(""),
                "action": a.and_then(|x| x.action.as_deref()).unwrap_or(""),
            })
        }
        _ => json!(msg.content),
    };

    let mut uid_sender_del = String::new();
    if msg.msg_type == 36 {
        if let Some(params) = first_attach.and_then(|a| a.params.as_deref()) {
            if let Ok(parsed) = serde_json::from_str::<Value>(params) {
                if let Some(cm) = parsed.get("custom_message").and_then(Value::as_str) {
                    if let Ok(cm_val) = serde_json::from_str::<Value>(cm) {
                        if let Some(uid) = cm_val
                            .get("highLightsV2")
                            .and_then(Value::as_array)
                            .and_then(|arr| arr.first())
                            .and_then(|item| item.get("uid"))
                            .and_then(Value::as_str)
                        {
                            let pc = resolve_user(uid);
                            uid_sender_del = if pc == self_uid { "0".to_owned() } else { pc };
                            server_type = "chat.delete.everyone".to_owned();
                        }
                    }
                }
            }
        }
    }

    let mentions = meta
        .map(|m| {
            m.mentions
                .iter()
                .map(|mn| {
                    let uid = if mn.uid == "-1" {
                        "-1".to_owned()
                    } else {
                        resolve_user(&mn.uid)
                    };
                    json!({
                        "uid": uid,
                        "pos": mn.pos,
                        "len": mn.len,
                        "type": mn.mention_type,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let quote = meta.and_then(|m| m.quote.as_ref()).map(|q| {
        json!({
            "attach": q.attach,
            "ownerId": resolve_user(&q.owner_id),
            "ts": q.ts,
            "fromD": q.from_d,
            "cliMsgId": q.cli_msg_id,
            "globalMsgId": q.global_msg_id,
            "msg": q.msg,
            "cliMsgType": q.cli_msg_type,
            "ttl": q.ttl,
            "partialQuote": q.partial_quote,
        })
    });

    let mut out = json!({
        "msgId": msg.global_id,
        "cliMsgId": msg.client_id.to_string(),
        "msgType": server_type,
        "status": if uid_from == "0" { 2 } else { 3 },
        "notify": "1",
        "mentions": mentions,
        "content": content,
        "ts": msg.timestamp.to_string(),
        "uidFrom": uid_from,
        "uidTo": pc_conv_id,
        "dName": "",
        "ttl": meta.and_then(|m| m.ttl).unwrap_or(0),
        "src": "sync-db",
        "uidSenderDel": uid_sender_del,
    });
    if let Some(q) = quote {
        out["quote"] = q;
    }
    if let Some(rf) = meta.and_then(|m| m.reference.as_ref()) {
        out["reference"] = json!({ "type": rf.ref_type, "data": rf.data });
    }
    if let Some(prop) = meta.and_then(|m| m.property.as_ref()) {
        out["propertyExt"] = json!({
            "size": prop.size,
            "color": prop.color,
            "type": prop.prop_type,
            "subType": prop.sub_type,
            "ext": prop.ext,
        });
    }
    Some(out)
}
