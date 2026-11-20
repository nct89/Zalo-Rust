use std::collections::HashSet;
use std::path::Path;

use serde_json::json;

use crate::batch;
use crate::keys::KeyPair;
use crate::message::{convert_chunk, pc_msg_type};
use crate::proto::{Decode, MessageChunk};
use crate::request::{parse_control, BatchType, ControlEvent, Database, MobileStatus, Query};
use crate::session::{Decoded, SyncSession, Update};
use crate::wasm::{find_wasm, ZprotoWasm};
use crate::Sync2Error;

// Minimal protobuf writer, standing in for the phone.
fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn field_varint(out: &mut Vec<u8>, n: u32, v: u64) {
    varint(out, u64::from(n) << 3);
    varint(out, v);
}
fn field_bytes(out: &mut Vec<u8>, n: u32, b: &[u8]) {
    varint(out, (u64::from(n) << 3) | 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn message(
    sender: &str,
    cli: u64,
    ty: u64,
    ts: u64,
    gid: u64,
    text: &str,
    attach_title: Option<&str>,
) -> Vec<u8> {
    let mut m = Vec::new();
    field_bytes(&mut m, 1, sender.as_bytes());
    field_varint(&mut m, 2, cli);
    field_varint(&mut m, 4, ty);
    field_varint(&mut m, 5, ts);
    field_varint(&mut m, 6, gid);
    field_bytes(&mut m, 7, text.as_bytes());
    if let Some(title) = attach_title {
        let mut attach = Vec::new();
        field_bytes(&mut attach, 5, title.as_bytes());
        field_bytes(&mut attach, 6, b"https://example.invalid/p.jpg");
        let mut meta = Vec::new();
        field_bytes(&mut meta, 1, &attach);
        field_varint(&mut meta, 6, 0);
        field_bytes(&mut m, 8, &meta);
    }
    m
}

fn chunk(partition: &str, messages: &[Vec<u8>]) -> Vec<u8> {
    let mut p = Vec::new();
    field_bytes(&mut p, 1, partition.as_bytes());
    for m in messages {
        field_bytes(&mut p, 2, m);
    }
    let mut c = Vec::new();
    field_varint(&mut c, 1, 1);
    field_bytes(&mut c, 2, &p);
    c
}

fn wasm() -> Option<ZprotoWasm> {
    let Some(path) = find_wasm(Path::new(env!("CARGO_MANIFEST_DIR"))) else {
        eprintln!("libzproto_wasm_bg.*.wasm not found; set ZALO_SYNC2_WASM to run this test");
        return None;
    };
    Some(ZprotoWasm::from_file(&path).expect("load wasm"))
}

#[test]
fn frames_round_trip() {
    let file = batch::join_frames([b"abc".as_slice(), b"".as_slice(), b"defg".as_slice()]);
    let frames = batch::frames(&file).unwrap();
    assert_eq!(frames, vec![b"abc".as_slice(), b"", b"defg"]);
    assert!(matches!(
        batch::frames(&file[..file.len() - 1]),
        Err(Sync2Error::Format(_))
    ));
}

#[test]
fn proto_decodes_messages() {
    let bytes = chunk(
        "group/77",
        &[message(
            "11",
            5,
            3,
            1_700_000_000_000,
            9001,
            "",
            Some("photo"),
        )],
    );
    let decoded = MessageChunk::decode(&bytes).unwrap();
    assert_eq!(decoded.platform, 1);
    let m = &decoded.partitions[0].messages[0];
    assert_eq!(decoded.partitions[0].id, "group/77");
    assert_eq!(
        (m.sender_id.as_str(), m.client_id, m.msg_type, m.global_id),
        ("11", 5, 3, 9001)
    );
    assert_eq!(m.meta.as_ref().unwrap().attachs[0].title, "photo");
}

#[test]
fn control_events_parse() {
    let batch = json!({
        "act_type": "transfer_sync2", "act": "upload_batch",
        "data": {"syncId": "S", "idx": 0, "isLast": true, "err": 0, "batchType": 2,
                 "msgUrl": "https://f/1", "scopes": [{"partition": "oneone/5", "from": 1, "to": 2}]}
    });
    let Some(ControlEvent::Batch(info)) = parse_control(&batch) else {
        panic!("not a batch")
    };
    assert_eq!(
        (info.idx, info.is_last, info.batch_type),
        (0, true, BatchType::MetadataEmbedded)
    );
    assert_eq!(info.scopes[0].partition, "oneone/5");

    let status = json!({"act_type": "transfer_sync2", "act": "transfer_status",
                        "data": "{\"syncId\":\"S\",\"status\":3,\"fromDevice\":0}"});
    assert_eq!(
        parse_control(&status),
        Some(ControlEvent::Status {
            sync_id: "S".into(),
            status: MobileStatus::WaitingConfirm
        })
    );
    let pc_status = json!({"act_type": "transfer_sync2", "act": "transfer_status",
                           "data": {"syncId": "S", "status": 1, "fromDevice": 2}});
    assert_eq!(parse_control(&pc_status), None);
    assert_eq!(
        parse_control(&json!({"act_type": "group", "act": "x", "data": {}})),
        None
    );
}

#[test]
fn msg_type_table() {
    assert_eq!(pc_msg_type(0), Some("webchat"));
    assert_eq!(pc_msg_type(22), Some("share.file"));
    assert_eq!(pc_msg_type(36), None);
}

#[test]
fn keys_match_signal_format() {
    let Some(mut wasm) = wasm() else { return };
    let pair = KeyPair::generate(&mut wasm).unwrap();
    assert_eq!(pair.private.len(), 32);
    assert_eq!(pair.public.len(), 33);
    assert_eq!(pair.public[0], 5);
    // Same private key, same public key.
    assert_eq!(wasm.derive_public_key(&pair.private).unwrap(), pair.public);
}

#[test]
fn session_decodes_simulated_phone_batches() {
    let Some(mut wasm) = wasm() else { return };
    let identity = KeyPair::generate(&mut wasm).unwrap();
    let mut session = SyncSession::new(&mut wasm, identity, Database::Message).unwrap();
    let body = session
        .request(
            vec![Query::messages("g500", true, 0, 2_000_000_000_000)],
            "",
            "linux",
            0,
        )
        .body();
    assert_eq!(body["data"]["req"]["type"], "message");
    assert_eq!(body["data"]["req"]["queries"][0]["partition"], "group/g500");
    assert_eq!(
        body["data"]["ik"].as_str().unwrap(),
        session.keys().identity.public_base64()
    );

    // Phone side: its own identity, encrypting to the PC's ik/ek.
    let phone = KeyPair::generate(&mut wasm).unwrap();
    let first = chunk(
        "group/g500",
        &[
            message("u1", 101, 0, 1_600_000_000_000, 7001, "xin chào", None),
            message("me", 102, 3, 1_600_000_001_000, 7002, "", Some("ảnh")),
        ],
    );
    let meta = wasm
        .create_metadata_cipher(
            &zstd::encode_all(first.as_slice(), 3).unwrap(),
            &phone.public,
            &phone.private,
            &session.keys().identity.public,
            &session.keys().ephemeral.public,
        )
        .unwrap();
    let second = chunk(
        "group/g500",
        &[
            message("u1", 101, 0, 1_600_000_000_000, 7001, "xin chào", None),
            message("u1", 103, 36, 1_600_000_002_000, 7003, "", None),
            message("u1", 104, 0, 1_600_000_003_000, 7004, "tin cuối", None),
        ],
    );
    let msg_frame = wasm
        .encrypt_message(
            &zstd::encode_all(second.as_slice(), 3).unwrap(),
            &meta.session_record,
        )
        .unwrap();

    let announce = |idx: u32, is_last: bool, batch_type: u8, sync_id: &str| {
        parse_control(&json!({"act_type": "transfer_sync2", "act": "upload_batch",
            "data": {"syncId": sync_id, "idx": idx, "isLast": is_last, "err": 0,
                     "batchType": batch_type, "msgUrl": format!("https://f/{idx}"), "scopes": []}}))
        .unwrap()
    };
    let sync_id = session.sync_id().to_owned();
    let Update::Download(info1) = session.on_control(&announce(1, true, 1, &sync_id)) else {
        panic!()
    };
    let Update::Download(info0) = session.on_control(&announce(0, false, 2, &sync_id)) else {
        panic!()
    };
    assert_eq!(
        session.on_control(&announce(0, false, 2, &sync_id)),
        Update::Duplicate
    );
    assert_eq!(
        session.on_control(&announce(0, false, 2, "other")),
        Update::Ignored
    );
    assert!(session.is_complete());

    let file0 = batch::join_frames([meta.ciphertext.as_slice()]);
    let file1 = batch::join_frames([msg_frame.as_slice()]);
    // A message batch before the metadata batch has no session record yet.
    assert!(matches!(
        session.decode(&mut wasm, &info1, &file1),
        Err(Sync2Error::NoSessionRecord)
    ));

    let ids = |gid: &str, _group: bool| Some(format!("pc-{gid}"));
    let mut seen = HashSet::new();
    let mut all = Vec::new();
    for (info, file) in [(&info0, &file0), (&info1, &file1)] {
        let Decoded::Messages(chunks) = session.decode(&mut wasm, info, file).unwrap() else {
            panic!()
        };
        for c in &chunks {
            all.extend(convert_chunk(c, &ids, "pc-me", &mut seen));
        }
    }
    let gids: Vec<i64> = all.iter().map(|m| m.global_msg_id).collect();
    assert_eq!(
        gids,
        vec![7001, 7002, 7004],
        "duplicate 7001 and undo 7003 are dropped"
    );
    assert_eq!(all[0].thread_id, "pc-g500");
    assert_eq!(all[0].body, "xin chào");
    assert!(all[1].from_me);
    assert_eq!(
        (all[1].msg_type, all[1].body.as_str()),
        ("chat.photo", "ảnh")
    );
    assert_eq!(all[2].to_server_json()["msgId"], "7004");

    // Wrong keys fail with the wasm's error object.
    let other = KeyPair::generate(&mut wasm).unwrap();
    let err = wasm
        .decrypt_metadata(
            &meta.ciphertext,
            &other.public,
            &other.private,
            &other.private,
        )
        .unwrap_err();
    assert!(
        matches!(err, Sync2Error::Crypto { ref code, .. } if code == "DecryptionError"),
        "{err}"
    );
}
