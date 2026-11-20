//! Failures from backup decryption and the local SQLite store.

use std::io;
use thiserror::Error;

/// A backup blob or the local database could not be processed.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The blob is shorter than one AES block.
    #[error("input is too small")]
    InputTooSmall,
    /// AES-CBC without padding needs a multiple of 16 bytes.
    #[error("ciphertext length is not AES block aligned")]
    NotBlockAligned,
    /// The cipher rejected the key or IV length.
    #[error("AES initialization failed")]
    AesInit,
    /// OpenSSL-compatible CBC produced no plaintext.
    #[error("AES decryption failed")]
    Decrypt,
    /// No span decrypted to a `ZDB4.0` header.
    #[error("invalid ZDB4.0 header")]
    BadMagic,
    /// The file table ends before every name and size is present.
    #[error("truncated ZDB4 file table")]
    TruncatedTable,
    /// A length in the file table does not fit the remaining bytes.
    #[error("ZDB4 file table is inconsistent")]
    BadTable,
    /// The XZ stream after the file table did not decode.
    #[error("XZ/LZMA2 decompression failed: {0}")]
    Lzma(String),
    /// Decompressed bytes are shorter than the sizes in the file table.
    #[error("decompressed payload is shorter than the file table")]
    ShortPayload,
    /// A `.db` entry does not start with the SQLite header, and no other `.db` does.
    #[error("extracted database is not SQLite")]
    NotSqlite,
    /// The archive has no file whose name ends in `.db`.
    #[error("archive contains no .db file")]
    NoDatabase,
    /// Creating a parent directory or reading a database file failed.
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
    /// SQLite rejected a statement or a constraint.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A thread kind string is neither `user` nor `group`.
    #[error("invalid thread kind")]
    BadKind,
}
