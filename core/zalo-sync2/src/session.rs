//! State of one sync session, from `cmd 590` to the last batch.

use std::collections::BTreeSet;

use crate::batch;
use crate::keys::{KeyPair, SyncKeys};
use crate::proto::{ConversationChunk, Decode, MessageChunk};
use crate::request::{
    new_sync_id, BatchInfo, BatchType, ControlEvent, Database, MobileStatus, Query, SyncRequest,
    DEFAULT_BATCH_SIZE,
};
use crate::wasm::ZprotoWasm;
use crate::Sync2Error;

/// What a control event meant for this session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// The event belongs to another session or feature.
    Ignored,
    /// The phone reported a status.
    Status(MobileStatus),
    /// A batch is ready: download `msg_url` and pass it to [`SyncSession::decode`].
    Download(BatchInfo),
    /// The phone reported an error for a batch.
    Failed(BatchInfo),
    /// A batch index that was already announced.
    Duplicate,
}

/// Decoded content of one batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    /// Frames of the `message` database.
    Messages(Vec<MessageChunk>),
    /// Frames of the `conversation` database.
    Conversations(Vec<ConversationChunk>),
}

/// One sync session.
#[derive(Debug)]
pub struct SyncSession {
    sync_id: String,
    keys: SyncKeys,
    database: Database,
    session_record: Option<Vec<u8>>,
    announced: BTreeSet<u32>,
    total: Option<u32>,
    last_status: Option<MobileStatus>,
}

impl SyncSession {
    /// Starts a session with the app's identity key and a new ephemeral key.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error`] when key generation fails.
    pub fn new(
        wasm: &mut ZprotoWasm,
        identity: KeyPair,
        database: Database,
    ) -> Result<Self, Sync2Error> {
        Ok(Self {
            sync_id: new_sync_id(),
            keys: SyncKeys {
                identity,
                ephemeral: KeyPair::generate(wasm)?,
            },
            database,
            session_record: None,
            announced: BTreeSet::new(),
            total: None,
            last_status: None,
        })
    }

    /// Session id (`syncId`).
    pub fn sync_id(&self) -> &str {
        &self.sync_id
    }

    /// Keys of this session.
    pub fn keys(&self) -> &SyncKeys {
        &self.keys
    }

    /// Last status reported by the phone.
    pub fn last_status(&self) -> Option<MobileStatus> {
        self.last_status
    }

    /// The `cmd 590` request for `queries`. Pass a `temp_key` only for the first
    /// session after a `transfer_after_login` event; otherwise the phone asks
    /// its user to confirm.
    pub fn request(
        &self,
        queries: Vec<Query>,
        temp_key: &str,
        device_name: &str,
        ussidx: u32,
    ) -> SyncRequest {
        SyncRequest {
            sync_id: self.sync_id.clone(),
            ek: self.keys.ephemeral.public_base64(),
            ik: self.keys.identity.public_base64(),
            temp_key: temp_key.to_owned(),
            device_name: device_name.to_owned(),
            database: self.database,
            priority: 1,
            queries,
            batch_size: DEFAULT_BATCH_SIZE,
            ussidx,
            on_demand: false,
        }
    }

    /// Handles a control event from `cmd 601`.
    pub fn on_control(&mut self, event: &ControlEvent) -> Update {
        match event {
            ControlEvent::Status { sync_id, status } if *sync_id == self.sync_id => {
                self.last_status = Some(*status);
                Update::Status(*status)
            }
            ControlEvent::Batch(info) if info.sync_id == self.sync_id => {
                if !self.announced.insert(info.idx) {
                    return Update::Duplicate;
                }
                if info.is_last {
                    self.total = Some(info.idx + 1);
                }
                Update::Download(info.clone())
            }
            ControlEvent::BatchError(info) if info.sync_id == self.sync_id => {
                Update::Failed(info.clone())
            }
            _ => Update::Ignored,
        }
    }

    /// `true` once every batch up to the last one has been announced.
    pub fn is_complete(&self) -> bool {
        self.total
            .is_some_and(|total| self.announced.len() as u32 == total)
    }

    /// Decrypts, unpacks and decodes a downloaded batch file.
    ///
    /// Decode the `MetadataEmbedded` batch first: it carries the session record
    /// that `Message` batches need. A `Message` batch that comes first returns
    /// [`Sync2Error::NoSessionRecord`]; keep it and retry later.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error`] for a bad file, a decryption failure, bad zstd
    /// data or a bad protobuf.
    pub fn decode(
        &mut self,
        wasm: &mut ZprotoWasm,
        info: &BatchInfo,
        file: &[u8],
    ) -> Result<Decoded, Sync2Error> {
        let mut plain = Vec::new();
        for frame in batch::frames(file)? {
            let decrypted = match info.batch_type {
                BatchType::MetadataEmbedded => {
                    let out = wasm.decrypt_metadata(
                        frame,
                        &self.keys.identity.public,
                        &self.keys.identity.private,
                        &self.keys.ephemeral.private,
                    )?;
                    self.session_record = Some(out.session_record);
                    out.plaintext
                }
                BatchType::Message => {
                    let record = self
                        .session_record
                        .as_deref()
                        .ok_or(Sync2Error::NoSessionRecord)?;
                    wasm.decrypt_message(frame, record)?
                }
                BatchType::Other(kind) => {
                    return Err(Sync2Error::Format(format!("unknown batch type {kind}")))
                }
            };
            plain.push(batch::decompress(&decrypted)?);
        }
        Ok(match self.database {
            Database::Message => Decoded::Messages(
                plain
                    .iter()
                    .map(|p| MessageChunk::decode(p))
                    .collect::<Result<_, _>>()?,
            ),
            Database::Conversation => Decoded::Conversations(
                plain
                    .iter()
                    .map(|p| ConversationChunk::decode(p))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }
}
