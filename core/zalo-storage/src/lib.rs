//! Backup decryption and the local message store.
//!
//! The cryptographic half follows `db-cross-v4`: an AES-256 key taken from the
//! first 32 characters of the private key, chunked AES-256-CBC, then an XZ
//! stream. The SQLite half is this client's own `users`, `threads` and
//! `messages` tables.

#![forbid(unsafe_code)]

mod crypto;
mod error;
mod store;
mod zalo_pc;
mod zdb4;

pub use crypto::{
    aes256_cbc_decrypt_nopad, aes256_cbc_decrypt_nopad_chunked, decompress_xz_lzma2,
    derive_aes256_key_zalo_v2,
};
pub use error::StorageError;
pub use store::{LocalStore, MessageRecord, ThreadKind, ThreadRecord, UserRecord};
pub use zalo_pc::{read_zalo_pc_friends, read_zalo_pc_oa, zalo_pc_index_db, PcFriend};
pub use zdb4::{decrypt_zdb4_v2, ExtractedFile};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::crypto::{aes256_cbc_encrypt_nopad_chunked, compress_xz_lzma2};

    const FIXTURE_KEY: &str = "0123456789abcdef0123456789abcdef";

    /// Ciphertext produced by OpenSSL 3.5 `enc -aes-256-cbc -nopad` with a zero
    /// IV. The key is the ASCII bytes of `0123456789ABCDEF0123456789ABCDEF`.
    /// Plaintext is `ZDB4.0-fixture!!` followed by `BLOCK-TWO-DATA!!`.
    const OPENSSL_CT: [u8; 32] = [
        0x54, 0x8b, 0xbc, 0x07, 0xd9, 0x94, 0xa0, 0xf6, 0x42, 0xe1, 0xb2, 0x45, 0xe4, 0x17, 0xe8,
        0x89, 0xba, 0x5c, 0xd3, 0xcc, 0xfe, 0x3d, 0x27, 0x57, 0x8b, 0xe2, 0xc8, 0x09, 0xe8, 0x8a,
        0x04, 0x5d,
    ];

    #[test]
    fn key_is_first_32_uppercase_ascii_bytes() {
        let key = derive_aes256_key_zalo_v2(FIXTURE_KEY);
        assert_eq!(&key, b"0123456789ABCDEF0123456789ABCDEF");

        let mut short = [0_u8; 32];
        short[0] = b'A';
        short[1] = b'B';
        assert_eq!(derive_aes256_key_zalo_v2("ab"), short);

        let hex_looking = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(hex_looking.len(), 64);
        let long = derive_aes256_key_zalo_v2(hex_looking);
        assert_eq!(&long, b"0123456789ABCDEF0123456789ABCDEF");
        assert_eq!(long[0], b'0');
    }

    #[test]
    fn openssl_vector_decrypts_two_chained_blocks() {
        let key = derive_aes256_key_zalo_v2(FIXTURE_KEY);
        let plaintext = b"ZDB4.0-fixture!!BLOCK-TWO-DATA!!";
        let got = aes256_cbc_decrypt_nopad_chunked(&OPENSSL_CT, &key).unwrap();
        assert_eq!(got, plaintext);
    }

    #[test]
    fn chunk_boundary_resets_the_iv() {
        let mut plaintext = vec![0_u8; 0x1_0000 + 16];
        plaintext[0] = 0x11;
        plaintext[0x1_0000] = 0x22;
        let key = derive_aes256_key_zalo_v2("chunk-boundary-key-material-xxxx");
        let ciphertext = aes256_cbc_encrypt_nopad_chunked(&plaintext, &key).unwrap();
        let chunked = aes256_cbc_decrypt_nopad_chunked(&ciphertext, &key).unwrap();
        assert_eq!(chunked, plaintext);
        let chained = aes256_cbc_decrypt_nopad(&ciphertext, &key).unwrap();
        assert_eq!(&chained[..0x1_0000], &plaintext[..0x1_0000]);
        assert_ne!(&chained[0x1_0000..], &plaintext[0x1_0000..]);
    }

    #[test]
    fn rejects_a_short_or_unaligned_blob() {
        let err = decrypt_zdb4_v2(&[0_u8; 8], FIXTURE_KEY).unwrap_err();
        assert!(matches!(err, StorageError::InputTooSmall));
        let err = decrypt_zdb4_v2(&[0_u8; 17], FIXTURE_KEY).unwrap_err();
        assert!(matches!(err, StorageError::NotBlockAligned));
    }

    #[test]
    fn xz_round_trip_ignores_zero_padding() {
        let raw = b"SQLite format 3\0payload";
        let mut packed = compress_xz_lzma2(raw).unwrap();
        packed.extend_from_slice(&[0_u8; 8]);
        let got = decompress_xz_lzma2(&packed).unwrap();
        assert_eq!(got, raw);
    }

    #[test]
    fn zdb4_round_trip_restores_sqlite_rows() {
        let dir = temp_dir();
        let db_path = dir.join("msg.db");
        let user = sample_user();
        let thread = sample_thread();
        let message = sample_message();
        {
            let store = LocalStore::open(&db_path).unwrap();
            store.upsert_user(&user).unwrap();
            store.upsert_thread(&thread).unwrap();
            let id = store.insert_message(&message).unwrap();
            assert!(id > 0);
        }
        let db_bytes = fs::read(&db_path).unwrap();
        assert!(db_bytes.starts_with(b"SQLite format 3\0"));
        let note = b"ghi chu";
        let plaintext = pack_zdb4(&[
            ("note.txt", note.as_slice()),
            ("msg.db", db_bytes.as_slice()),
        ]);
        let key = derive_aes256_key_zalo_v2(FIXTURE_KEY);
        let ciphertext = aes256_cbc_encrypt_nopad_chunked(&plaintext, &key).unwrap();

        let prefixed = decrypt_with_prefix(&ciphertext);
        assert_eq!(prefixed.len(), 2);
        assert_eq!(prefixed[0].name, "note.txt");
        assert_eq!(prefixed[0].bytes, note);
        assert_eq!(prefixed[1].bytes, db_bytes);

        let mut trailed = ciphertext.clone();
        trailed.extend_from_slice(&[9, 8, 7, 6]);
        let from_trailer = decrypt_zdb4_v2(&trailed, FIXTURE_KEY).unwrap();
        assert_eq!(from_trailer[1].bytes, db_bytes);

        let restored_path = dir.join("restored.db");
        fs::write(&restored_path, &prefixed[1].bytes).unwrap();
        let restored = LocalStore::open(&restored_path).unwrap();
        let loaded = restored.user("user-1001").unwrap().unwrap();
        assert_eq!(loaded, user);
        let threads = restored.threads().unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].kind, ThreadKind::User);
        assert_eq!(threads[0].title, "Ban thu");
        let messages = restored.messages("thread-1001").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, "tin thu nghiem");
        assert_eq!(messages[0].cli_msg_id, Some(11));
        assert_eq!(messages[0].global_msg_id, Some(22));

        let wrong = decrypt_zdb4_v2(&ciphertext, "ffffffffffffffffffffffffffffffff");
        assert!(matches!(wrong, Err(StorageError::BadMagic)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn archive_without_a_sqlite_database_is_rejected() {
        let plaintext = pack_zdb4(&[("note.txt", b"chi co ghi chu")]);
        let key = derive_aes256_key_zalo_v2(FIXTURE_KEY);
        let ciphertext = aes256_cbc_encrypt_nopad_chunked(&plaintext, &key).unwrap();
        let err = decrypt_zdb4_v2(&ciphertext, FIXTURE_KEY).unwrap_err();
        assert!(matches!(err, StorageError::NoDatabase));

        let plaintext = pack_zdb4(&[("msg.db", b"not a sqlite file")]);
        let ciphertext = aes256_cbc_encrypt_nopad_chunked(&plaintext, &key).unwrap();
        let err = decrypt_zdb4_v2(&ciphertext, FIXTURE_KEY).unwrap_err();
        assert!(matches!(err, StorageError::NotSqlite));
    }

    #[test]
    fn store_updates_rows_and_enforces_foreign_keys() {
        let store = LocalStore::open_in_memory().unwrap();
        let mut user = sample_user();
        store.upsert_user(&user).unwrap();
        store.upsert_thread(&sample_thread()).unwrap();
        store.insert_message(&sample_message()).unwrap();

        user.display_name = "Ban thu 2".to_owned();
        user.updated_at = 1_700_000_000_500;
        store.upsert_user(&user).unwrap();
        let loaded = store.user("user-1001").unwrap().unwrap();
        assert_eq!(loaded.display_name, "Ban thu 2");

        let mut group = sample_thread();
        group.thread_id = "group-7".to_owned();
        group.kind = ThreadKind::Group;
        group.title = "Nhom thu".to_owned();
        group.peer_user_id = None;
        group.updated_at = 1_700_000_000_900;
        store.upsert_thread(&group).unwrap();
        store.set_thread_title("group-7", "Ten moi").unwrap();
        let renamed = store
            .threads()
            .unwrap()
            .into_iter()
            .find(|t| t.thread_id == "group-7")
            .unwrap();
        assert_eq!(renamed.title, "Ten moi");
        assert_eq!(renamed.unread, group.unread);
        assert_eq!(renamed.updated_at, group.updated_at);
        let mut second = sample_message();
        second.thread_id = "group-7".to_owned();
        second.cli_msg_id = Some(12);
        second.sent_at = 1_700_000_000_800;
        second.body = "tin nhom".to_owned();
        store.insert_message(&second).unwrap();

        let threads = store.threads().unwrap();
        assert_eq!(threads[0].thread_id, "group-7");
        assert_eq!(threads[0].kind, ThreadKind::Group);
        assert_eq!(store.messages("group-7").unwrap()[0].body, "tin nhom");
        let global = store.messages("group-7").unwrap()[0].global_msg_id;
        if let Some(global) = global {
            let ids = store
                .replace_body_by_global_id("group-7", global, "Tin nhắn đã thu hồi")
                .unwrap();
            assert_eq!(ids.len(), 1);
            assert_eq!(
                store.messages("group-7").unwrap()[0].body,
                "Tin nhắn đã thu hồi"
            );
            store
                .replace_body_by_global_id("group-7", global, "tin nhom")
                .unwrap();
        }
        assert!(store
            .replace_body_by_global_id("group-7", -1, "x")
            .unwrap()
            .is_empty());

        let mut orphan = sample_message();
        orphan.thread_id = "missing".to_owned();
        orphan.cli_msg_id = Some(99);
        assert!(store.insert_message(&orphan).is_err());
        assert!(store.user("absent").unwrap().is_none());

        let users = store.users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].user_id, "user-1001");

        let all = store.all_messages().unwrap();
        assert_eq!(all.len(), 2);

        store
            .set_attachment(all[0].message_id, r#"{"thumb":"a"}"#)
            .unwrap();
        store
            .set_attachment(all[0].message_id, r#"{"thumb":"b"}"#)
            .unwrap();
        assert_eq!(
            store.attachments().unwrap(),
            vec![(all[0].message_id, r#"{"thumb":"b"}"#.to_owned())]
        );
        assert!(store.set_attachment(9_999, "{}").is_err());

        store.delete_message(all[0].message_id).unwrap();
        assert_eq!(store.all_messages().unwrap().len(), 1);
        assert!(
            store.attachments().unwrap().is_empty(),
            "attachment follows its message"
        );

        store.delete_thread_messages("group-7").unwrap();
        assert!(store.messages("group-7").unwrap().is_empty());

        store.insert_message(&sample_message()).unwrap();
        assert_eq!(store.all_messages().unwrap().len(), 1);
        store.clear_messages().unwrap();
        assert!(store.all_messages().unwrap().is_empty());
    }

    #[test]
    fn searches_messages_by_utf8_substring_and_thread() {
        let store = LocalStore::open_in_memory().unwrap();
        store.upsert_user(&sample_user()).unwrap();
        store.upsert_thread(&sample_thread()).unwrap();
        store
            .upsert_thread(&ThreadRecord {
                thread_id: "group-7".to_owned(),
                kind: ThreadKind::Group,
                title: "Nhom B".to_owned(),
                peer_user_id: None,
                unread: 0,
                updated_at: 1_700_000_000_200,
            })
            .unwrap();
        store
            .insert_message(&MessageRecord {
                message_id: 0,
                thread_id: "thread-1001".to_owned(),
                sender_id: Some("user-1001".to_owned()),
                cli_msg_id: Some(1),
                global_msg_id: Some(101),
                body: "Hợp đồng tháng 11".to_owned(),
                sent_at: 100,
            })
            .unwrap();
        store
            .insert_message(&MessageRecord {
                message_id: 0,
                thread_id: "group-7".to_owned(),
                sender_id: Some("user-1001".to_owned()),
                cli_msg_id: Some(2),
                global_msg_id: Some(102),
                body: "Đặt hàng %100".to_owned(),
                sent_at: 200,
            })
            .unwrap();

        let local = store.search_messages(Some("thread-1001"), "HỢP", 20).unwrap();
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].global_msg_id, Some(101));

        let global = store.search_messages(None, "đặt hàng %100", 20).unwrap();
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].thread_id, "group-7");
        assert!(store.search_messages(None, "   ", 10).unwrap().is_empty());
    }

    fn decrypt_with_prefix(ciphertext: &[u8]) -> Vec<ExtractedFile> {
        let mut blob = vec![0xaa, 0xbb, 0xcc, 0xdd];
        blob.extend_from_slice(ciphertext);
        decrypt_zdb4_v2(&blob, FIXTURE_KEY).unwrap()
    }

    fn pack_zdb4(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (_, bytes) in files {
            raw.extend_from_slice(bytes);
        }
        let xz = compress_xz_lzma2(&raw).unwrap();
        let mut plaintext = Vec::new();
        plaintext.extend_from_slice(b"ZDB4.0");
        plaintext.extend_from_slice(&[0_u8; 8]);
        plaintext.extend_from_slice(&u32::try_from(files.len()).unwrap().to_be_bytes());
        for (name, bytes) in files {
            let name_bytes = name.as_bytes();
            plaintext.extend_from_slice(&u32::try_from(name_bytes.len()).unwrap().to_be_bytes());
            plaintext.extend_from_slice(name_bytes);
            plaintext.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
        }
        plaintext.extend_from_slice(&xz);
        while plaintext.len() % 16 != 0 {
            plaintext.push(0);
        }
        plaintext
    }

    fn sample_user() -> UserRecord {
        UserRecord {
            user_id: "user-1001".to_owned(),
            display_name: "Ban thu".to_owned(),
            zalo_name: "ban.thu".to_owned(),
            updated_at: 1_700_000_000_000,
        }
    }

    fn sample_thread() -> ThreadRecord {
        ThreadRecord {
            thread_id: "thread-1001".to_owned(),
            kind: ThreadKind::User,
            title: "Ban thu".to_owned(),
            peer_user_id: Some("user-1001".to_owned()),
            unread: 1,
            updated_at: 1_700_000_000_100,
        }
    }

    fn sample_message() -> MessageRecord {
        MessageRecord {
            message_id: 0,
            thread_id: "thread-1001".to_owned(),
            sender_id: Some("user-1001".to_owned()),
            cli_msg_id: Some(11),
            global_msg_id: Some(22),
            body: "tin thu nghiem".to_owned(),
            sent_at: 1_700_000_000_200,
        }
    }

    fn temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("zalo-storage-{}-{nanos}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
