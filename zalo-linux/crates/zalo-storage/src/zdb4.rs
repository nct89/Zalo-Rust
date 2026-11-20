//! ZDB4.0 backup container.
//!
//! After AES decryption the plaintext starts with `ZDB4.0`. The file table
//! begins at byte 14, matching `process_zdb4_v2` in `db-cross-v4`: a big-endian
//! file count, then each name length, name and decompressed size. The bytes
//! after the table are one XZ stream. Decompressed output is split by those sizes.

use crate::crypto::{
    aes256_cbc_decrypt_nopad, aes256_cbc_decrypt_nopad_chunked, decompress_xz_lzma2,
    derive_aes256_key_zalo_v2,
};
use crate::error::StorageError;

const MAGIC: &[u8] = b"ZDB4.0";
const TABLE_AT: usize = 14;
const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";

/// One file taken out of a ZDB4 archive.
///
/// `name` is the table entry. It is not a path on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedFile {
    /// Name stored in the file table.
    pub name: String,
    /// Decompressed contents.
    pub bytes: Vec<u8>,
}

/// Decrypts a ZDB4 v2 backup and returns the files inside it.
///
/// The key comes from [`derive_aes256_key_zalo_v2`]. Ciphertext may start at
/// byte 0, after a 4-byte prefix, or before a 4-byte trailer, which are the
/// three layouts `db-cross-v4` tries. At least one extracted name must end in
/// `.db` and that file must be a SQLite database.
///
/// # Errors
///
/// Returns [`StorageError`] when the blob is too small, the header is not
/// `ZDB4.0`, decompression fails, or the archive has no SQLite database.
pub fn decrypt_zdb4_v2(blob: &[u8], private_key: &str) -> Result<Vec<ExtractedFile>, StorageError> {
    if blob.len() < 16 {
        return Err(StorageError::InputTooSmall);
    }
    let key = derive_aes256_key_zalo_v2(private_key);
    let spans = ciphertext_spans(blob.len());
    if spans.is_empty() {
        return Err(StorageError::NotBlockAligned);
    }
    for (offset, len) in spans {
        let ciphertext = &blob[offset..offset + len];
        let plaintext = match aes256_cbc_decrypt_nopad_chunked(ciphertext, &key) {
            Ok(plaintext) => plaintext,
            Err(_) => match aes256_cbc_decrypt_nopad(ciphertext, &key) {
                Ok(plaintext) => plaintext,
                Err(_) => continue,
            },
        };
        if plaintext.len() >= MAGIC.len() && &plaintext[..MAGIC.len()] == MAGIC {
            return split_archive(&plaintext);
        }
    }
    Err(StorageError::BadMagic)
}

fn ciphertext_spans(len: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    if len % 16 == 0 {
        spans.push((0, len));
    }
    if len > 4 && (len - 4) % 16 == 0 {
        spans.push((4, len - 4));
        spans.push((0, len - 4));
    }
    spans
}

fn split_archive(plaintext: &[u8]) -> Result<Vec<ExtractedFile>, StorageError> {
    let (table, payload_at) = parse_table(plaintext)?;
    let raw = decompress_xz_lzma2(&plaintext[payload_at..])?;
    let mut cursor = 0_usize;
    let mut extracted = Vec::with_capacity(table.len());
    for entry in &table {
        let size = usize::try_from(entry.size).map_err(|_| StorageError::BadTable)?;
        let end = cursor.checked_add(size).ok_or(StorageError::ShortPayload)?;
        if end > raw.len() {
            return Err(StorageError::ShortPayload);
        }
        extracted.push(ExtractedFile {
            name: entry.name.clone(),
            bytes: raw[cursor..end].to_vec(),
        });
        cursor = end;
    }
    validate_sqlite(&extracted)?;
    Ok(extracted)
}

struct TableEntry {
    name: String,
    size: u32,
}

fn parse_table(plaintext: &[u8]) -> Result<(Vec<TableEntry>, usize), StorageError> {
    if plaintext.len() < TABLE_AT + 4 || &plaintext[..MAGIC.len()] != MAGIC {
        return Err(StorageError::BadMagic);
    }
    let mut offset = TABLE_AT;
    let count = read_u32(plaintext, &mut offset)?;
    if count > 4096 {
        return Err(StorageError::BadTable);
    }
    let mut table = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name_len = read_u32(plaintext, &mut offset)?;
        if name_len > 1024 {
            return Err(StorageError::BadTable);
        }
        let name_len = name_len as usize;
        let name_end = offset
            .checked_add(name_len)
            .ok_or(StorageError::TruncatedTable)?;
        if name_end > plaintext.len() {
            return Err(StorageError::TruncatedTable);
        }
        let name = std::str::from_utf8(&plaintext[offset..name_end])
            .map_err(|_| StorageError::BadTable)?
            .to_owned();
        offset = name_end;
        let size = read_u32(plaintext, &mut offset)?;
        table.push(TableEntry { name, size });
    }
    Ok((table, offset))
}

fn read_u32(data: &[u8], offset: &mut usize) -> Result<u32, StorageError> {
    let end = offset.checked_add(4).ok_or(StorageError::TruncatedTable)?;
    let bytes = data.get(*offset..end).ok_or(StorageError::TruncatedTable)?;
    let mut buf = [0_u8; 4];
    buf.copy_from_slice(bytes);
    *offset = end;
    Ok(u32::from_be_bytes(buf))
}

fn validate_sqlite(files: &[ExtractedFile]) -> Result<(), StorageError> {
    let mut saw_db = false;
    let mut saw_sqlite = false;
    for file in files {
        if file.name.len() >= 3 && file.name.as_bytes().ends_with(b".db") {
            saw_db = true;
            if file.bytes.len() >= SQLITE_HEADER.len() && file.bytes.starts_with(SQLITE_HEADER) {
                saw_sqlite = true;
            }
        }
    }
    if !saw_db {
        return Err(StorageError::NoDatabase);
    }
    if !saw_sqlite {
        return Err(StorageError::NotSqlite);
    }
    Ok(())
}
