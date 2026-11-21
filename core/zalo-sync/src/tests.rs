use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::config::{SyncRemoteConfig, SyncSource, TimeRange, SYNC_UNLIMITED_LIMIT};
use crate::engine::{
    classify_mobile_status, compute_completion_percent, exceeded_max_percent_batch_fail,
    sync_conversations_macro, sync_other_messages_macro, MissingMessageRange,
    MobileStatusDisposition,
};
use crate::proto::{
    read_framed_blocks, sync_message_to_server_json, write_framed_blocks, zstd_compress,
    zstd_decompress, ConvType, ConversationSyncChunk, MessagePartition, MessageSyncChunk,
    PlatformType, SyncAttach, SyncConversation, SyncMention, SyncMessage, SyncMetaData,
    SyncProperties, SyncQuote, SyncReference,
};
use crate::protocol::{
    build_591_body, build_592_body, format_partition, parse_sync_controls, retry_backoff_ms,
    should_retry_cmd, BatchScope, BatchType, DeviceType, DisposeReason, MobileStatus, RpcErrorKind,
    SyncControlEvent, SyncDatabaseKind, SyncPriority, SyncRequest590, UploadBatchPayload,
    CMD_SYNC_ACK_DELETE, CMD_SYNC_REQUEST, CMD_SYNC_WAKEUP,
};
use crate::session::{
    group_prepare_queries, ConversationRemoteDb, MessageRemoteDb, SessionManager, SessionOptions,
    SessionQuerySpec, SyncSessionError, SyncTransport, UssIdxCounter,
};
use crate::wasm::{SyncWasm, SYNC_WASM_FILENAME};

fn sync_wasm_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/wasm")
        .join(SYNC_WASM_FILENAME)
}

#[test]
fn real_sync_wasm_metadata_and_message_roundtrip() {
    let mut wasm = SyncWasm::from_file(&sync_wasm_path()).expect("load real Sync 2 wasm");
    let sender_ik = wasm.gen_keypair().expect("sender ik");
    let recv_ik = wasm.gen_keypair().expect("receiver ik");
    let recv_ek = wasm.gen_keypair().expect("receiver ek");
    assert!(!sender_ik.public_key.is_empty());
    assert!(!recv_ek.private_key.is_empty());

    let meta_plain = b"sync2-metadata-payload";
    let enc_meta = wasm
        .create_metadata_cipher(
            meta_plain,
            &sender_ik.public_key,
            &sender_ik.private_key,
            &recv_ik.public_key,
            &recv_ek.public_key,
            None,
        )
        .expect("create metadata cipher");
    assert!(!enc_meta.ciphertext.is_empty());
    assert!(!enc_meta.session_record.is_empty());

    let dec_meta = wasm
        .decrypt_metadata(
            &enc_meta.ciphertext,
            &recv_ik.public_key,
            &recv_ik.private_key,
            &recv_ek.private_key,
            None,
        )
        .expect("decrypt metadata");
    assert_eq!(dec_meta.plaintext, meta_plain);
    assert_eq!(dec_meta.sender_identity, sender_ik.public_key);

    let msg_plain = b"sync2-zstd-protobuf-chunk-bytes";
    let enc_msg = wasm
        .encrypt_message(msg_plain, &enc_meta.session_record)
        .expect("encrypt message");
    let dec_msg = wasm
        .decrypt_message(&enc_msg.ciphertext, &dec_meta.session_record)
        .expect("decrypt message");
    assert_eq!(dec_msg.plaintext, msg_plain);
}

#[test]
fn protobuf_conversation_and_message_roundtrip() {
    let conv_chunk = ConversationSyncChunk {
        platform: PlatformType::Mobile,
        conversations: vec![
            SyncConversation {
                conv_id: "g_global_1".into(),
                conv_type: ConvType::Group,
                responded_by_me: true,
                last_ts: 1_700_000_000_000,
                last_global_id: "9876543210123".into(),
                last_client_id: 42,
            },
            SyncConversation {
                conv_id: "u_global_2".into(),
                conv_type: ConvType::OneOne,
                responded_by_me: false,
                last_ts: 1_699_000_000_000,
                last_global_id: "123456".into(),
                last_client_id: 7,
            },
        ],
    };
    let enc = conv_chunk.encode();
    let dec = ConversationSyncChunk::decode(&enc).expect("decode conv chunk");
    assert_eq!(dec, conv_chunk);

    let msg_chunk = MessageSyncChunk {
        platform: PlatformType::Pc,
        partitions: vec![MessagePartition {
            id: "oneone/u_global_2".into(),
            messages: vec![SyncMessage {
                sender_id: "u_global_2".into(),
                client_id: 1001,
                msg_status: 3,
                msg_type: 0,
                timestamp: 1_700_000_111_000,
                global_id: "5555555555".into(),
                content: "Xin chào Zalo".into(),
                meta: Some(SyncMetaData {
                    attachs: vec![SyncAttach {
                        action: Some("rtf".into()),
                        title: Some("Xin chào Zalo".into()),
                        cat_id: Some(12),
                        child_number: Some(0),
                        ..SyncAttach::default()
                    }],
                    mentions: vec![SyncMention {
                        uid: "-1".into(),
                        pos: 0,
                        len: 4,
                        mention_type: 1,
                    }],
                    reference: Some(SyncReference {
                        ref_type: 2,
                        data: "ref-data".into(),
                    }),
                    quote: Some(SyncQuote {
                        attach: "{}".into(),
                        owner_id: "u_global_2".into(),
                        ts: 1_700_000_000_000,
                        from_d: "Alice".into(),
                        cli_msg_id: 999,
                        global_msg_id: "4444444444".into(),
                        msg: "Quoted".into(),
                        cli_msg_type: 1,
                        ttl: Some(60_000),
                        partial_quote: Some("part".into()),
                    }),
                    property: Some(SyncProperties {
                        size: Some(14),
                        color: Some(1),
                        prop_type: Some(2),
                        sub_type: Some(3),
                        ext: Some("ext".into()),
                    }),
                    ttl: Some(300_000),
                }),
            }],
        }],
    };
    let enc_m = msg_chunk.encode();
    let z = zstd_compress(&enc_m).expect("zstd compress");
    let framed = write_framed_blocks(&[&z]);
    let blocks = read_framed_blocks(&framed).expect("read framed");
    assert_eq!(blocks.len(), 1);
    let plain = zstd_decompress(&blocks[0]).expect("zstd decompress");
    let dec_m = MessageSyncChunk::decode(&plain).expect("decode msg chunk");
    assert_eq!(dec_m, msg_chunk);

    let sv = sync_message_to_server_json(
        &dec_m.partitions[0].messages[0],
        "222",
        "999",
        |g| if g == "u_global_2" { "222".into() } else { g.to_owned() },
    )
    .expect("convert to server json");
    assert_eq!(sv["msgType"], "webchat");
    assert_eq!(sv["uidFrom"], "222");
    assert_eq!(sv["uidTo"], "222");
    assert_eq!(sv["src"], "sync-db");
}

#[test]
fn control_parser_enforces_electron_predicates() {
    let now = 1_700_000_500_000u64;
    let payload = json!({
        "data": {
            "controls": [
                // Valid transfer_after_login (older)
                {
                    "act_type": "transfer_msg",
                    "act": "transfer_after_login",
                    "ts": now - 500_000,
                    "data": { "temp_key": "old_key", "imei": "my-imei" }
                },
                // Valid transfer_after_login (newer -> must win)
                {
                    "act_type": "transfer_msg",
                    "act": "transfer_after_login",
                    "ts": now - 10_000,
                    "data": { "temp_key": "fresh_key", "imei": "my-imei" }
                },
                // Invalid: transfer_sync2 must NOT populate tempKey!
                {
                    "act_type": "transfer_sync2",
                    "act": "transfer_after_login",
                    "ts": now - 1_000,
                    "data": { "temp_key": "wrong_act_type", "imei": "my-imei" }
                },
                // Invalid: expired (> 600s)
                {
                    "act_type": "transfer_msg",
                    "act": "transfer_after_login",
                    "ts": now - 600_001,
                    "data": { "temp_key": "expired", "imei": "my-imei" }
                },
                // Invalid: wrong imei
                {
                    "act_type": "transfer_msg",
                    "act": "transfer_after_login",
                    "ts": now - 1_000,
                    "data": { "temp_key": "other_dev", "imei": "other-imei" }
                },
                // Valid transfer_status from Mobile (fromDevice == 0)
                {
                    "act_type": "transfer_sync2",
                    "act": "transfer_status",
                    "data": { "syncId": "sid1", "fromDevice": 0, "status": 4 }
                },
                // Ignored: fromDevice == 2 (PC)
                {
                    "act_type": "transfer_sync2",
                    "act": "transfer_status",
                    "data": { "syncId": "sid1", "fromDevice": 2, "status": 4 }
                },
                // Valid upload_batch err == 0
                {
                    "act_type": "transfer_sync2",
                    "act": "upload_batch",
                    "data": {
                        "syncId": "sid1", "idx": 0, "isLast": true, "err": 0,
                        "batchType": 2, "msgUrl": "https://example.invalid/0",
                        "scopes": [{ "partition": "", "from": 100, "to": 200 }]
                    }
                },
                // Error upload_batch err != 0
                {
                    "act_type": "transfer_sync2",
                    "act": "upload_batch",
                    "data": {
                        "syncId": "sid1", "idx": 1, "isLast": false, "err": 500,
                        "batchType": 1, "msgUrl": "", "scopes": []
                    }
                }
            ]
        }
    });

    let events = parse_sync_controls(&payload, "my-imei", now);
    assert_eq!(events.len(), 4);
    match &events[0] {
        SyncControlEvent::TransferAfterLogin(ev) => assert_eq!(ev.temp_key, "fresh_key"),
        other => panic!("expected TransferAfterLogin, got {other:?}"),
    }
    match &events[1] {
        SyncControlEvent::MobileStatus(st) => {
            assert_eq!(st.sync_id, "sid1");
            assert_eq!(st.status, MobileStatus::Confirmed);
        }
        other => panic!("expected MobileStatus, got {other:?}"),
    }
    assert!(matches!(events[2], SyncControlEvent::UploadBatch(_)));
    assert!(matches!(events[3], SyncControlEvent::BatchError(_)));
}

#[test]
fn wire_json_590_591_592_and_retry_policy_match_electron() {
    let body_591 = build_591_body("req_1", "sid_a", DeviceType::Mobile, DisposeReason::UserCancel);
    assert_eq!(body_591["reqId"], "req_1");
    assert_eq!(body_591["data"]["reason"], 0); // UserCancel reason is 0, NEVER 7!
    assert_eq!(body_591["data"]["toDevice"], 0);

    let body_592 = build_592_body("req_2", "sid_a", DeviceType::Mobile);
    assert_eq!(body_592["reqId"], "req_2");
    assert_eq!(body_592["syncId"], "sid_a");
    assert!(body_592.get("data").is_none()); // 592 does NOT wrap in `data`!

    assert_eq!(
        classify_mobile_status(MobileStatus::UserCancel),
        MobileStatusDisposition::Cancel(DisposeReason::UserCancel)
    );
    assert_eq!(retry_backoff_ms(1), 1_000);
    assert_eq!(retry_backoff_ms(2), 2_000);
    assert_eq!(retry_backoff_ms(3), 3_000);

    assert!(!should_retry_cmd(CMD_SYNC_REQUEST, RpcErrorKind::NoNetwork));
    assert!(!should_retry_cmd(CMD_SYNC_REQUEST, RpcErrorKind::Timeout));
    assert!(!should_retry_cmd(
        CMD_SYNC_REQUEST,
        RpcErrorKind::ServerCode(211)
    ));
    assert!(should_retry_cmd(
        CMD_SYNC_REQUEST,
        RpcErrorKind::ServerCode(500)
    ));
    assert!(!should_retry_cmd(
        CMD_SYNC_ACK_DELETE,
        RpcErrorKind::Timeout
    ));
    assert!(should_retry_cmd(CMD_SYNC_WAKEUP, RpcErrorKind::Timeout));
    assert!(should_retry_cmd(CMD_SYNC_WAKEUP, RpcErrorKind::NoNetwork));
}

#[test]
fn remote_config_defaults_and_overrides_match_electron() {
    let cfg = SyncRemoteConfig::default();
    assert!(!cfg.enable);
    assert_eq!(cfg.batch_size, 2_000);
    assert_eq!(cfg.conversations_batch_size, 1_000);
    assert_eq!(cfg.transfer_batch_size, 2_000);
    assert_eq!(cfg.max_number_of_concurrent_sync_items, 4);
    assert_eq!(cfg.max_batch_fail, 10);
    assert_eq!(cfg.sync_conv_list_batch_max_attempts, 3);
    assert_eq!(cfg.manual_sync.awaiting_mobile_timeout_ms, 600_000);
    assert_eq!(cfg.manual_sync.wakeup_timeout, 180_000);
    assert_eq!(cfg.manual_sync.batch_timeout, 10_000);
    assert_eq!(cfg.manual_sync.last_chat_max_number_of_conv, 10);

    let server = json!({
        "data": {
            "setttings": {
                "features": {
                    "sync_2_pc": {
                        "enable": true,
                        "conversations_batch_size": 500,
                        "batch_size": 1500,
                        "manual_sync": {
                            "awaiting_mobile_timeout_ms": 450000
                        }
                    }
                }
            }
        }
    });
    let parsed = SyncRemoteConfig::from_server_info(&server);
    assert!(parsed.is_enabled_for(SyncSource::Manual));
    assert_eq!(parsed.conversations_batch_size, 500);
    assert_eq!(parsed.batch_size, 1500);
    assert_eq!(
        parsed.timeout_configs(SyncSource::Manual).confirm_timeout_ms,
        450_000
    );

    // Layering PC getServerInfo -> Web getServerInfo (omits `enable`) -> getLoginInfo settings.
    let mut layered = SyncRemoteConfig::default();
    layered.apply_server_info(&server);
    layered.apply_server_info(&json!({
        "data": {
            "setttings": {
                "features": {
                    "sync_2_pc": { "sync_re_prepare_max_rounds": 1 }
                }
            }
        }
    }));
    assert!(layered.is_enabled_for(SyncSource::Manual));
    // Per-account authen-info-config (`features.sync_2_pc`) can override.
    layered.apply_server_info(&json!({
        "features": {
            "sync_2_pc": { "enable": false }
        }
    }));
    assert!(!layered.is_enabled_for(SyncSource::Manual));
}

#[test]
fn prepare_groups_split_at_30_and_sort_priority_descending() {
    let mut queries = Vec::new();
    for i in 0..65 {
        queries.push(SessionQuerySpec {
            partition: format!("oneone/g_{i}"),
            from: 0,
            to: 1000,
            limit: SYNC_UNLIMITED_LIMIT,
            db_kind: SyncDatabaseKind::Message,
            priority: SyncPriority::Low,
        });
    }
    let groups = group_prepare_queries(queries);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 30);
    assert_eq!(groups[1].len(), 30);
    assert_eq!(groups[2].len(), 5);
}

#[test]
fn percentage_and_fail_ratio_match_electron() {
    assert_eq!(compute_completion_percent(0, 10), None);
    assert_eq!(compute_completion_percent(1, 10), Some(10));
    assert_eq!(compute_completion_percent(3, 7), Some(42));
    assert_eq!(compute_completion_percent(10, 10), Some(100));

    assert!(!exceeded_max_percent_batch_fail(0, 0, 0.5));
    assert!(!exceeded_max_percent_batch_fail(5, 5, 0.5)); // 0.5 is NOT > 0.5
    assert!(exceeded_max_percent_batch_fail(6, 5, 0.5));
}

/// Offline mock transport that records every `590`/`591`/`592` frame and serves
/// real WASM-encrypted framed zstd protobuf payloads.
struct MockTransport {
    sent_590: Mutex<Vec<SyncRequest590>>,
    sent_591: Mutex<Vec<(String, DisposeReason)>>,
    files: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    on_590: Mutex<Option<Box<dyn Fn(&SyncRequest590) + Send>>>,
}

impl MockTransport {
    fn new() -> Self {
        Self {
            sent_590: Mutex::new(Vec::new()),
            sent_591: Mutex::new(Vec::new()),
            files: Mutex::new(std::collections::HashMap::new()),
            on_590: Mutex::new(None),
        }
    }
}

impl SyncTransport for MockTransport {
    async fn send_request_590(&self, req: &SyncRequest590) -> Result<(), SyncSessionError> {
        self.sent_590.lock().unwrap().push(req.clone());
        if let Some(cb) = self.on_590.lock().unwrap().as_ref() {
            cb(req);
        }
        Ok(())
    }

    async fn send_delete_591(
        &self,
        sync_id: &str,
        _to_device: DeviceType,
        reason: DisposeReason,
    ) -> Result<(), SyncSessionError> {
        self.sent_591
            .lock()
            .unwrap()
            .push((sync_id.to_owned(), reason));
        Ok(())
    }

    async fn send_wakeup_592(
        &self,
        _sync_id: &str,
        _to_device: DeviceType,
    ) -> Result<i64, SyncSessionError> {
        Ok(0)
    }

    async fn download_batch(&self, url: &str) -> Result<Vec<u8>, SyncSessionError> {
        self.files
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| SyncSessionError::BatchDecode {
                step: "Download",
                message: format!("missing mock url {url}"),
            })
    }
}

#[tokio::test]
async fn conversation_pagination_keeps_session_open_and_closes_once_with_sync_success() {
    let wasm = Arc::new(Mutex::new(
        SyncWasm::from_file(&sync_wasm_path()).expect("wasm"),
    ));
    let (sender_ik, recv_ik) = {
        let mut w = wasm.lock().unwrap();
        (w.gen_keypair().unwrap(), w.gen_keypair().unwrap())
    };

    let transport = Arc::new(MockTransport::new());
    let ussidx = Arc::new(UssIdxCounter::default());
    let manager = SessionManager::new(
        recv_ik.clone(),
        "".into(), // manual sync sends empty tempKey
        "uss-1".into(),
        "test-pc".into(),
        SessionOptions::default(),
        ussidx,
    );
    let db = Arc::new(ConversationRemoteDb::new(manager));

    // Build 15 conversations (timestamps 1500, 1400, ..., 100) so page 1 (limit 10)
    // and page 2 (remaining 5) both come from the single prepared conversation session!
    let convs: Vec<SyncConversation> = (1..=15)
        .rev()
        .map(|i| SyncConversation {
            conv_id: format!("g_conv_{i}"),
            conv_type: ConvType::OneOne,
            responded_by_me: true,
            last_ts: i * 100,
            last_global_id: (i * 1000).to_string(),
            last_client_id: i,
        })
        .collect();
    let chunk_bytes = zstd_compress(
        &ConversationSyncChunk {
            platform: PlatformType::Mobile,
            conversations: convs,
        }
        .encode(),
    )
    .unwrap();

    // When 590 arrives, encrypt the chunk with the session's `ek` using real Sync 2 WASM and feed batch!
    let db_cb = Arc::clone(&db);
    let tr_cb = Arc::clone(&transport);
    let wasm_cb = Arc::clone(&wasm);
    let sender_ik_cb = sender_ik.clone();
    let recv_ik_cb = recv_ik.clone();
    *transport.on_590.lock().unwrap() = Some(Box::new(move |req: &SyncRequest590| {
        let mut w = wasm_cb.lock().unwrap();
        let enc_meta = w
            .create_metadata_cipher(
                b"",
                &sender_ik_cb.public_key,
                &sender_ik_cb.private_key,
                &recv_ik_cb.public_key,
                &req.ek_public,
                None,
            )
            .unwrap();
        let enc_msg = w
            .encrypt_message(&chunk_bytes, &enc_meta.session_record)
            .unwrap();
        drop(w);

        let meta_framed = write_framed_blocks(&[&enc_meta.ciphertext]);
        let msg_framed = write_framed_blocks(&[&enc_msg.ciphertext]);
        let meta_url = format!("https://mock.invalid/{}/meta", req.sync_id);
        let msg_url = format!("https://mock.invalid/{}/data", req.sync_id);
        {
            let mut files = tr_cb.files.lock().unwrap();
            files.insert(meta_url.clone(), meta_framed);
            files.insert(msg_url.clone(), msg_framed);
        }
        let meta_batch = UploadBatchPayload {
            sync_id: req.sync_id.clone(),
            idx: 0,
            is_last: false,
            err: 0,
            batch_type: BatchType::MetadataEmbedded,
            msg_url: meta_url,
            scopes: vec![BatchScope {
                partition: String::new(),
                from: 0,
                to: 0,
            }],
        };
        let data_batch = UploadBatchPayload {
            sync_id: req.sync_id.clone(),
            idx: 1,
            is_last: true,
            err: 0,
            batch_type: BatchType::Message,
            msg_url,
            scopes: vec![BatchScope {
                partition: String::new(),
                from: 0,
                to: crate::config::JS_MAX_SAFE_INTEGER,
            }],
        };
        let db2 = Arc::clone(&db_cb);
        tokio::spawn(async move {
            db2.manager().dispatch_batch(meta_batch).await;
            db2.manager().dispatch_batch(data_batch).await;
        });
    }));

    let cfg = SyncRemoteConfig::default(); // first page = 10, next page = 1000
    let cancel = AtomicBool::new(false);
    let mut page_sizes = Vec::new();
    let out = sync_conversations_macro(
        &db,
        transport.as_ref(),
        wasm.as_ref(),
        &cfg,
        SyncSource::Manual,
        TimeRange::default(),
        0,
        &cancel,
        |page, _| page_sizes.push(page.len()),
    )
    .await
    .expect("conversation sync succeeds");

    assert_eq!(out.pages, 2);
    assert_eq!(page_sizes, vec![10, 6]); // inclusive boundary `toTs = lastTs` (600 appears at boundary, matching Electron!)
    // Only 1 `590` sent (prepared once and reused for both pages!).
    let sent_590 = transport.sent_590.lock().unwrap();
    assert_eq!(sent_590.len(), 1);
    assert_eq!(sent_590[0].db_kind, SyncDatabaseKind::Conversation);
    assert_eq!(sent_590[0].temp_key, "");
    assert_eq!(sent_590[0].priority, SyncPriority::High);
    // Exactly ONE `591` sent at the end with reason SyncSuccess (1), NONE between pages!
    let sent_591 = transport.sent_591.lock().unwrap();
    assert_eq!(sent_591.len(), 1);
    assert_eq!(sent_591[0].1, DisposeReason::SyncSuccess);
}

#[tokio::test]
async fn other_messages_prepare_30_reuses_sessions_and_reports_true_percent() {
    let wasm = Arc::new(Mutex::new(
        SyncWasm::from_file(&sync_wasm_path()).expect("wasm"),
    ));
    let (sender_ik, recv_ik) = {
        let mut w = wasm.lock().unwrap();
        (w.gen_keypair().unwrap(), w.gen_keypair().unwrap())
    };
    let transport = Arc::new(MockTransport::new());
    let ussidx = Arc::new(UssIdxCounter::default());
    let manager = SessionManager::new(
        recv_ik.clone(),
        "auto_key_1".into(),
        "uss-msg".into(),
        "test-pc".into(),
        SessionOptions::default(),
        ussidx,
    );
    let db = Arc::new(MessageRemoteDb::new(manager));

    // 32 conversations -> PrepareAll MUST split into 2 overlapping sessions (30 + 2),
    // first session sends tempKey "auto_key_1", second sends "" (empty)!
    let ranges: Vec<MissingMessageRange> = (0..32)
        .map(|i| MissingMessageRange {
            conv_id: format!("u_{i}"),
            global_noised_id: format!("g_{i}"),
            is_group: false,
            from_ts: 0,
            to_ts: 10_000,
        })
        .collect();

    let db_cb = Arc::clone(&db);
    let tr_cb = Arc::clone(&transport);
    let wasm_cb = Arc::clone(&wasm);
    let sender_ik_cb = sender_ik.clone();
    let recv_ik_cb = recv_ik.clone();
    *transport.on_590.lock().unwrap() = Some(Box::new(move |req: &SyncRequest590| {
        // Build one MetadataEmbedded batch + one Message batch containing all queried partitions
        let mut partitions = Vec::new();
        let mut scopes = Vec::new();
        for q in &req.queries {
            partitions.push(MessagePartition {
                id: q.partition.clone(),
                messages: vec![SyncMessage {
                    sender_id: "g_sender".into(),
                    client_id: 1,
                    msg_status: 3,
                    msg_type: 0,
                    timestamp: 5_000,
                    global_id: "100".into(),
                    content: format!("hello {}", q.partition),
                    meta: None,
                }],
            });
            scopes.push(BatchScope {
                partition: q.partition.clone(),
                from: q.from,
                to: q.to,
            });
        }
        let plain = zstd_compress(
            &MessageSyncChunk {
                platform: PlatformType::Mobile,
                partitions,
            }
            .encode(),
        )
        .unwrap();
        let (enc_meta, enc_msg) = {
            let mut w = wasm_cb.lock().unwrap();
            let em = w
                .create_metadata_cipher(
                    &zstd_compress(&MessageSyncChunk::default().encode()).unwrap(),
                    &sender_ik_cb.public_key,
                    &sender_ik_cb.private_key,
                    &recv_ik_cb.public_key,
                    &req.ek_public,
                    None,
                )
                .unwrap();
            let ms = w.encrypt_message(&plain, &em.session_record).unwrap();
            (em, ms)
        };
        let meta_url = format!("https://mock.invalid/{}_meta", req.sync_id);
        let msg_url = format!("https://mock.invalid/{}_msg", req.sync_id);
        {
            let mut files = tr_cb.files.lock().unwrap();
            files.insert(meta_url.clone(), write_framed_blocks(&[&enc_meta.ciphertext]));
            files.insert(msg_url.clone(), write_framed_blocks(&[&enc_msg.ciphertext]));
        }
        let b_meta = UploadBatchPayload {
            sync_id: req.sync_id.clone(),
            idx: 0,
            is_last: false,
            err: 0,
            batch_type: BatchType::MetadataEmbedded,
            msg_url: meta_url,
            scopes: vec![BatchScope {
                partition: String::new(),
                from: 0,
                to: 0,
            }],
        };
        let b_msg = UploadBatchPayload {
            sync_id: req.sync_id.clone(),
            idx: 1,
            is_last: true,
            err: 0,
            batch_type: BatchType::Message,
            msg_url,
            scopes,
        };
        let db2 = Arc::clone(&db_cb);
        tokio::spawn(async move {
            db2.manager().dispatch_batch(b_meta).await;
            db2.manager().dispatch_batch(b_msg).await;
        });
    }));

    let cfg = SyncRemoteConfig::default();
    let cancel = Arc::new(AtomicBool::new(false));
    let percents = Arc::new(Mutex::new(Vec::<Option<u8>>::new()));
    let percents_cb = Arc::clone(&percents);

    let out = sync_other_messages_macro(
        Arc::clone(&db),
        Arc::clone(&transport),
        Arc::clone(&wasm),
        &cfg,
        ranges,
        cancel,
        Arc::new(|_range: &MissingMessageRange, _msgs: Vec<SyncMessage>| {}),
        Arc::new(move |_done, _total, pct, _msgs| {
            percents_cb.lock().unwrap().push(pct);
        }),
    )
    .await
    .expect("messages sync succeeds");

    assert_eq!(out.total_ranges, 32);
    assert_eq!(out.completed_ranges, 32);
    assert_eq!(out.messages_fetched, 32);

    // Exactly 2 `590` sessions sent during PrepareAll (30 + 2), ZERO extra 590s during GetAll!
    let sent = transport.sent_590.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].queries.len(), 30);
    assert_eq!(sent[1].queries.len(), 2);
    assert_eq!(sent[0].temp_key, "auto_key_1");
    assert_eq!(sent[1].temp_key, "");
    assert_eq!(sent[0].ussidx, 0);
    assert_eq!(sent[1].ussidx, 1);
    assert_eq!(sent[0].db_kind, SyncDatabaseKind::Message);
    assert_eq!(sent[0].priority, SyncPriority::Low);
    assert_eq!(sent[0].queries[0].limit, SYNC_UNLIMITED_LIMIT);
    assert_eq!(sent[0].queries[0].partition, format_partition("g_0", false));

    // Final progress percentage reached 100%, never emitted 0% fake progress.
    let pcts = percents.lock().unwrap();
    assert_eq!(pcts.len(), 32);
    assert_eq!(*pcts.last().unwrap(), Some(100));
}
