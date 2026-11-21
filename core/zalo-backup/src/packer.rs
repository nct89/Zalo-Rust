//! Message backup scope builder, noise filter, `proto.Sync2.Message.SyncChunk` encoder,
//! zstd + `libzproto_backup_wasm_bg` block writer (`[u32 LE len][block]` into `<dir>/0`),
//! whole-file SHA-256 checksum, and exact Electron progress math.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::wasm::{BackupCryptoSession, BackupWasmError};

/// `Number.MAX_SAFE_INTEGER` used as default `to` in `buildMessageScopes`.
pub const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
/// Page size in `buildMessageScopes` (500 conversations per page).
pub const SCOPE_PAGE_SIZE: usize = 500;
/// Maximum pages in `buildMessageScopes` (200 pages = 100,000 conversations).
pub const MAX_SCOPE_PAGES: usize = 200;
/// Per-scope message limit (`wa = 1e5` in Electron worker).
pub const MESSAGE_SCOPE_LIMIT: u64 = 100_000;
/// `proto.Sync2.PlatformType.PLATFORM_TYPE_PC = 3`.
pub const PLATFORM_TYPE_PC: u32 = 3;

/// Errors emitted by the backup packer.
#[derive(Debug, thiserror::Error)]
pub enum PackerError {
    #[error("encryption is enabled but required key material is missing: {0}")]
    MissingEncryptionMaterial(&'static str),
    #[error("backup wasm error: {0}")]
    Wasm(#[from] BackupWasmError),
    #[error("zstd compression failed: {0}")]
    Zstd(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Conversation entry fed into `build_message_scopes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupConversationInput {
    /// PC conversation id (e.g. `"123456"` for 1-1, `"g_98765"` or `"g98765"` for group).
    pub pc_conv_id: String,
    pub is_group: bool,
}

/// One backup scope (`{ from: 0, to: MAX_SAFE_INTEGER, partition, limit: 100000 }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupMessageScope {
    pub pc_conv_id: String,
    pub is_group: bool,
    pub from: u64,
    pub to: u64,
    pub limit: u64,
}

/// Builds backup message scopes capped at `SCOPE_PAGE_SIZE * MAX_SCOPE_PAGES` (100,000),
/// matching `buildMessageScopes` in Electron.
#[must_use]
pub fn build_message_scopes(conversations: &[BackupConversationInput]) -> Vec<BackupMessageScope> {
    let cap = SCOPE_PAGE_SIZE * MAX_SCOPE_PAGES;
    conversations
        .iter()
        .take(cap)
        .map(|c| BackupMessageScope {
            pc_conv_id: c.pc_conv_id.clone(),
            is_group: c.is_group,
            from: 0,
            to: JS_MAX_SAFE_INTEGER,
            limit: MESSAGE_SCOPE_LIMIT,
        })
        .collect()
}

/// Outcome of `drop_unnoisable_scopes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredScopes {
    pub scopes: Vec<BackupMessageScope>,
    /// `true` when 0 partitions were resolved (lookup outage) → all scopes kept.
    pub lookup_outage: bool,
    pub dropped_count: usize,
}

/// Exact port of `dropUnnoisableScopes` (`shared-worker`):
/// - If `noise_map` resolved ZERO of the requested scopes → keep ALL scopes and mark `lookup_outage = true`.
/// - If at least one scope was resolved → drop every scope absent from `noise_map` (`lookup_outage = false`).
#[must_use]
pub fn drop_unnoisable_scopes(
    scopes: Vec<BackupMessageScope>,
    noise_map: &HashMap<String, String>,
) -> FilteredScopes {
    let any_resolved = scopes
        .iter()
        .any(|s| noise_map.get(&s.pc_conv_id).is_some_and(|v| !v.is_empty()));
    if !any_resolved {
        return FilteredScopes {
            scopes,
            lookup_outage: true,
            dropped_count: 0,
        };
    }
    let total = scopes.len();
    let kept: Vec<BackupMessageScope> = scopes
        .into_iter()
        .filter(|s| noise_map.get(&s.pc_conv_id).is_some_and(|v| !v.is_empty()))
        .collect();
    let dropped_count = total.saturating_sub(kept.len());
    FilteredScopes {
        scopes: kept,
        lookup_outage: false,
        dropped_count,
    }
}

/// Builds the cross-platform partition string (`"group/<gid>"` or `"oneone/<gid>"`).
#[must_use]
pub fn format_wire_partition(global_noised_id: &str, is_group: bool) -> String {
    if is_group {
        format!("group/{global_noised_id}")
    } else {
        format!("oneone/{global_noised_id}")
    }
}

/// Exact `Ai` map (`shared-worker` line 5341): PC numeric msgType → cross-platform `xi` msgType.
#[must_use]
pub fn map_pc_numeric_msg_type(pc_type: i32) -> i32 {
    match pc_type {
        1 => 0,       // Text
        2 | 11 => 3,  // Photo
        3 => 6,       // Voice
        4 | 13 => 10, // Sticker
        5 => 2,       // Doodle
        6 => 12,      // Contact
        7 => 23,      // Gift
        17 => 18,     // Location
        18 => 19,     // Video
        19 => 22,     // File
        20 => 36,     // Undo
        21 => 15,     // OA
        25 => 21,     // ECard
        26 => 26,     // Poll
        52 => 24,     // Webcontent
        53 => 53,     // WebcontentV2
        -1909 => 20,  // ChatInfo
        -27 => 33,    // ChatDelete
        other => other,
    }
}

/// Maps Zalo string `msg_type` (from `LocalStore` / server events) → cross-platform `xi` msgType.
#[must_use]
pub fn map_string_msg_type(msg_type: &str) -> i32 {
    match msg_type {
        "webchat" | "text" | "" => 0,
        "chat.doodle" => 2,
        "chat.photo" => 3,
        "chat.voice" => 6,
        "chat.sticker" => 10,
        "chat.recommended" => 12,
        "chat.list.action" => 15,
        "chat.location.new" => 18,
        "chat.video.msg" => 19,
        "chat.info" | "event" => 20,
        "chat.ecard" => 21,
        "share.file" => 22,
        "chat.gif" => 23,
        "chat.webcontent" => 24,
        "chat.video.live.msg" => 25,
        "chat.poll" => 26,
        "chat.delete" => 33,
        "chat.undo" => 36,
        "chat.webcontent.v2" => 53,
        _ => 0,
    }
}

/// Exact `Li` map (`shared-worker` line 5343): PC msgStatus → proto `MsgStatus` (fallback `0`).
#[must_use]
pub fn map_pc_msg_status(pc_status: i32) -> i32 {
    match pc_status {
        -2 | -22 => 1,
        10 | -20 | -21 | -23 | -24 | -25 => 2,
        1 => 3,
        2 => 4,
        3 | 4 => 5,
        _ => 0,
    }
}

/// Local message record supplied by `LocalStore` to the backup packer.
#[derive(Debug, Clone, Default)]
pub struct LocalBackupMessage {
    /// Sender PC uid (`"0"` or `"me"` when sent by the current user).
    pub sender_pc_id: String,
    pub client_id: u64,
    pub pc_status: i32,
    /// Optional numeric PC msgType (takes precedence over `msg_type_str` if `Some`).
    pub pc_msg_type: Option<i32>,
    /// String msgType (e.g. `"webchat"`, `"chat.photo"`).
    pub msg_type_str: String,
    pub timestamp_ms: u64,
    pub global_id: String,
    pub content: String,
    pub attach: Option<BackupAttach>,
    pub mentions: Vec<BackupMention>,
    pub quote: Option<BackupQuote>,
    pub reference: Option<BackupReference>,
    pub properties: Option<BackupProperties>,
    pub ttl: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupAttach {
    pub attach_type: i32,
    pub ext_info: String,
    pub action: String,
    pub params: String,
    pub title: String,
    pub href: String,
    pub thumb: String,
    pub description: String,
    pub cat_id: i32,
    pub id: String,
    pub child_number: i32,
    pub remains: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupMention {
    pub uid: String,
    pub pos: i32,
    pub len: i32,
    pub mention_type: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupQuote {
    pub attach: String,
    pub owner_id: String,
    pub ts: u64,
    pub from_d: i32,
    pub cli_msg_id: u64,
    pub global_msg_id: u64,
    pub msg: String,
    pub cli_msg_type: i32,
    pub ttl: u64,
    pub partial_quote: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupReference {
    pub ref_type: i32,
    pub data: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupProperties {
    pub size: i32,
    pub color: i32,
    pub prop_type: i32,
    pub sub_type: i32,
    pub ext: String,
}

/// Transformed cross-platform message ready for protobuf serialization.
#[derive(Debug, Clone, Default)]
pub struct WireBackupMessage {
    pub sender_id: String,
    pub client_id: u64,
    pub msg_status: i32,
    pub msg_type: i32,
    pub timestamp: u64,
    pub global_id: String,
    pub content: String,
    pub attach: Option<BackupAttach>,
    pub mentions: Vec<BackupMention>,
    pub quote: Option<BackupQuote>,
    pub reference: Option<BackupReference>,
    pub properties: Option<BackupProperties>,
    pub ttl: u64,
}

/// Transforms a [`LocalBackupMessage`] into a [`WireBackupMessage`], resolving `senderId == "0"`
/// to `my_global_noised_id` and other sender/mention/quote IDs via `noise_map`.
#[must_use]
pub fn transform_message(
    msg: &LocalBackupMessage,
    my_pc_uid: &str,
    my_global_noised_id: Option<&str>,
    noise_map: &HashMap<String, String>,
) -> WireBackupMessage {
    let resolve_uid = |uid: &str| -> String {
        if uid == "0" || uid == "me" || (!my_pc_uid.is_empty() && uid == my_pc_uid) {
            if let Some(mine) = my_global_noised_id.filter(|s| !s.is_empty()) {
                return mine.to_owned();
            }
            return "0".to_owned();
        }
        noise_map
            .get(uid)
            .cloned()
            .unwrap_or_else(|| uid.to_owned())
    };

    let msg_type = msg
        .pc_msg_type
        .map(map_pc_numeric_msg_type)
        .unwrap_or_else(|| map_string_msg_type(&msg.msg_type_str));
    let msg_status = map_pc_msg_status(msg.pc_status);

    let mentions = msg
        .mentions
        .iter()
        .map(|m| BackupMention {
            uid: if m.uid == "-1" {
                "-1".to_owned()
            } else {
                resolve_uid(&m.uid)
            },
            pos: m.pos,
            len: m.len,
            mention_type: m.mention_type,
        })
        .collect();

    let quote = msg.quote.as_ref().map(|q| {
        let mut out = q.clone();
        if !out.owner_id.is_empty() {
            out.owner_id = resolve_uid(&out.owner_id);
        }
        out
    });

    let global_id = normalize_global_id(&msg.global_id);

    WireBackupMessage {
        sender_id: resolve_uid(&msg.sender_pc_id),
        client_id: msg.client_id,
        msg_status,
        msg_type,
        timestamp: msg.timestamp_ms,
        global_id,
        content: msg.content.clone(),
        attach: msg.attach.clone(),
        mentions,
        quote,
        reference: msg.reference.clone(),
        properties: msg.properties.clone(),
        ttl: msg.ttl,
    }
}

fn normalize_global_id(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "0" {
        return "0".to_owned();
    }
    if trimmed.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        trimmed.to_owned()
    } else {
        "0".to_owned()
    }
}

/// Encodes one partition of messages into a `proto.Sync2.Message.SyncChunk` wire buffer
/// with `platform = PLATFORM_TYPE_PC (3)`.
/// If `partition_id` is `None`, emits an empty `partitionsList` (matching `xn.doEncode`).
#[must_use]
pub fn encode_message_sync_chunk(
    partition_id: Option<&str>,
    messages: &[WireBackupMessage],
) -> Vec<u8> {
    let mut out = Vec::new();
    // field 1: platform = 3 (PC)
    write_varint_field(&mut out, 1, PLATFORM_TYPE_PC as u64);
    if let Some(pid) = partition_id {
        let mut part_buf = Vec::new();
        if !pid.is_empty() {
            write_string_field(&mut part_buf, 1, pid);
        }
        for m in messages {
            let msg_bytes = encode_wire_message(m);
            write_bytes_field(&mut part_buf, 2, &msg_bytes);
        }
        write_bytes_field(&mut out, 2, &part_buf);
    }
    out
}

fn encode_wire_message(m: &WireBackupMessage) -> Vec<u8> {
    let mut b = Vec::new();
    if !m.sender_id.is_empty() {
        write_string_field(&mut b, 1, &m.sender_id);
    }
    if m.client_id != 0 {
        write_varint_field(&mut b, 2, m.client_id);
    }
    if m.msg_status != 0 {
        write_varint_field(&mut b, 3, m.msg_status as i64 as u64);
    }
    if m.msg_type != 0 {
        write_varint_field(&mut b, 4, m.msg_type as i64 as u64);
    }
    if m.timestamp != 0 {
        write_varint_field(&mut b, 5, m.timestamp);
    }
    if !m.global_id.is_empty() && m.global_id != "0" {
        write_string_field(&mut b, 6, &m.global_id);
    }
    if !m.content.is_empty() {
        write_string_field(&mut b, 7, &m.content);
    }
    let meta = encode_metadata(m);
    if !meta.is_empty() {
        write_bytes_field(&mut b, 8, &meta);
    }
    b
}

fn encode_metadata(m: &WireBackupMessage) -> Vec<u8> {
    let mut b = Vec::new();
    if let Some(a) = &m.attach {
        let mut ab = Vec::new();
        if a.attach_type != 0 {
            write_varint_field(&mut ab, 1, a.attach_type as i64 as u64);
        }
        if !a.ext_info.is_empty() {
            write_string_field(&mut ab, 2, &a.ext_info);
        }
        if !a.action.is_empty() {
            write_string_field(&mut ab, 3, &a.action);
        }
        if !a.params.is_empty() {
            write_string_field(&mut ab, 4, &a.params);
        }
        if !a.title.is_empty() {
            write_string_field(&mut ab, 5, &a.title);
        }
        if !a.href.is_empty() {
            write_string_field(&mut ab, 6, &a.href);
        }
        if !a.thumb.is_empty() {
            write_string_field(&mut ab, 7, &a.thumb);
        }
        if !a.description.is_empty() {
            write_string_field(&mut ab, 8, &a.description);
        }
        if a.cat_id != 0 {
            write_varint_field(&mut ab, 9, a.cat_id as i64 as u64);
        }
        if !a.id.is_empty() {
            write_string_field(&mut ab, 10, &a.id);
        }
        if a.child_number != 0 {
            write_varint_field(&mut ab, 11, a.child_number as i64 as u64);
        }
        if !a.remains.is_empty() {
            write_string_field(&mut ab, 12, &a.remains);
        }
        if !ab.is_empty() {
            write_bytes_field(&mut b, 1, &ab);
        }
    }
    for men in &m.mentions {
        let mut mb = Vec::new();
        if !men.uid.is_empty() {
            write_string_field(&mut mb, 1, &men.uid);
        }
        if men.pos != 0 {
            write_varint_field(&mut mb, 2, men.pos as i64 as u64);
        }
        if men.len != 0 {
            write_varint_field(&mut mb, 3, men.len as i64 as u64);
        }
        if men.mention_type != 0 {
            write_varint_field(&mut mb, 4, men.mention_type as i64 as u64);
        }
        write_bytes_field(&mut b, 2, &mb);
    }
    if let Some(q) = &m.quote {
        let mut qb = Vec::new();
        if !q.attach.is_empty() {
            write_string_field(&mut qb, 1, &q.attach);
        }
        if !q.owner_id.is_empty() {
            write_string_field(&mut qb, 2, &q.owner_id);
        }
        if q.ts != 0 {
            write_varint_field(&mut qb, 3, q.ts);
        }
        if q.from_d != 0 {
            write_varint_field(&mut qb, 4, q.from_d as i64 as u64);
        }
        if q.cli_msg_id != 0 {
            write_varint_field(&mut qb, 5, q.cli_msg_id);
        }
        if q.global_msg_id != 0 {
            write_varint_field(&mut qb, 6, q.global_msg_id);
        }
        if !q.msg.is_empty() {
            write_string_field(&mut qb, 7, &q.msg);
        }
        if q.cli_msg_type != 0 {
            write_varint_field(&mut qb, 8, q.cli_msg_type as i64 as u64);
        }
        if q.ttl != 0 {
            write_varint_field(&mut qb, 9, q.ttl);
        }
        if !q.partial_quote.is_empty() {
            write_string_field(&mut qb, 10, &q.partial_quote);
        }
        if !qb.is_empty() {
            write_bytes_field(&mut b, 3, &qb);
        }
    }
    if let Some(r) = &m.reference {
        let mut rb = Vec::new();
        if r.ref_type != 0 {
            write_varint_field(&mut rb, 1, r.ref_type as i64 as u64);
        }
        if !r.data.is_empty() {
            write_string_field(&mut rb, 2, &r.data);
        }
        if !rb.is_empty() {
            write_bytes_field(&mut b, 4, &rb);
        }
    }
    if let Some(p) = &m.properties {
        let mut pb = Vec::new();
        if p.size != 0 {
            write_varint_field(&mut pb, 1, p.size as i64 as u64);
        }
        if p.color != 0 {
            write_varint_field(&mut pb, 2, p.color as i64 as u64);
        }
        if p.prop_type != 0 {
            write_varint_field(&mut pb, 3, p.prop_type as i64 as u64);
        }
        if p.sub_type != 0 {
            write_varint_field(&mut pb, 4, p.sub_type as i64 as u64);
        }
        if !p.ext.is_empty() {
            write_string_field(&mut pb, 5, &p.ext);
        }
        if !pb.is_empty() {
            write_bytes_field(&mut b, 5, &pb);
        }
    }
    if m.ttl != 0 {
        write_varint_field(&mut b, 6, m.ttl);
    }
    b
}

/// Returns the on-disk backup block file path `<backup_dir>/0` (`getSync2BackupFilePath(dir, "0")`).
#[must_use]
pub fn backup_block_file_path(backup_dir: &Path) -> PathBuf {
    backup_dir.join("0")
}

/// Appends one framed block `[u32 LE byte_length][payload]` to `<backup_dir>/0`,
/// matching `writeSync2BackupData` (`yt` in `preload-shared-worker.js:45160-45166`).
pub fn append_framed_block(path: &Path, block: &[u8]) -> Result<(), PackerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let len = u32::try_from(block.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "block exceeds u32"))?;
    file.write_all(&len.to_le_bytes())?;
    file.write_all(block)?;
    Ok(())
}

/// Reads all `[u32 LE len][payload]` blocks from a backup file buffer (for verification/tests).
pub fn read_framed_blocks(data: &[u8]) -> Result<Vec<Vec<u8>>, PackerError> {
    let mut blocks = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        if pos + 4 > data.len() {
            return Err(PackerError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated u32 LE block header",
            )));
        }
        let len = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
            as usize;
        pos += 4;
        if pos + len > data.len() {
            return Err(PackerError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated block payload",
            )));
        }
        blocks.push(data[pos..pos + len].to_vec());
        pos += len;
    }
    Ok(blocks)
}

/// Computes lowercase hex SHA-256 checksum and byte length of the whole backup file on disk
/// (`readBackupFileWithChecksum` / `Gb` in Electron).
pub fn compute_file_sha256_and_size(path: &Path) -> Result<(String, u64), PackerError> {
    let bytes = fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hex::encode(hasher.finalize());
    Ok((digest, bytes.len() as u64))
}

/// Worker packing percentage formula (`shared-worker`):
/// mid-run: `y > 0 ? min(99, floor(done / total * 100)) : 0`.
#[must_use]
pub fn compute_worker_progress_percent(done_scopes: usize, total_scopes: usize) -> u32 {
    if total_scopes == 0 {
        return 0;
    }
    let pct = (done_scopes.saturating_mul(100)) / total_scopes;
    (pct as u32).min(99)
}

/// UI packing percentage: `floor(worker_percent * weight / 100)`.
#[must_use]
pub fn compute_ui_pack_percent(worker_percent: u32, weight: u32) -> u32 {
    let w = weight.min(100);
    worker_percent.min(100).saturating_mul(w) / 100
}

/// UI upload percentage: `floor(weight + chunk_percent * (100 - weight) / 100)`
/// where `chunk_percent = floor(uploaded_chunks / total_chunks * 100)`.
#[must_use]
pub fn compute_ui_upload_percent(
    uploaded_chunks: u32,
    total_chunks: u32,
    weight: u32,
) -> u32 {
    let w = weight.min(100);
    if total_chunks == 0 {
        return w;
    }
    let chunk_pct = uploaded_chunks
        .min(total_chunks)
        .saturating_mul(100)
        / total_chunks;
    let delta = chunk_pct.saturating_mul(100 - w) / 100;
    (w + delta).min(100)
}

/// Result produced by [`pack_backup_file`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackResult {
    pub file_path: PathBuf,
    pub file_size: u64,
    pub original_size: u64,
    pub item_count: u64,
    pub scope_count: u64,
    pub checksum: String,
    pub lookup_outage: bool,
    pub dropped_scopes: usize,
}

/// Parameters for [`pack_backup_file`].
pub struct PackParams<'a> {
    pub backup_dir: &'a Path,
    pub bkid: &'a str,
    pub server_nonce_b64: &'a str,
    pub client_salt_b64: &'a str,
    pub encryption_enabled: bool,
    pub my_pc_uid: &'a str,
    pub my_global_noised_id: Option<&'a str>,
    pub conversations: &'a [BackupConversationInput],
    pub noise_map: &'a HashMap<String, String>,
}

/// Runs the complete Electron worker packing pipeline into `<backup_dir>/0`:
/// 1. Validates encryption material upfront — when `encryption_enabled == true`, NEVER falls back
///    to raw plaintext if `bkid`, `server_nonce_b64`, or `client_salt_b64` is empty!
/// 2. Builds scopes (max 500×200) and applies `drop_unnoisable_scopes`.
/// 3. For each scope, reads messages via `fetch_messages(scope)`, transforms IDs/types/statuses,
///    encodes `proto.Sync2.Message.SyncChunk` (`PLATFORM_TYPE_PC`), zstd-compresses, encrypts via
///    `libzproto_backup_wasm_bg` (if `encryption_enabled`), and appends `[u32 LE][block]` to `<dir>/0`.
/// 4. Reports worker progress (`min(99, floor(done/total*100))`, then `100` at the end).
/// 5. Computes whole-file lowercase hex SHA-256 and file size.
pub fn pack_backup_file<F, P>(
    params: &PackParams<'_>,
    wasm_path: Option<&Path>,
    mut fetch_messages: F,
    mut on_worker_progress: P,
) -> Result<PackResult, PackerError>
where
    F: FnMut(&BackupMessageScope) -> Vec<LocalBackupMessage>,
    P: FnMut(u32),
{
    let mut crypto_session: Option<BackupCryptoSession> = if params.encryption_enabled {
        if params.bkid.is_empty() {
            return Err(PackerError::MissingEncryptionMaterial("bkid"));
        }
        if params.server_nonce_b64.is_empty() {
            return Err(PackerError::MissingEncryptionMaterial(
                "server_nonce (CMD 631 must run before engine)",
            ));
        }
        if params.client_salt_b64.is_empty() {
            return Err(PackerError::MissingEncryptionMaterial("client_salt"));
        }
        let path = wasm_path.ok_or(PackerError::MissingEncryptionMaterial(
            "libzproto_backup_wasm_bg path",
        ))?;
        Some(BackupCryptoSession::start_from_file(
            path,
            params.server_nonce_b64,
            params.client_salt_b64,
            true,
        )?)
    } else {
        None
    };

    let raw_scopes = build_message_scopes(params.conversations);
    let filtered = drop_unnoisable_scopes(raw_scopes, params.noise_map);
    let total_scopes = filtered.scopes.len();

    let file_path = backup_block_file_path(params.backup_dir);
    if file_path.exists() {
        let _ = fs::remove_file(&file_path);
    }
    fs::create_dir_all(params.backup_dir)?;

    if total_scopes == 0 {
        // Worker reports scope_count = 0 so pipeline handles UserSystem + reset (NOT empty popup!).
        return Ok(PackResult {
            file_path,
            file_size: 0,
            original_size: 0,
            item_count: 0,
            scope_count: 0,
            checksum: String::new(),
            lookup_outage: filtered.lookup_outage,
            dropped_scopes: filtered.dropped_count,
        });
    }

    let mut total_items: u64 = 0;
    let mut total_original_size: u64 = 0;
    let mut done_scopes: usize = 0;

    for scope in &filtered.scopes {
        let raw_msgs = fetch_messages(scope);
        let wire_partition = params
            .noise_map
            .get(&scope.pc_conv_id)
            .filter(|s| !s.is_empty())
            .map(|gid| format_wire_partition(gid, scope.is_group));

        let wire_msgs: Vec<WireBackupMessage> = raw_msgs
            .iter()
            .take(scope.limit as usize)
            .map(|m| {
                transform_message(
                    m,
                    params.my_pc_uid,
                    params.my_global_noised_id,
                    params.noise_map,
                )
            })
            .collect();

        let (encoded, block_items) = if let Some(ref pid) = wire_partition {
            let bytes = encode_message_sync_chunk(Some(pid), &wire_msgs);
            (bytes, wire_msgs.len() as u64)
        } else {
            // `xn.doEncode` emits an empty chunk with `itemCount: 0` when partition noise fails.
            let bytes = encode_message_sync_chunk(None, &[]);
            (bytes, 0)
        };

        total_original_size = total_original_size.saturating_add(encoded.len() as u64);
        total_items = total_items.saturating_add(block_items);

        let compressed = zstd::bulk::compress(&encoded, 3)
            .map_err(|e| PackerError::Zstd(e.to_string()))?;

        let block_payload = if let Some(ref mut sess) = crypto_session {
            sess.encrypt_block(&compressed)?
        } else {
            compressed
        };

        append_framed_block(&file_path, &block_payload)?;

        done_scopes += 1;
        on_worker_progress(compute_worker_progress_percent(done_scopes, total_scopes));
    }

    on_worker_progress(100);
    let (checksum, file_size) = compute_file_sha256_and_size(&file_path)?;

    Ok(PackResult {
        file_path,
        file_size,
        original_size: total_original_size,
        item_count: total_items,
        scope_count: total_scopes as u64,
        checksum,
        lookup_outage: filtered.lookup_outage,
        dropped_scopes: filtered.dropped_count,
    })
}

// ---------------------------------------------------------------------------
// Minimal wire-exact proto3 encoding helpers (zero external deps, zero unsafe)
// ---------------------------------------------------------------------------

fn encode_varint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if v == 0 {
            break;
        }
    }
}

fn write_tag(buf: &mut Vec<u8>, field_num: u32, wire_type: u32) {
    encode_varint(buf, ((field_num as u64) << 3) | (wire_type as u64));
}

fn write_varint_field(buf: &mut Vec<u8>, field_num: u32, val: u64) {
    write_tag(buf, field_num, 0);
    encode_varint(buf, val);
}

fn write_bytes_field(buf: &mut Vec<u8>, field_num: u32, bytes: &[u8]) {
    write_tag(buf, field_num, 2);
    encode_varint(buf, bytes.len() as u64);
    buf.extend_from_slice(bytes);
}

fn write_string_field(buf: &mut Vec<u8>, field_num: u32, s: &str) {
    write_bytes_field(buf, field_num, s.as_bytes());
}
