//! Read side of Zalo PC's "sync messages from phone" flow (sync v2).
//!
//! The server keeps only about two days of missed messages. Zalo PC gets older
//! history from the phone: it sends `cmd 590` with fresh keys, the phone asks
//! its user to confirm, uploads encrypted batches to the file server, and the
//! server announces each batch with a `transfer_sync2` control event
//! (`cmd 601`). The PC downloads the batch, splits it into frames, decrypts each
//! frame, unpacks it with zstd and decodes the `Sync2.*` protobuf.
//!
//! This crate has no I/O. It builds the request bodies, reads the control
//! events, and turns downloaded batch files into [`message::SyncedMessage`]s.
//! It implements the phone-to-desktop history sync protocol.
//!
//! The crypto lives in Zalo's own wasm module; [`wasm::ZprotoWasm`] runs it with
//! a pure-Rust interpreter.

pub mod batch;
pub mod keys;
pub mod message;
pub mod proto;
pub mod request;
pub mod session;
pub mod wasm;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum Sync2Error {
    /// Reading the wasm file failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The wasm module failed to load or trapped.
    #[error("wasm: {0}")]
    Wasm(String),
    /// The wasm crypto returned an error object.
    #[error("crypto {code}: {message}")]
    Crypto {
        /// `code` field, for example `DecryptionError`.
        code: String,
        /// `message` field.
        message: String,
    },
    /// A batch file or protobuf is malformed.
    #[error("format: {0}")]
    Format(String),
    /// zstd decompression failed.
    #[error("zstd: {0}")]
    Zstd(String),
    /// A `Message` batch arrived before the session's metadata batch.
    #[error("session record not received yet")]
    NoSessionRecord,
}

impl From<wasmi::Error> for Sync2Error {
    fn from(err: wasmi::Error) -> Self {
        Sync2Error::Wasm(err.to_string())
    }
}

impl From<wasmi::errors::LinkerError> for Sync2Error {
    fn from(err: wasmi::errors::LinkerError) -> Self {
        Sync2Error::Wasm(err.to_string())
    }
}

#[cfg(test)]
mod tests;
