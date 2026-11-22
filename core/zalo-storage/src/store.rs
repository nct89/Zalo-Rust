//! Local SQLite index for users, conversation threads and messages.
//!
//! This schema belongs to the Rust client. Decrypting a ZDB4 backup yields
//! SQLite files; those official database layouts are not recreated here.
//! Rows written through [`LocalStore`] are the conversations this client keeps.

use std::fs;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::StorageError;

const SCHEMA: &str = "
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS users (
    user_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    zalo_name TEXT NOT NULL DEFAULT '',
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS threads (
    thread_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('user', 'group')),
    title TEXT NOT NULL,
    peer_user_id TEXT,
    unread INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (peer_user_id) REFERENCES users(user_id)
);
CREATE TABLE IF NOT EXISTS messages (
    message_id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL,
    sender_id TEXT,
    cli_msg_id INTEGER,
    global_msg_id INTEGER,
    body TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    FOREIGN KEY (thread_id) REFERENCES threads(thread_id),
    FOREIGN KEY (sender_id) REFERENCES users(user_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS messages_thread_cli
    ON messages(thread_id, cli_msg_id)
    WHERE cli_msg_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS messages_thread_sent
    ON messages(thread_id, sent_at, message_id);
CREATE INDEX IF NOT EXISTS messages_thread_global
    ON messages(thread_id, global_msg_id)
    WHERE global_msg_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS attachments (
    message_id INTEGER PRIMARY KEY,
    data TEXT NOT NULL,
    FOREIGN KEY (message_id) REFERENCES messages(message_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id TEXT NOT NULL,
    is_group INTEGER NOT NULL,
    body TEXT NOT NULL,
    urgency INTEGER NOT NULL DEFAULT 0,
    client_id INTEGER NOT NULL,
    quote_json TEXT,
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0
);
";

/// Whether a thread is a direct chat or a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadKind {
    /// A conversation with one user.
    User,
    /// A conversation with a group.
    Group,
}

impl ThreadKind {
    /// Storage value used in the `threads.kind` column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Group => "group",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "user" => Ok(Self::User),
            "group" => Ok(Self::Group),
            _ => Err(StorageError::BadKind),
        }
    }
}

/// One person stored in `users`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    /// Stable id used by threads and messages.
    pub user_id: String,
    /// Name shown in the conversation list.
    pub display_name: String,
    /// Zalo name, when the caller has one.
    pub zalo_name: String,
    /// Unix milliseconds of the last update.
    pub updated_at: i64,
}

/// One conversation stored in `threads`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadRecord {
    /// Stable conversation id.
    pub thread_id: String,
    /// Direct chat or group.
    pub kind: ThreadKind,
    /// Title shown for the conversation.
    pub title: String,
    /// Other participant when `kind` is [`ThreadKind::User`].
    pub peer_user_id: Option<String>,
    /// Unread count kept by this client.
    pub unread: i64,
    /// Unix milliseconds of the last update.
    pub updated_at: i64,
}

/// One message stored in `messages`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageRecord {
    /// Row id assigned by SQLite. Zero on a record that has not been inserted.
    pub message_id: i64,
    /// Conversation that owns the message.
    pub thread_id: String,
    /// Sender, when the message has one.
    pub sender_id: Option<String>,
    /// Client message id, when the caller has one.
    pub cli_msg_id: Option<i64>,
    /// Server message id, when the caller has one.
    pub global_msg_id: Option<i64>,
    /// Message text kept by this client.
    pub body: String,
    /// Unix milliseconds when the message was sent.
    pub sent_at: i64,
}

/// One pending outgoing text message queued while offline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxRecord {
    /// SQLite row id (zero before insertion).
    pub id: i64,
    /// Destination thread id (`g_...` for groups or peer uid).
    pub thread_id: String,
    /// True for group threads.
    pub is_group: bool,
    /// Plain-text body.
    pub body: String,
    /// Urgency level (`0` normal, `1` important, `2` urgent).
    pub urgency: u8,
    /// Client-generated message id (`cliMsgId`) preserved across retries.
    pub client_id: u64,
    /// Optional serialized JSON quote target.
    pub quote_json: Option<String>,
    /// Unix milliseconds when queued.
    pub created_at: i64,
    /// Number of failed delivery attempts so far.
    pub attempts: i64,
}

/// Open SQLite file, or an in-memory database, with the local schema applied.
pub struct LocalStore {
    conn: Connection,
}

impl LocalStore {
    /// Opens `path`, creates missing parent directories, and applies the schema.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the directory or the database cannot be opened.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Merges a WAL into the main database so a later copy sees committed rows.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the file cannot be opened.
    pub fn checkpoint(path: &Path) -> Result<(), StorageError> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA wal_checkpoint(FULL);")?;
        Ok(())
    }

    /// Opens a private in-memory database and applies the schema.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot create the database.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self, StorageError> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Inserts a user or replaces the stored name and timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the row.
    pub fn upsert_user(&self, user: &UserRecord) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO users (user_id, display_name, zalo_name, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET
                display_name = excluded.display_name,
                zalo_name = excluded.zalo_name,
                updated_at = excluded.updated_at",
            params![
                user.user_id,
                user.display_name,
                user.zalo_name,
                user.updated_at
            ],
        )?;
        Ok(())
    }

    /// Inserts a thread or replaces its title, peer, unread count and timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the peer user does not exist or SQLite
    /// rejects the row.
    pub fn upsert_thread(&self, thread: &ThreadRecord) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO threads (thread_id, kind, title, peer_user_id, unread, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(thread_id) DO UPDATE SET
                kind = excluded.kind,
                title = excluded.title,
                peer_user_id = excluded.peer_user_id,
                unread = excluded.unread,
                updated_at = excluded.updated_at",
            params![
                thread.thread_id,
                thread.kind.as_str(),
                thread.title,
                thread.peer_user_id,
                thread.unread,
                thread.updated_at
            ],
        )?;
        Ok(())
    }

    /// Updates only `title`. Unread, kind, peer and timestamp stay as stored.
    /// A missing thread is left absent.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the update.
    pub fn set_thread_title(&self, thread_id: &str, title: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE threads SET title = ?1 WHERE thread_id = ?2",
            params![title, thread_id],
        )?;
        Ok(())
    }

    /// Inserts a message and returns the new `message_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the thread or sender is missing, or when
    /// `cli_msg_id` is already stored for that thread.
    pub fn insert_message(&self, message: &MessageRecord) -> Result<i64, StorageError> {
        self.conn.execute(
            "INSERT INTO messages (
                thread_id, sender_id, cli_msg_id, global_msg_id, body, sent_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                message.thread_id,
                message.sender_id,
                message.cli_msg_id,
                message.global_msg_id,
                message.body,
                message.sent_at
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Loads one user by id.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn user(&self, user_id: &str) -> Result<Option<UserRecord>, StorageError> {
        let found = self
            .conn
            .query_row(
                "SELECT user_id, display_name, zalo_name, updated_at
                 FROM users WHERE user_id = ?1",
                params![user_id],
                map_user,
            )
            .optional()?;
        Ok(found)
    }

    /// Lists all stored users, newest `updated_at` first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when a row cannot be read.
    pub fn users(&self) -> Result<Vec<UserRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, display_name, zalo_name, updated_at
             FROM users ORDER BY updated_at DESC, user_id",
        )?;
        let rows = stmt.query_map([], map_user)?;
        let mut users = Vec::new();
        for row in rows {
            users.push(row?);
        }
        Ok(users)
    }

    /// Lists threads, newest `updated_at` first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when a row cannot be read.
    pub fn threads(&self) -> Result<Vec<ThreadRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT thread_id, kind, title, peer_user_id, unread, updated_at
             FROM threads ORDER BY updated_at DESC, thread_id",
        )?;
        let rows = stmt.query_map([], map_thread_row)?;
        let mut threads = Vec::new();
        for row in rows {
            let (thread_id, kind, title, peer_user_id, unread, updated_at) = row?;
            threads.push(ThreadRecord {
                thread_id,
                kind: ThreadKind::parse(&kind)?,
                title,
                peer_user_id,
                unread,
                updated_at,
            });
        }
        Ok(threads)
    }

    /// Lists messages in one thread, oldest `sent_at` first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn messages(&self, thread_id: &str) -> Result<Vec<MessageRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT message_id, thread_id, sender_id, cli_msg_id, global_msg_id, body, sent_at
             FROM messages
             WHERE thread_id = ?1
             ORDER BY sent_at, message_id",
        )?;
        let rows = stmt.query_map(params![thread_id], map_message)?;
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }

    /// Lists all messages across every thread, oldest `sent_at` first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn all_messages(&self) -> Result<Vec<MessageRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT message_id, thread_id, sender_id, cli_msg_id, global_msg_id, body, sent_at
             FROM messages
             ORDER BY sent_at, message_id",
        )?;
        let rows = stmt.query_map([], map_message)?;
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        Ok(messages)
    }

    /// Stores (or replaces) the attachment JSON of one message, such as a
    /// photo's URLs and size.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the message does not exist or SQLite
    /// rejects the row.
    pub fn set_attachment(&self, message_id: i64, data: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT INTO attachments (message_id, data) VALUES (?1, ?2)
             ON CONFLICT(message_id) DO UPDATE SET data = excluded.data",
            params![message_id, data],
        )?;
        Ok(())
    }

    /// Every stored attachment as `(message_id, data)`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn attachments(&self) -> Result<Vec<(i64, String)>, StorageError> {
        let mut stmt = self
            .conn
            .prepare("SELECT message_id, data FROM attachments ORDER BY message_id")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Newest server message id stored for direct threads and for groups,
    /// as `(direct, group)`. The realtime socket resumes after these.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn newest_global_ids(&self) -> Result<(Option<i64>, Option<i64>), StorageError> {
        let newest = |kind: &str| -> Result<Option<i64>, StorageError> {
            Ok(self.conn.query_row(
                "SELECT MAX(m.global_msg_id) FROM messages m
                 JOIN threads t ON t.thread_id = m.thread_id
                 WHERE t.kind = ?1",
                params![kind],
                |row| row.get::<_, Option<i64>>(0),
            )?)
        };
        Ok((newest("user")?, newest("group")?))
    }

    /// Time of the newest stored message in `thread_id` whose server id is at
    /// most `global_msg_id`: how far a "read up to id X" mark reaches.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn newest_at_upto(
        &self,
        thread_id: &str,
        global_msg_id: i64,
    ) -> Result<Option<i64>, StorageError> {
        Ok(self.conn.query_row(
            "SELECT MAX(sent_at) FROM messages
             WHERE thread_id = ?1 AND global_msg_id IS NOT NULL AND global_msg_id <= ?2",
            params![thread_id, global_msg_id],
            |row| row.get::<_, Option<i64>>(0),
        )?)
    }

    /// Fills in the server ids of a message sent from here once its echo
    /// comes back over the socket.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the statement.
    pub fn set_server_ids(
        &self,
        message_id: i64,
        cli_msg_id: Option<i64>,
        global_msg_id: i64,
    ) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE messages SET global_msg_id = ?2, cli_msg_id = COALESCE(cli_msg_id, ?3)
             WHERE message_id = ?1",
            params![message_id, global_msg_id, cli_msg_id],
        )?;
        Ok(())
    }

    /// `true` when a message with this server id is already stored in the thread.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn has_global_id(&self, thread_id: &str, global_msg_id: i64) -> Result<bool, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM messages WHERE thread_id = ?1 AND global_msg_id = ?2 LIMIT 1",
                params![thread_id, global_msg_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Deletes one message by `message_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the statement.
    pub fn delete_message(&self, message_id: i64) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM messages WHERE message_id = ?1",
            params![message_id],
        )?;
        Ok(())
    }

    /// Replaces the body of every message in `thread_id` whose Zalo id is
    /// `global_msg_id` (a recalled message). Returns the local `message_id`s.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the statement.
    pub fn replace_body_by_global_id(
        &self,
        thread_id: &str,
        global_msg_id: i64,
        body: &str,
    ) -> Result<Vec<i64>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT message_id FROM messages WHERE thread_id = ?1 AND global_msg_id = ?2",
        )?;
        let ids = stmt
            .query_map(params![thread_id, global_msg_id], |row| {
                row.get::<_, i64>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        self.conn.execute(
            "UPDATE messages SET body = ?3 WHERE thread_id = ?1 AND global_msg_id = ?2",
            params![thread_id, global_msg_id, body],
        )?;
        Ok(ids)
    }

    /// Deletes all messages belonging to `thread_id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the statement.
    pub fn delete_thread_messages(&self, thread_id: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "DELETE FROM messages WHERE thread_id = ?1",
            params![thread_id],
        )?;
        Ok(())
    }

    /// Deletes every message in the local database while keeping users and threads.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite rejects the statement.
    pub fn clear_messages(&self) -> Result<(), StorageError> {
        self.conn.execute("DELETE FROM messages", [])?;
        Ok(())
    }

    /// Searches stored messages in `thread_id` (or all threads when `None`) by
    /// case-insensitive UTF-8 substring, newest `sent_at` first.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the query fails.
    pub fn search_messages(
        &self,
        thread_id: Option<&str>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MessageRecord>, StorageError> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT message_id, thread_id, sender_id, cli_msg_id, global_msg_id, body, sent_at
             FROM messages
             WHERE (?1 IS NULL OR thread_id = ?1)
             ORDER BY sent_at DESC, message_id DESC",
        )?;
        let rows = stmt.query_map(params![thread_id], map_message)?;
        let mut out = Vec::new();
        for row in rows {
            let msg = row?;
            if msg.body.to_lowercase().contains(&needle) {
                out.push(msg);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Search messages inside a single thread combining optional keyword,
    /// optional sender filter, and optional inclusive Unix-ms time range
    /// (`searchForEmpty: true` matching Zalo Electron `compact2438.js`).
    pub fn search_thread_filtered(
        &self,
        thread_id: &str,
        query: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MessageRecord>, StorageError> {
        let needle = query.trim().to_lowercase();
        if limit == 0 || (needle.is_empty() && from_ms.is_none() && to_ms.is_none()) {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT message_id, thread_id, sender_id, cli_msg_id, global_msg_id, body, sent_at
             FROM messages
             WHERE thread_id = ?1
               AND (?2 IS NULL OR sent_at >= ?2)
               AND (?3 IS NULL OR sent_at <= ?3)
             ORDER BY sent_at DESC, message_id DESC",
        )?;
        let rows = stmt.query_map(params![thread_id, from_ms, to_ms], map_message)?;
        let mut out = Vec::new();
        for row in rows {
            let msg = row?;
            if needle.is_empty() || msg.body.to_lowercase().contains(&needle) {
                out.push(msg);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Appends a pending outgoing message to the persistent offline outbox.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot write the row.
    pub fn enqueue_outbox(&self, rec: &OutboxRecord) -> Result<i64, StorageError> {
        self.conn.execute(
            "INSERT INTO outbox (thread_id, is_group, body, urgency, client_id, quote_json, created_at, attempts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rec.thread_id,
                i64::from(rec.is_group),
                rec.body,
                i64::from(rec.urgency),
                rec.client_id as i64,
                rec.quote_json,
                rec.created_at,
                rec.attempts,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Returns all queued outgoing messages in FIFO order.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot query `outbox`.
    pub fn list_outbox(&self) -> Result<Vec<OutboxRecord>, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, thread_id, is_group, body, urgency, client_id, quote_json, created_at, attempts
             FROM outbox
             ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], map_outbox)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Deletes a delivered or abandoned outbox entry by row `id`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot delete the row.
    pub fn remove_outbox(&self, id: i64) -> Result<(), StorageError> {
        self.conn
            .execute("DELETE FROM outbox WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Inserts or updates a batch of threads inside a single SQLite transaction,
    /// ensuring placeholder `users` rows exist for any `peer_user_id` so foreign
    /// key constraints never drop synced 1-1 conversations.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the SQLite transaction fails.
    pub fn upsert_threads_batch(&mut self, threads: &[ThreadRecord]) -> Result<usize, StorageError> {
        if threads.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        {
            let mut user_stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO users (user_id, display_name, zalo_name, updated_at)
                 VALUES (?1, ?2, '', ?3)",
            )?;
            let mut thread_stmt = tx.prepare_cached(
                "INSERT INTO threads (thread_id, kind, title, peer_user_id, unread, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(thread_id) DO UPDATE SET
                    kind = excluded.kind,
                    title = CASE WHEN threads.title = '' THEN excluded.title ELSE threads.title END,
                    peer_user_id = COALESCE(threads.peer_user_id, excluded.peer_user_id),
                    updated_at = MAX(threads.updated_at, excluded.updated_at)",
            )?;
            for thread in threads {
                if let Some(peer_uid) = thread.peer_user_id.as_deref().filter(|s| !s.is_empty()) {
                    let display_name = if thread.title.is_empty() {
                        peer_uid
                    } else {
                        thread.title.as_str()
                    };
                    user_stmt.execute(params![peer_uid, display_name, thread.updated_at])?;
                }
                thread_stmt.execute(params![
                    thread.thread_id,
                    thread.kind.as_str(),
                    thread.title,
                    thread.peer_user_id,
                    thread.unread,
                    thread.updated_at
                ])?;
            }
        }
        tx.commit()?;
        Ok(threads.len())
    }

    /// Inserts a batch of synced messages (and optional attachment JSON) inside a
    /// single SQLite transaction, deduplicating by `(thread_id, global_msg_id)`
    /// and `(thread_id, cli_msg_id)`, and ensuring placeholder `users` rows exist
    /// for every `sender_id` so foreign key constraints never drop messages.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when the SQLite transaction fails.
    pub fn insert_messages_batch(
        &mut self,
        items: &[(MessageRecord, Option<String>)],
    ) -> Result<usize, StorageError> {
        if items.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut inserted = 0usize;
        {
            let mut user_stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO users (user_id, display_name, zalo_name, updated_at)
                 VALUES (?1, ?2, '', ?3)",
            )?;
            let mut has_global_stmt = tx.prepare_cached(
                "SELECT 1 FROM messages WHERE thread_id = ?1 AND global_msg_id = ?2 LIMIT 1",
            )?;
            let mut insert_stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO messages (
                    thread_id, sender_id, cli_msg_id, global_msg_id, body, sent_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut attach_stmt = tx.prepare_cached(
                "INSERT INTO attachments (message_id, data) VALUES (?1, ?2)
                 ON CONFLICT(message_id) DO UPDATE SET data = excluded.data",
            )?;
            let mut bump_thread_stmt = tx.prepare_cached(
                "UPDATE threads SET updated_at = MAX(updated_at, ?2) WHERE thread_id = ?1",
            )?;
            for (msg, attach) in items {
                if let Some(gid) = msg.global_msg_id {
                    let exists = has_global_stmt
                        .query_row(params![msg.thread_id, gid], |_| Ok(()))
                        .optional()?
                        .is_some();
                    if exists {
                        continue;
                    }
                }
                if let Some(sender) = msg.sender_id.as_deref().filter(|s| !s.is_empty()) {
                    user_stmt.execute(params![sender, sender, msg.sent_at])?;
                }
                let changed = insert_stmt.execute(params![
                    msg.thread_id,
                    msg.sender_id,
                    msg.cli_msg_id,
                    msg.global_msg_id,
                    msg.body,
                    msg.sent_at
                ])?;
                if changed > 0 {
                    inserted += 1;
                    bump_thread_stmt.execute(params![msg.thread_id, msg.sent_at])?;
                    if let Some(data) = attach.as_deref() {
                        let row_id = tx.last_insert_rowid();
                        if row_id > 0 {
                            attach_stmt.execute(params![row_id, data])?;
                        }
                    }
                }
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Increments the failed-attempt counter on an outbox entry.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError`] when SQLite cannot update the row.
    pub fn bump_outbox_attempt(&self, id: i64) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE outbox SET attempts = attempts + 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }
}

fn map_outbox(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxRecord> {
    let is_group: i64 = row.get(2)?;
    let urgency: i64 = row.get(4)?;
    let client_id: i64 = row.get(5)?;
    Ok(OutboxRecord {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        is_group: is_group != 0,
        body: row.get(3)?,
        urgency: u8::try_from(urgency).unwrap_or(0),
        client_id: client_id as u64,
        quote_json: row.get(6)?,
        created_at: row.get(7)?,
        attempts: row.get(8)?,
    })
}

fn map_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserRecord> {
    Ok(UserRecord {
        user_id: row.get(0)?,
        display_name: row.get(1)?,
        zalo_name: row.get(2)?,
        updated_at: row.get(3)?,
    })
}

fn map_thread_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, Option<String>, i64, i64)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageRecord> {
    Ok(MessageRecord {
        message_id: row.get(0)?,
        thread_id: row.get(1)?,
        sender_id: row.get(2)?,
        cli_msg_id: row.get(3)?,
        global_msg_id: row.get(4)?,
        body: row.get(5)?,
        sent_at: row.get(6)?,
    })
}
