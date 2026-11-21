use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::config::{
    seconds_to_ms_with_sentinel, BackupRemoteConfig, ServerBackupConfigs, UPLOAD_REUSE_WINDOW_MS,
};
use crate::guard::{
    check_cooldown, evaluate_confirm_new_backup, BackupErrorCategory, ConfirmNewBackupDecision,
    ConfirmSourcePopup, SpamRuleTracker,
};
use crate::packer::{
    build_message_scopes, compute_ui_pack_percent, compute_ui_upload_percent,
    compute_worker_progress_percent, drop_unnoisable_scopes, pack_backup_file, read_framed_blocks,
    BackupAttach, BackupConversationInput, BackupMention, BackupQuote, LocalBackupMessage,
    PackParams, PackerError, MESSAGE_SCOPE_LIMIT,
};
use crate::pipeline::{
    build_and_validate_metadata, ensure_init_630, ensure_session_before_engine,
    fetch_configs_and_status, get_resume_step_for, next_auto_resume_delay_ms,
    trigger_mobile_restore_swallow_errors, upload_backup_file_chunked, BackupDoneMonitor,
    BackupDoneMonitorStatus, BackupInterruptState, BackupPipelineError, BackupSessionState,
    BackupTimingConfig, BackupTransport, ResumeStep, UploadChunkError, UploadChunkRequest,
};
use crate::protocol::{
    build_init_630_payload, generate_client_salt, parse_630_response, parse_631_response,
    parse_632_status, parse_634_configs, parse_backup_controls, BackupControlEvent, BackupRpcError,
    BackupStatus632,
};
use crate::wasm::{derive_backup_wasm_key, BackupCryptoSession, BACKUP_WASM_FILENAME};

fn workspace_backup_wasm_path() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("../../assets/wasm")
        .join(BACKUP_WASM_FILENAME)
}

fn fast_timing() -> BackupTimingConfig {
    BackupTimingConfig {
        init_retry_delay_ms: 1,
        chunk_timeout_retry_delay_ms: 1,
        chunk_server_retry_delay_ms: 1,
        network_wait_max_ms: 10,
        network_poll_interval_ms: 2,
    }
}

struct MockBackupTransport {
    resp_634: Mutex<VecDeque<Result<Value, BackupRpcError>>>,
    resp_632: Mutex<VecDeque<Result<Value, BackupRpcError>>>,
    resp_631: Mutex<VecDeque<Result<Value, BackupRpcError>>>,
    resp_630: Mutex<VecDeque<Result<Value, BackupRpcError>>>,
    resp_633: Mutex<VecDeque<Result<Value, BackupRpcError>>>,
    upload_results: Mutex<VecDeque<Result<(), UploadChunkError>>>,
    uploaded_chunks: Mutex<Vec<(u32, u32, bool, usize)>>,
    calls_634: AtomicU32,
    calls_632: AtomicU32,
    calls_631: AtomicU32,
    calls_630: AtomicU32,
    calls_633: AtomicU32,
    online: AtomicBool,
    /// Optional flag to flip `online` to true after first poll.
    flip_online_on_check: AtomicBool,
}

impl Default for MockBackupTransport {
    fn default() -> Self {
        Self {
            resp_634: Mutex::new(VecDeque::new()),
            resp_632: Mutex::new(VecDeque::new()),
            resp_631: Mutex::new(VecDeque::new()),
            resp_630: Mutex::new(VecDeque::new()),
            resp_633: Mutex::new(VecDeque::new()),
            upload_results: Mutex::new(VecDeque::new()),
            uploaded_chunks: Mutex::new(Vec::new()),
            calls_634: AtomicU32::new(0),
            calls_632: AtomicU32::new(0),
            calls_631: AtomicU32::new(0),
            calls_630: AtomicU32::new(0),
            calls_633: AtomicU32::new(0),
            online: AtomicBool::new(true),
            flip_online_on_check: AtomicBool::new(false),
        }
    }
}

impl BackupTransport for MockBackupTransport {
    async fn send_634_get_configs(&self, _req_id: &str) -> Result<Value, BackupRpcError> {
        self.calls_634.fetch_add(1, Ordering::SeqCst);
        self.resp_634
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(json!({ "error_code": 0, "data": { "configs": { "retention_time": 0 } } })))
    }

    async fn send_632_get_metadata(&self, _req_id: &str) -> Result<Value, BackupRpcError> {
        self.calls_632.fetch_add(1, Ordering::SeqCst);
        self.resp_632
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(json!({ "error_code": 1001, "data": null })))
    }

    async fn send_631_create_session(&self, _req_id: &str) -> Result<Value, BackupRpcError> {
        self.calls_631.fetch_add(1, Ordering::SeqCst);
        self.resp_631
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(BackupRpcError::Incomplete("no mock 631".into())))
    }

    async fn send_630_init_session(
        &self,
        _req_id: &str,
        _meta: &crate::protocol::BackupInitMetadata,
        _bkid: &str,
        _client_salt: &str,
    ) -> Result<Value, BackupRpcError> {
        self.calls_630.fetch_add(1, Ordering::SeqCst);
        self.resp_630
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(BackupRpcError::Incomplete("no mock 630".into())))
    }

    async fn send_633_signal_restore(&self, _req_id: &str) -> Result<Value, BackupRpcError> {
        self.calls_633.fetch_add(1, Ordering::SeqCst);
        self.resp_633
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(json!({ "error_code": 0, "data": { "err": 0 } })))
    }

    async fn upload_chunk<'a>(&'a self, req: &'a UploadChunkRequest<'a>) -> Result<(), UploadChunkError> {
        self.uploaded_chunks
            .lock()
            .unwrap()
            .push((req.chunk_id, req.total_chunks, req.is_last, req.bytes.len()));
        self.upload_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }

    fn is_online(&self) -> bool {
        let cur = self.online.load(Ordering::SeqCst);
        if !cur && self.flip_online_on_check.swap(false, Ordering::SeqCst) {
            self.online.store(true, Ordering::SeqCst);
        }
        cur
    }
}

#[test]
fn step1_config_defaults_sentinel_and_server_overrides() {
    let def = BackupRemoteConfig::default();
    assert!(!def.enabled);
    assert_eq!(def.max_request, 6);
    assert_eq!(def.fixed_time, 60);
    assert_eq!(def.restrict_time, 120);
    assert_eq!(def.create_backup_waiting_time, 600);
    assert_eq!(def.max_file_size, 0);
    assert_eq!(def.chunk_size, 3_145_728);
    assert_eq!(def.backup_progress_weight, 90);
    assert_eq!(def.after_success_auto_hide_banner_seconds, 300);
    assert_eq!(def.backup_done_timeout_seconds, 180);
    assert_eq!(def.auto_resume_window_seconds, 300);
    assert_eq!(def.auto_resume_max_attempts, 3);
    assert_eq!(def.auto_resume_backoff_ms, vec![2_000, 8_000, 30_000]);
    assert_eq!(def.backup_ttl_ms, 259_200_000);
    assert!(def.encryption_enabled);
    assert_eq!(def.backup_configs_cache_ttl_ms, -1);
    assert_eq!(def.crypto_version, 1);
    assert_eq!(def.db_format_version, 1);

    // `-1` sentinel is preserved, never multiplied by 1000!
    assert_eq!(seconds_to_ms_with_sentinel(-1), -1);
    assert_eq!(seconds_to_ms_with_sentinel(300), 300_000);

    // Default TTL `-1` means 634 cache is ALWAYS stale!
    assert!(!def.is_server_configs_fresh(Some(1_000), 1_001));

    let server = json!({
        "setttings": {
            "features": {
                "backup_msg": {
                    "enabled": true,
                    "chunk_size": 1048576,
                    "after_success_auto_hide_banner_seconds": -1,
                    "backup_configs_cache_ttl_ms": 60000
                }
            }
        }
    });
    let parsed = BackupRemoteConfig::from_server_info(&server);
    assert!(parsed.enabled);
    assert_eq!(parsed.chunk_size, 1_048_576);
    assert_eq!(parsed.after_success_auto_hide_ms(), -1);
    assert!(parsed.is_server_configs_fresh(Some(1_000), 50_000));
    assert!(!parsed.is_server_configs_fresh(Some(1_000), 70_000));

    // Positive retention_time from 634 overrides backup_ttl_ms.
    let sc = ServerBackupConfigs {
        retention_time_ms: 604_800_000,
        raw: None,
    };
    assert_eq!(parsed.effective_backup_ttl_ms(Some(&sc)), 604_800_000);

    // Layering PC getServerInfo -> Web getServerInfo (no `backup_msg`) -> getLoginInfo settings.
    let mut layered = BackupRemoteConfig::default();
    layered.apply_server_info(&server);
    layered.apply_server_info(&json!({
        "data": { "setttings": { "features": {} } }
    }));
    assert!(layered.enabled);
    layered.apply_server_info(&json!({
        "features": {
            "backup_msg": { "enabled": false, "max_request": 10 }
        }
    }));
    assert!(!layered.enabled);
    assert_eq!(layered.max_request, 10);
}

#[tokio::test]
async fn step2_634_and_632_mapping_and_local_ttl_expiry() {
    let cfg = BackupRemoteConfig::default();
    let tr = MockBackupTransport::default();

    // Case A: 634 returns 1-day retention, 632 returns 1001 -> NONE + qos_ok = true.
    tr.resp_634
        .lock()
        .unwrap()
        .push_back(Ok(json!({ "error_code": 0, "data": { "configs": { "retention_time": 86_400_000 } } })));
    tr.resp_632
        .lock()
        .unwrap()
        .push_back(Ok(json!({ "error_code": 1001, "data": null })));

    let mut state = BackupSessionState::default();
    let status = fetch_configs_and_status(&tr, &mut state, &cfg, 1_000_000, "r1", "r2").await;
    assert_eq!(
        status,
        BackupStatus632::None {
            qos_ok: true,
            error_code: 1001
        }
    );
    assert_eq!(
        state.server_configs.as_ref().unwrap().retention_time_ms,
        86_400_000
    );

    // Case B: 632 server error 500 -> NONE + qos_ok = false (never throws!).
    let st_err = parse_632_status(
        Some(&json!({ "error_code": 500, "data": null })),
        1_000_000,
        &cfg,
        state.server_configs.as_ref(),
    );
    assert_eq!(
        st_err,
        BackupStatus632::None {
            qos_ok: false,
            error_code: 500
        }
    );

    // Case C: 632 EXISTING with created_time older than retention -> is_locally_expired = true!
    let existing = parse_632_status(
        Some(&json!({
            "error_code": 0,
            "data": {
                "reqId": "r2",
                "metadata": {
                    "bkid": "bk_old",
                    "pc_device_name": "Linux-PC",
                    "size": 4096,
                    "created_time": 10_000,
                    "expired_time": 0
                }
            }
        })),
        100_000_000,
        &cfg,
        state.server_configs.as_ref(),
    );
    let BackupStatus632::Existing(info) = existing else {
        panic!("expected Existing");
    };
    assert_eq!(info.bkid, "bk_old");
    assert_eq!(info.ttl_ms, 100_000_000 + 86_400_000);
    assert!(info.is_locally_expired(
        100_000_000,
        cfg.effective_backup_ttl_ms(state.server_configs.as_ref())
    ));
    assert!(!info.is_locally_expired(
        20_000,
        cfg.effective_backup_ttl_ms(state.server_configs.as_ref())
    ));

    // Also verify parse_634_configs returns None on error.
    assert!(parse_634_configs(&json!({ "error_code": 1, "data": {} })).is_none());
}

#[test]
fn step3_spam_7th_locks_120s_and_5_step_confirm_guard_order() {
    let cfg = BackupRemoteConfig::default(); // max_request=6, fixed=60s, restrict=120s, cooldown=600s
    let mut spam = SpamRuleTracker::default();

    // 1. Running blocks without recording a request.
    assert_eq!(
        evaluate_confirm_new_backup(
            true,
            false,
            true,
            ConfirmSourcePopup::Intro,
            None,
            10_000,
            &cfg,
            &mut spam
        ),
        ConfirmNewBackupDecision::IgnoreConcurrent
    );
    assert!(spam.request_timestamps_ms.is_empty());

    // 2. Cooldown blocks with toast without recording a request.
    assert!(check_cooldown(Some(100_000), 200_000, &cfg).is_some());
    assert_eq!(
        evaluate_confirm_new_backup(
            false,
            false,
            true,
            ConfirmSourcePopup::ExistingBackup,
            Some(100_000),
            200_000,
            &cfg,
            &mut spam
        ),
        ConfirmNewBackupDecision::CooldownToast {
            remaining_ms: 500_000
        }
    );
    assert!(spam.request_timestamps_ms.is_empty());

    // 4. External busy blocks without recording a request.
    assert_eq!(
        evaluate_confirm_new_backup(
            false,
            true,
            true,
            ConfirmSourcePopup::Intro,
            None,
            10_000,
            &cfg,
            &mut spam
        ),
        ConfirmNewBackupDecision::ExternalBusy
    );
    assert!(spam.request_timestamps_ms.is_empty());

    // 5. Six requests within 60s succeed without locking; 7th trips lock for 120s!
    for i in 0..6u64 {
        assert_eq!(
            evaluate_confirm_new_backup(
                false,
                false,
                true,
                ConfirmSourcePopup::Intro,
                None,
                1_000_000 + i * 1_000,
                &cfg,
                &mut spam
            ),
            ConfirmNewBackupDecision::StartFreshBackup
        );
        assert!(!spam.is_locked(1_000_000 + i * 1_000));
    }
    // 7th request clears timestamps & locks for 120_000 ms.
    let _ = evaluate_confirm_new_backup(
        false,
        false,
        true,
        ConfirmSourcePopup::Intro,
        None,
        1_010_000,
        &cfg,
        &mut spam,
    );
    assert!(spam.request_timestamps_ms.is_empty());
    assert_eq!(spam.locked_until_ms, Some(1_130_000));

    // Subsequent click from ExistingBackup -> SpamToast; from Intro -> SpamPopup!
    assert_eq!(
        evaluate_confirm_new_backup(
            false,
            false,
            true,
            ConfirmSourcePopup::ExistingBackup,
            None,
            1_020_000,
            &cfg,
            &mut spam
        ),
        ConfirmNewBackupDecision::SpamToast {
            remaining_ms: 110_000
        }
    );
    assert_eq!(
        evaluate_confirm_new_backup(
            false,
            false,
            true,
            ConfirmSourcePopup::Intro,
            None,
            1_020_000,
            &cfg,
            &mut spam
        ),
        ConfirmNewBackupDecision::SpamPopup {
            unlock_at_ms: 1_130_000
        }
    );
}

#[test]
fn step4_real_backup_wasm_encrypt_decrypt_roundtrip_and_no_raw_fallback() {
    let key = derive_backup_wasm_key();
    assert_eq!(key.len(), 64);

    let wasm_path = workspace_backup_wasm_path();
    assert!(
        wasm_path.exists(),
        "expected backup wasm at {}",
        wasm_path.display()
    );

    // Empty client_salt MUST be rejected!
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode([0x42u8; 32]);
    let salt_b64 = generate_client_salt();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&salt_b64)
            .unwrap()
            .len(),
        32
    );
    assert!(BackupCryptoSession::start_from_file(&wasm_path, &nonce_b64, "", true).is_err());
    // Empty server_nonce when encryption_enabled == true MUST be rejected!
    assert!(BackupCryptoSession::start_from_file(&wasm_path, "", &salt_b64, true).is_err());

    // Real encrypt -> decrypt roundtrip through libzproto_backup_wasm_bg!
    let plaintext = b"Zalo PC->Phone backup protobuf + zstd block payload!";
    let ciphertext = {
        let mut sess = BackupCryptoSession::start_from_file(&wasm_path, &nonce_b64, &salt_b64, true)
            .expect("start backup crypto session");
        sess.encrypt_block(plaintext).expect("encrypt backup block")
    };
    assert_ne!(ciphertext.as_slice(), plaintext);
    assert!(ciphertext.len() > plaintext.len());

    let decrypted = {
        let mut sess = BackupCryptoSession::start_from_file(&wasm_path, &nonce_b64, &salt_b64, true)
            .expect("restart backup crypto session with same nonce/salt");
        sess.decrypt_block(&ciphertext)
            .expect("decrypt backup block")
    };
    assert_eq!(decrypted.as_slice(), plaintext);

    // Packer MUST refuse empty server_nonce when encryption_enabled == true (NEVER raw fallback!).
    let tmp = std::env::temp_dir().join(format!("zalo_bk_test_nononce_{}", std::process::id()));
    let convs = vec![BackupConversationInput {
        pc_conv_id: "100".into(),
        is_group: false,
    }];
    let noise_map = HashMap::from([("100".to_owned(), "g_100".to_owned())]);
    let res = pack_backup_file(
        &PackParams {
            backup_dir: &tmp,
            bkid: "bk_1",
            server_nonce_b64: "",
            client_salt_b64: &salt_b64,
            encryption_enabled: true,
            my_pc_uid: "1",
            my_global_noised_id: Some("g_me"),
            conversations: &convs,
            noise_map: &noise_map,
        },
        Some(&wasm_path),
        |_| vec![],
        |_| {},
    );
    assert!(matches!(
        res,
        Err(PackerError::MissingEncryptionMaterial(_))
    ));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn step5_scopes_filter_outage_vs_partial_and_full_packed_file_roundtrip() {
    let convs = vec![
        BackupConversationInput {
            pc_conv_id: "u1".into(),
            is_group: false,
        },
        BackupConversationInput {
            pc_conv_id: "g_2".into(),
            is_group: true,
        },
        BackupConversationInput {
            pc_conv_id: "u3_missing".into(),
            is_group: false,
        },
    ];
    let scopes = build_message_scopes(&convs);
    assert_eq!(scopes.len(), 3);
    assert_eq!(scopes[0].limit, MESSAGE_SCOPE_LIMIT);

    // Outage (0 resolved) -> keep ALL 3 scopes + lookup_outage = true!
    let outage = drop_unnoisable_scopes(scopes.clone(), &HashMap::new());
    assert!(outage.lookup_outage);
    assert_eq!(outage.scopes.len(), 3);
    assert_eq!(outage.dropped_count, 0);

    // Partial resolution -> drop u3_missing!
    let noise_map = HashMap::from([
        ("u1".to_owned(), "global_u1".to_owned()),
        ("g_2".to_owned(), "global_g2".to_owned()),
    ]);
    let partial = drop_unnoisable_scopes(scopes, &noise_map);
    assert!(!partial.lookup_outage);
    assert_eq!(partial.scopes.len(), 2);
    assert_eq!(partial.dropped_count, 1);

    // End-to-end pack with REAL backup WASM + zstd + [u32 LE] framing + SHA-256!
    let wasm_path = workspace_backup_wasm_path();
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode([0x55u8; 32]);
    let salt_b64 = generate_client_salt();
    let tmp = std::env::temp_dir().join(format!("zalo_bk_test_pack_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);

    let mut worker_pcts = Vec::new();
    let pack = pack_backup_file(
        &PackParams {
            backup_dir: &tmp,
            bkid: "bk_pack_42",
            server_nonce_b64: &nonce_b64,
            client_salt_b64: &salt_b64,
            encryption_enabled: true,
            my_pc_uid: "my_uid",
            my_global_noised_id: Some("global_me"),
            conversations: &convs,
            noise_map: &noise_map,
        },
        Some(&wasm_path),
        |scope| {
            vec![LocalBackupMessage {
                sender_pc_id: "0".into(),
                client_id: 777,
                pc_status: 1,
                pc_msg_type: Some(1),
                msg_type_str: "webchat".into(),
                timestamp_ms: 1_700_000_000_000,
                global_id: "999888".into(),
                content: format!("hello {}", scope.pc_conv_id),
                attach: Some(BackupAttach {
                    title: "att".into(),
                    ..Default::default()
                }),
                mentions: vec![BackupMention {
                    uid: "u1".into(),
                    pos: 0,
                    len: 2,
                    mention_type: 0,
                }],
                quote: Some(BackupQuote {
                    owner_id: "0".into(),
                    msg: "quoted".into(),
                    ..Default::default()
                }),
                ..Default::default()
            }]
        },
        |pct| worker_pcts.push(pct),
    )
    .expect("pack backup file");

    assert_eq!(pack.scope_count, 2);
    assert_eq!(pack.item_count, 2);
    assert_eq!(pack.dropped_scopes, 1);
    assert!(!pack.lookup_outage);
    assert_eq!(pack.checksum.len(), 64);
    assert!(pack.file_path.ends_with("0"));
    // Worker percentages: 1/2 -> 50%, 2/2 -> min(99, 100)=99%, final -> 100%.
    assert_eq!(worker_pcts, vec![50, 99, 100]);

    // Verify file on disk uses [u32 LE][ciphertext] and decrypts + zstd-decompresses cleanly!
    let raw = std::fs::read(&pack.file_path).unwrap();
    let blocks = read_framed_blocks(&raw).unwrap();
    assert_eq!(blocks.len(), 2);
    let mut dec_sess =
        BackupCryptoSession::start_from_file(&wasm_path, &nonce_b64, &salt_b64, true).unwrap();
    for (idx, blk) in blocks.iter().enumerate() {
        let compressed = dec_sess.decrypt_block(blk).unwrap();
        let proto_bytes = zstd::bulk::decompress(&compressed, 1024 * 1024).unwrap();
        // Verify partition wire strings exist in protobuf payload.
        let expected_part = if idx == 0 {
            b"oneone/global_u1".as_slice()
        } else {
            b"group/global_g2".as_slice()
        };
        assert!(
            proto_bytes
                .windows(expected_part.len())
                .any(|w| w == expected_part),
            "expected partition {:?} in decoded protobuf",
            String::from_utf8_lossy(expected_part)
        );
        // Verify senderId "0" was noised to "global_me".
        assert!(
            proto_bytes.windows(b"global_me".len()).any(|w| w == b"global_me"),
            "expected noised self senderId"
        );
    }

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn step6_631_630_reuse_window_and_error_212_reset() {
    let cfg = BackupRemoteConfig::default();
    let tr = MockBackupTransport::default();
    let mut state = BackupSessionState::default();

    // 631 retries on timeout, succeeds on attempt 2.
    tr.resp_631.lock().unwrap().push_back(Err(BackupRpcError::Transport {
        code: "ETIMEDOUT".into(),
        message: "timeout".into(),
    }));
    tr.resp_631.lock().unwrap().push_back(Ok(json!({
        "error_code": 0,
        "data": { "reqId": "r1", "err": 0, "bkid": "bk_100", "server_nonce": "AAAA" }
    })));

    let (bkid, nonce, salt) =
        ensure_session_before_engine(&tr, &mut state, &cfg, 1_000, fast_timing(), |a| {
            format!("631_{a}")
        })
        .await
        .expect("631 succeeds on retry");
    assert_eq!(bkid, "bk_100");
    assert_eq!(nonce, "AAAA");
    assert!(!salt.is_empty());
    assert_eq!(tr.calls_631.load(Ordering::SeqCst), 2);

    // Calling ensure_session_before_engine again within window reuses existing 631 without calling server!
    let (bkid2, _, _) =
        ensure_session_before_engine(&tr, &mut state, &cfg, 2_000, fast_timing(), |a| {
            format!("631_again_{a}")
        })
        .await
        .unwrap();
    assert_eq!(bkid2, "bk_100");
    assert_eq!(tr.calls_631.load(Ordering::SeqCst), 2);

    // Server error on 631 is NEVER retried!
    let tr_err = MockBackupTransport::default();
    tr_err.resp_631.lock().unwrap().push_back(Ok(json!({
        "error_code": 0,
        "data": { "err": 999 }
    })));
    let mut st_err = BackupSessionState::default();
    let err = ensure_session_before_engine(&tr_err, &mut st_err, &cfg, 1_000, fast_timing(), |_| {
        "r".into()
    })
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        BackupPipelineError::Rpc(BackupRpcError::Server { code: 999, .. })
    ));
    assert_eq!(tr_err.calls_631.load(Ordering::SeqCst), 1);

    // Build metadata & call 630 -> succeeds and sets bkid_created_at_ms.
    let pack = crate::packer::PackResult {
        file_path: PathBuf::from("/tmp/dummy_0"),
        file_size: 128,
        original_size: 256,
        item_count: 5,
        scope_count: 2,
        checksum: "a".repeat(64),
        lookup_outage: false,
        dropped_scopes: 0,
    };
    let meta = build_and_validate_metadata(&pack, "LinuxPC", &cfg, &mut state).unwrap();
    let wire_630 = build_init_630_payload("req630", &meta, &bkid, &salt);
    assert_eq!(wire_630["reqId"], "req630");
    assert_eq!(wire_630["data"]["bkid"], "bk_100");
    assert_eq!(wire_630["data"]["min_msg_ts"], 0);

    tr.resp_630.lock().unwrap().push_back(Ok(json!({
        "error_code": 0,
        "data": { "reqId": "req630", "err": 0, "bkid": "bk_100" }
    })));
    ensure_init_630(&tr, &mut state, &meta, 5_000, fast_timing(), |_| "req630".into())
        .await
        .expect("630 succeeds");
    assert_eq!(tr.calls_630.load(Ordering::SeqCst), 1);

    // Within oy (1_740_000 ms), 630 is skipped and bkid reused!
    ensure_init_630(
        &tr,
        &mut state,
        &meta,
        5_000 + UPLOAD_REUSE_WINDOW_MS,
        fast_timing(),
        |_| "req630_skip".into(),
    )
    .await
    .expect("630 skipped within oy");
    assert_eq!(tr.calls_630.load(Ordering::SeqCst), 1);

    // Past oy -> clears keys and returns UploadWindowExpired without calling 630!
    let expired = ensure_init_630(
        &tr,
        &mut state,
        &meta,
        5_000 + UPLOAD_REUSE_WINDOW_MS + 1,
        fast_timing(),
        |_| "req630_exp".into(),
    )
    .await
    .unwrap_err();
    assert_eq!(expired, BackupPipelineError::UploadWindowExpired);
    assert!(state.bkid.is_none());
    assert!(state.server_nonce.is_none());
    assert!(state.client_salt.is_none());

    // Error 212 on 630 clears session keys!
    state.bkid = Some("bk_212".into());
    state.server_nonce = Some("nonce".into());
    state.client_salt = Some("salt".into());
    state.bkid_created_at_ms = None;
    tr.resp_630.lock().unwrap().push_back(Ok(json!({
        "error_code": 0,
        "data": { "err": 212 }
    })));
    let err_212 = ensure_init_630(&tr, &mut state, &meta, 10_000, fast_timing(), |_| "r212".into())
        .await
        .unwrap_err();
    assert!(matches!(
        err_212,
        BackupPipelineError::Rpc(BackupRpcError::Server { code: 212, .. })
    ));
    assert!(state.bkid.is_none());
    assert!(state.client_salt.is_none());

    // scope_count == 0 -> EmptyScopes -> UserSystem banner + clears session keys!
    state.bkid = Some("bk_empty".into());
    let empty_pack = crate::packer::PackResult {
        scope_count: 0,
        item_count: 0,
        ..pack
    };
    let empty_err = build_and_validate_metadata(&empty_pack, "LinuxPC", &cfg, &mut state).unwrap_err();
    assert_eq!(empty_err, BackupPipelineError::EmptyScopes);
    assert_eq!(empty_err.banner_category(), BackupErrorCategory::UserSystem);
    assert!(state.bkid.is_none());

    // Also verify parse_631_response / parse_630_response reject missing fields.
    assert!(parse_631_response(&json!({ "error_code": 0, "data": { "err": 0, "bkid": "x" } })).is_err());
    assert!(parse_630_response(&json!({ "error_code": 0, "data": { "err": 0 } })).is_err());
}

#[tokio::test]
async fn step7_8_9_10_chunked_upload_retries_hy_success_before_633_and_monitor() {
    let tmp = std::env::temp_dir().join(format!("zalo_bk_test_up_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let file_path = tmp.join("0");
    // 250 bytes -> with chunk_size = 100 -> 3 chunks (100, 100, 50).
    let payload = vec![0xABu8; 250];
    std::fs::write(&file_path, &payload).unwrap();
    let (checksum, size) = crate::packer::compute_file_sha256_and_size(&file_path).unwrap();

    let mut cfg = BackupRemoteConfig::default();
    cfg.chunk_size = 100;
    cfg.backup_progress_weight = 90;

    let mut state = BackupSessionState {
        bkid: Some("bk_up_1".into()),
        server_nonce: Some("nonce".into()),
        client_salt: Some("salt".into()),
        bkid_created_at_ms: Some(1_000),
        last_uploaded_chunk_id: 0,
        file_path: Some(file_path.clone()),
        metadata: Some(crate::protocol::BackupInitMetadata {
            pc_device_name: "Linux".into(),
            min_msg_ts: 0,
            size,
            original_size: size,
            msg_count: 10,
            thread_count: 2,
            crypto_version: 1,
            db_format_version: 1,
            checksum,
        }),
        ..Default::default()
    };

    let tr = MockBackupTransport::default();
    // Chunk 1: attempt 1 -> timeout, attempt 2 -> server 112, attempt 3 -> Ok!
    // Chunk 2: attempt 1 -> network error (recovers on poll), attempt 2 -> Ok!
    // Chunk 3: attempt 1 -> Ok!
    {
        let mut q = tr.upload_results.lock().unwrap();
        q.push_back(Err(UploadChunkError::classify(
            None,
            "ECONNABORTED",
            "timeout",
            false,
        )));
        q.push_back(Err(UploadChunkError::classify(
            Some(112),
            "",
            "transient",
            false,
        )));
        q.push_back(Ok(()));
        q.push_back(Err(UploadChunkError::classify(
            None,
            "ERR_NO_NETWORK",
            "offline",
            true,
        )));
        q.push_back(Ok(()));
        q.push_back(Ok(()));
    }
    tr.online.store(false, Ordering::SeqCst);
    tr.flip_online_on_check.store(true, Ordering::SeqCst);

    let mut ui_pcts = Vec::new();
    let outcome = upload_backup_file_chunked(&tr, &mut state, &cfg, fast_timing(), |p| {
        ui_pcts.push(p)
    })
    .await
    .expect("upload succeeds after retries + hy verify");

    assert_eq!(outcome.bkid, "bk_up_1");
    assert_eq!(outcome.total_chunks, 3);
    assert_eq!(state.last_uploaded_chunk_id, 3);
    // CRITICAL: 633 MUST NOT be called during upload!
    assert_eq!(tr.calls_633.load(Ordering::SeqCst), 0);

    // Check exact UI progress formula (weight = 90):
    // chunk 1/3 -> 33% -> 90 + floor(33*10/100) = 93
    // chunk 2/3 -> 66% -> 90 + floor(66*10/100) = 96
    // chunk 3/3 -> 100% -> 90 + 10 = 100
    assert_eq!(ui_pcts, vec![93, 96, 100]);
    assert_eq!(compute_ui_pack_percent(50, 90), 45);
    assert_eq!(compute_ui_pack_percent(100, 90), 90);
    assert_eq!(compute_worker_progress_percent(1, 2), 50);
    assert_eq!(compute_worker_progress_percent(2, 2), 99);
    assert_eq!(compute_ui_upload_percent(1, 3, 90), 93);

    // QoS-only backup_done monitor: matches by bkid, ignores other bkIds, times out at 180s.
    let mut mon = BackupDoneMonitor::arm("bk_up_1".into(), 10_000, &cfg);
    assert!(!mon.on_backup_done("other_bkid", 0));
    assert_eq!(mon.status, BackupDoneMonitorStatus::Pending);
    assert!(mon.on_backup_done("bk_up_1", 0));
    assert_eq!(mon.status, BackupDoneMonitorStatus::MatchedOk);

    let mut mon_to = BackupDoneMonitor::arm("bk_up_2".into(), 10_000, &cfg);
    assert!(!mon_to.check_timeout(189_999));
    assert!(mon_to.check_timeout(190_000));
    assert_eq!(mon_to.status, BackupDoneMonitorStatus::MonitorTimeout);

    // Control event parser for `backup_pc2mb`.
    let ctrls = parse_backup_controls(&[
        json!({ "content": { "act_type": "backup_pc2mb", "act": "backup_done", "data": "{\"bkid\":\"bk_up_1\",\"err\":0}" } }),
        json!({ "content": { "act_type": "backup_pc2mb", "act": "mb_start_restore", "data": { "bkid": "bk_up_1" } } }),
        json!({ "content": { "act_type": "transfer_sync2", "act": "upload_batch", "data": {} } }),
    ]);
    assert_eq!(
        ctrls,
        vec![
            BackupControlEvent::BackupDone {
                bkid: "bk_up_1".into(),
                err: 0
            },
            BackupControlEvent::MobileStartRestore {
                bkid: "bk_up_1".into()
            },
        ]
    );

    // 633 ONLY called via user action, and ALWAYS swallows errors!
    tr.resp_633
        .lock()
        .unwrap()
        .push_back(Ok(json!({ "error_code": 0, "data": { "err": 500 } })));
    let ok = trigger_mobile_restore_swallow_errors(&tr, "req_633").await;
    assert!(!ok);
    assert_eq!(tr.calls_633.load(Ordering::SeqCst), 1);

    // Auto-resume step and backoff checks.
    let intr = BackupInterruptState {
        bkid: Some("bk_1".into()),
        file_path: None,
        interrupted_at_ms: 10_000,
        attempts: 0,
        last_uploaded_chunk_id: 1,
    };
    assert_eq!(get_resume_step_for(&intr), ResumeStep::Upload);
    assert_eq!(next_auto_resume_delay_ms(&intr, 20_000, &cfg), Some(2_000));
    let intr_engine = BackupInterruptState {
        bkid: None,
        file_path: None,
        interrupted_at_ms: 10_000,
        attempts: 2,
        last_uploaded_chunk_id: 0,
    };
    assert_eq!(get_resume_step_for(&intr_engine), ResumeStep::Engine);
    assert_eq!(
        next_auto_resume_delay_ms(&intr_engine, 20_000, &cfg),
        Some(30_000)
    );
    // Past 300s window -> None.
    assert_eq!(
        next_auto_resume_delay_ms(&intr_engine, 320_000, &cfg),
        None
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
