//! Safe `wasmi` host for Electron's Sync 2 crypto module
//! (`libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm`, webpack module `Vox7`).
//!
//! This module exclusively loads the Sync 2 WASM binary (phone → PC).
//! It never loads or calls the PC → phone backup WASM module.

use std::collections::HashMap;
use std::path::Path;

use rand::RngCore;
use thiserror::Error;
use wasmi::{Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

/// Exact Electron Sync 2 WASM filename.
pub const SYNC_WASM_FILENAME: &str = "libzproto_wasm_bg.ecc0555ba9d0f8867a4699ce943ec745.wasm";

/// Errors raised while instantiating or calling the Sync 2 WASM module.
#[derive(Debug, Error)]
pub enum SyncWasmError {
    /// Failed to read the `.wasm` file from disk.
    #[error("failed to read Sync 2 wasm file: {0}")]
    Io(#[from] std::io::Error),
    /// `wasmi` compilation, linking, or execution error.
    #[error("wasmi error: {0}")]
    Wasmi(String),
    /// The WASM module threw a JS exception via `__wbindgen_throw` or returned an error slot.
    #[error("wasm exception: {0}")]
    Exception(String),
    /// Expected export or return value shape was missing or invalid.
    #[error("wasm protocol error: {0}")]
    Protocol(String),
}

/// Ed25519 / X25519 sync key pair emitted by `zprotoEd25519GenerateKeyPair`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPair {
    /// Raw public key bytes as emitted by `libzproto_wasm_bg`.
    pub public_key: Vec<u8>,
    /// Raw private key bytes as emitted by `libzproto_wasm_bg`.
    pub private_key: Vec<u8>,
}

/// Decrypted metadata batch (`zprotoSync2DecryptMetadata`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecryptedMetadata {
    /// Session identifier string.
    pub session_id: String,
    /// Decrypted (still zstd-compressed) payload bytes.
    pub plaintext: Vec<u8>,
    /// Sender identity public key bytes.
    pub sender_identity: Vec<u8>,
    /// Session record bytes required to decrypt subsequent message batches.
    pub session_record: Vec<u8>,
}

/// Encrypted metadata batch (`zprotoSync2CreateMetadataCipher`, used in roundtrip tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedMetadata {
    /// Session identifier string.
    pub session_id: String,
    /// Encrypted metadata ciphertext bytes.
    pub ciphertext: Vec<u8>,
    /// Receiver identity public key bytes.
    pub receiver_identity: Vec<u8>,
    /// Session record bytes used to encrypt subsequent message batches.
    pub session_record: Vec<u8>,
}

/// Decrypted message batch (`zprotoSync2DecryptMessage`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecryptedMessage {
    /// Session identifier string.
    pub session_id: String,
    /// Decrypted (still zstd-compressed) payload bytes.
    pub plaintext: Vec<u8>,
}

/// Encrypted message batch (`zprotoSync2EncryptMessage`, used in roundtrip tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedMessage {
    /// Session identifier string.
    pub session_id: String,
    /// Encrypted message ciphertext bytes.
    pub ciphertext: Vec<u8>,
}

#[derive(Clone, Debug)]
enum JsVal {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Bytes(Vec<u8>),
    Memory,
    MemoryView { offset: u32, len: u32 },
    Object(HashMap<String, JsVal>),
    Array(Vec<JsVal>),
    GlobalThis,
    Crypto,
    Error(String),
    Function(String),
}

struct HostState {
    heap: Vec<Option<JsVal>>,
    free_list: Vec<u32>,
    memory: Option<Memory>,
    last_exn: Option<String>,
}

impl HostState {
    fn new() -> Self {
        // Mirror wasm-bindgen's reserved slots: 0..128 undefined, 128=undefined, 129=null, 130=true, 131=false.
        let mut heap = vec![Some(JsVal::Undefined); 128];
        heap.push(Some(JsVal::Undefined)); // 128
        heap.push(Some(JsVal::Null)); // 129
        heap.push(Some(JsVal::Bool(true))); // 130
        heap.push(Some(JsVal::Bool(false))); // 131
        Self {
            heap,
            free_list: Vec::new(),
            memory: None,
            last_exn: None,
        }
    }

    fn alloc(&mut self, val: JsVal) -> u32 {
        if let Some(idx) = self.free_list.pop() {
            self.heap[idx as usize] = Some(val);
            idx
        } else {
            let idx = self.heap.len() as u32;
            self.heap.push(Some(val));
            idx
        }
    }

    fn get(&self, idx: u32) -> &JsVal {
        self.heap
            .get(idx as usize)
            .and_then(Option::as_ref)
            .unwrap_or(&JsVal::Undefined)
    }

    fn get_mut(&mut self, idx: u32) -> Option<&mut JsVal> {
        self.heap.get_mut(idx as usize).and_then(Option::as_mut)
    }

    fn take(&mut self, idx: u32) -> JsVal {
        if idx < 132 {
            return self.get(idx).clone();
        }
        let val = self
            .heap
            .get_mut(idx as usize)
            .and_then(Option::take)
            .unwrap_or(JsVal::Undefined);
        self.free_list.push(idx);
        val
    }

    fn describe_val(val: &JsVal) -> String {
        match val {
            JsVal::String(s) | JsVal::Error(s) => s.clone(),
            JsVal::Number(n) => n.to_string(),
            JsVal::Bool(b) => b.to_string(),
            JsVal::Null => "null".to_owned(),
            JsVal::Undefined => "undefined".to_owned(),
            JsVal::Object(map) => {
                if let Some(msg) = map.get("message") {
                    let code = map
                        .get("code")
                        .map(Self::describe_val)
                        .unwrap_or_default();
                    if code.is_empty() {
                        Self::describe_val(msg)
                    } else {
                        format!("[{code}] {}", Self::describe_val(msg))
                    }
                } else {
                    format!("{map:?}")
                }
            }
            other => format!("{other:?}"),
        }
    }
}

/// Live instance of Electron's Sync 2 WASM module (`libzproto_wasm_bg`).
pub struct SyncWasm {
    store: Store<HostState>,
    memory: Memory,
    add_to_stack_pointer: TypedFunc<i32, i32>,
    malloc: TypedFunc<(i32, i32), i32>,
    free_mem: TypedFunc<(i32, i32, i32), ()>,
    gen_keypair_fn: TypedFunc<i32, ()>,
    keypair_pub_fn: TypedFunc<i32, i32>,
    keypair_priv_fn: TypedFunc<i32, i32>,
    keypair_free_fn: TypedFunc<(i32, i32), ()>,
    create_meta_fn: TypedFunc<(i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32), ()>,
    decrypt_meta_fn: TypedFunc<(i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32), ()>,
    meta_session_id_fn: TypedFunc<i32, i32>,
    meta_plaintext_fn: TypedFunc<i32, i32>,
    meta_sender_id_fn: TypedFunc<i32, i32>,
    meta_session_rec_fn: TypedFunc<i32, i32>,
    meta_dec_free_fn: TypedFunc<(i32, i32), ()>,
    meta_enc_free_fn: TypedFunc<(i32, i32), ()>,
    encrypt_msg_fn: TypedFunc<(i32, i32, i32, i32, i32), ()>,
    decrypt_msg_fn: TypedFunc<(i32, i32, i32, i32, i32), ()>,
    msg_session_id_fn: TypedFunc<i32, i32>,
    msg_plaintext_fn: TypedFunc<i32, i32>,
    msg_dec_free_fn: TypedFunc<(i32, i32), ()>,
    msg_enc_free_fn: TypedFunc<(i32, i32), ()>,
}

impl SyncWasm {
    /// Loads `libzproto_wasm_bg` from a file path on disk.
    pub fn from_file(path: &Path) -> Result<Self, SyncWasmError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// Instantiates `libzproto_wasm_bg` from raw `.wasm` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SyncWasmError> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes)
            .map_err(|e| SyncWasmError::Wasmi(format!("compile: {e}")))?;
        let mut store = Store::new(&engine, HostState::new());
        let mut linker: Linker<HostState> = Linker::new(&engine);
        register_wbg_imports(&mut linker)?;

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| SyncWasmError::Wasmi(format!("instantiate: {e}")))?;
        Self::from_instance(store, instance)
    }

    fn from_instance(mut store: Store<HostState>, instance: Instance) -> Result<Self, SyncWasmError> {
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| SyncWasmError::Protocol("missing exported memory".into()))?;
        store.data_mut().memory = Some(memory);

        let get_fn = |name: &str| {
            instance
                .get_func(&store, name)
                .ok_or_else(|| SyncWasmError::Protocol(format!("missing export {name}")))
        };

        let add_to_stack_pointer = get_fn("__wbindgen_add_to_stack_pointer")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let malloc = get_fn("__wbindgen_export_2")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let free_mem = get_fn("__wbindgen_export_1")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;

        let gen_keypair_fn = get_fn("zprotoEd25519GenerateKeyPair")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let keypair_pub_fn = get_fn("zprotoed25519keypair_publicKey")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let keypair_priv_fn = get_fn("zprotoed25519keypair_privateKey")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let keypair_free_fn = get_fn("__wbg_zprotoed25519keypair_free")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;

        let create_meta_fn = get_fn("zprotoSync2CreateMetadataCipher")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let decrypt_meta_fn = get_fn("zprotoSync2DecryptMetadata")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let meta_session_id_fn = get_fn("zprotosyncmetadatadecrypted_sessionId")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let meta_plaintext_fn = get_fn("zprotosyncmetadatadecrypted_plaintext")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let meta_sender_id_fn = get_fn("zprotosyncmetadatadecrypted_senderIdentity")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let meta_session_rec_fn = get_fn("zprotosyncmetadatadecrypted_sessionRecord")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let meta_dec_free_fn = get_fn("__wbg_zprotosyncmetadatadecrypted_free")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let meta_enc_free_fn = get_fn("__wbg_zprotosyncmetadataencrypted_free")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;

        let encrypt_msg_fn = get_fn("zprotoSync2EncryptMessage")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let decrypt_msg_fn = get_fn("zprotoSync2DecryptMessage")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let msg_session_id_fn = get_fn("zprotosyncmessagedecrypted_sessionId")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let msg_plaintext_fn = get_fn("zprotosyncmessagedecrypted_plaintext")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let msg_dec_free_fn = get_fn("__wbg_zprotosyncmessagedecrypted_free")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let msg_enc_free_fn = get_fn("__wbg_zprotosyncmessageencrypted_free")?
            .typed(&store)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;

        Ok(Self {
            store,
            memory,
            add_to_stack_pointer,
            malloc,
            free_mem,
            gen_keypair_fn,
            keypair_pub_fn,
            keypair_priv_fn,
            keypair_free_fn,
            create_meta_fn,
            decrypt_meta_fn,
            meta_session_id_fn,
            meta_plaintext_fn,
            meta_sender_id_fn,
            meta_session_rec_fn,
            meta_dec_free_fn,
            meta_enc_free_fn,
            encrypt_msg_fn,
            decrypt_msg_fn,
            msg_session_id_fn,
            msg_plaintext_fn,
            msg_dec_free_fn,
            msg_enc_free_fn,
        })
    }

    fn write_bytes(&mut self, data: &[u8]) -> Result<(i32, i32), SyncWasmError> {
        let len = data.len() as i32;
        let ptr = self
            .malloc
            .call(&mut self.store, (len, 1))
            .map_err(|e| SyncWasmError::Wasmi(format!("malloc({len}): {e}")))?;
        if !data.is_empty() {
            self.memory
                .write(&mut self.store, ptr as usize, data)
                .map_err(|e| SyncWasmError::Wasmi(format!("mem write: {e}")))?;
        }
        Ok((ptr, len))
    }

    fn read_i32(&self, addr: i32) -> Result<i32, SyncWasmError> {
        let mut buf = [0u8; 4];
        self.memory
            .read(&self.store, addr as usize, &mut buf)
            .map_err(|e| SyncWasmError::Wasmi(format!("mem read: {e}")))?;
        Ok(i32::from_le_bytes(buf))
    }

    fn take_bytes(&mut self, idx: i32) -> Result<Vec<u8>, SyncWasmError> {
        let val = self.store.data_mut().take(idx as u32);
        match val {
            JsVal::Bytes(b) => Ok(b),
            JsVal::MemoryView { offset, len } => {
                let mut out = vec![0u8; len as usize];
                self.memory
                    .read(&self.store, offset as usize, &mut out)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                Ok(out)
            }
            other => Err(SyncWasmError::Protocol(format!(
                "expected Uint8Array from wasm getter, got {other:?}"
            ))),
        }
    }

    fn take_string(&mut self, idx: i32) -> Result<String, SyncWasmError> {
        let val = self.store.data_mut().take(idx as u32);
        match val {
            JsVal::String(s) => Ok(s),
            other => Err(SyncWasmError::Protocol(format!(
                "expected string from wasm getter, got {other:?}"
            ))),
        }
    }

    fn check_result_ptr(&mut self, retptr: i32) -> Result<i32, SyncWasmError> {
        let ptr = self.read_i32(retptr)?;
        let err_idx = self.read_i32(retptr + 4)?;
        let is_err = self.read_i32(retptr + 8)?;
        if is_err != 0 {
            let err_val = self.store.data_mut().take(err_idx as u32);
            return Err(SyncWasmError::Exception(HostState::describe_val(&err_val)));
        }
        Ok(ptr)
    }

    /// Generates an Ed25519/Sync2 keypair (`la()` -> `oa.e()` in Electron `shared-worker`).
    pub fn gen_keypair(&mut self) -> Result<KeyPair, SyncWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let call_res = self.gen_keypair_fn.call(&mut self.store, retptr);
        let result = match call_res {
            Ok(()) => self.check_result_ptr(retptr).and_then(|ptr| {
                let pub_idx = self
                    .keypair_pub_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let priv_idx = self
                    .keypair_priv_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let public_key = self.take_bytes(pub_idx)?;
                let private_key = self.take_bytes(priv_idx)?;
                let _ = self.keypair_free_fn.call(&mut self.store, (ptr, 0));
                Ok(KeyPair {
                    public_key,
                    private_key,
                })
            }),
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(SyncWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    /// Decrypts a `BatchType::MetadataEmbedded` chunk (`oa.j` in Electron `shared-worker`).
    pub fn decrypt_metadata(
        &mut self,
        ciphertext: &[u8],
        ik_public: &[u8],
        ik_private: &[u8],
        ek_private: &[u8],
        session_id_hint: Option<&str>,
    ) -> Result<DecryptedMetadata, SyncWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let (c_ptr, c_len) = self.write_bytes(ciphertext)?;
        let (ipub_ptr, ipub_len) = self.write_bytes(ik_public)?;
        let (ipriv_ptr, ipriv_len) = self.write_bytes(ik_private)?;
        let (epriv_ptr, epriv_len) = self.write_bytes(ek_private)?;
        let (opt_ptr, opt_len) = match session_id_hint {
            Some(s) => self.write_bytes(s.as_bytes())?,
            None => (0, 0),
        };
        let call_res = self.decrypt_meta_fn.call(
            &mut self.store,
            (
                retptr, c_ptr, c_len, ipub_ptr, ipub_len, ipriv_ptr, ipriv_len, epriv_ptr,
                epriv_len, opt_ptr, opt_len,
            ),
        );
        let result = match call_res {
            Ok(()) => self.check_result_ptr(retptr).and_then(|ptr| {
                let sid_idx = self
                    .meta_session_id_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let pt_idx = self
                    .meta_plaintext_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let sender_idx = self
                    .meta_sender_id_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let rec_idx = self
                    .meta_session_rec_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let session_id = self.take_string(sid_idx)?;
                let plaintext = self.take_bytes(pt_idx)?;
                let sender_identity = self.take_bytes(sender_idx)?;
                let session_record = self.take_bytes(rec_idx)?;
                let _ = self.meta_dec_free_fn.call(&mut self.store, (ptr, 0));
                Ok(DecryptedMetadata {
                    session_id,
                    plaintext,
                    sender_identity,
                    session_record,
                })
            }),
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(SyncWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    /// Encrypts a metadata chunk (`oa.h` in Electron `shared-worker`, used in roundtrip tests).
    pub fn create_metadata_cipher(
        &mut self,
        plaintext: &[u8],
        sender_ik_public: &[u8],
        sender_ik_private: &[u8],
        receiver_ik_public: &[u8],
        receiver_ek_public: &[u8],
        session_id_hint: Option<&str>,
    ) -> Result<EncryptedMetadata, SyncWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let (p_ptr, p_len) = self.write_bytes(plaintext)?;
        let (sipub_ptr, sipub_len) = self.write_bytes(sender_ik_public)?;
        let (sipriv_ptr, sipriv_len) = self.write_bytes(sender_ik_private)?;
        let (ripub_ptr, ripub_len) = self.write_bytes(receiver_ik_public)?;
        let (repub_ptr, repub_len) = self.write_bytes(receiver_ek_public)?;
        let (opt_ptr, opt_len) = match session_id_hint {
            Some(s) => self.write_bytes(s.as_bytes())?,
            None => (0, 0),
        };
        let call_res = self.create_meta_fn.call(
            &mut self.store,
            (
                retptr, p_ptr, p_len, sipub_ptr, sipub_len, sipriv_ptr, sipriv_len, ripub_ptr,
                ripub_len, repub_ptr, repub_len, opt_ptr, opt_len,
            ),
        );
        let result = match call_res {
            Ok(()) => self.check_result_ptr(retptr).and_then(|ptr| {
                let sid_idx = self
                    .meta_session_id_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let ct_idx = self
                    .meta_plaintext_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let recv_idx = self
                    .meta_sender_id_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let rec_idx = self
                    .meta_session_rec_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let session_id = self.take_string(sid_idx)?;
                let ciphertext = self.take_bytes(ct_idx)?;
                let receiver_identity = self.take_bytes(recv_idx)?;
                let session_record = self.take_bytes(rec_idx)?;
                let _ = self.meta_enc_free_fn.call(&mut self.store, (ptr, 0));
                Ok(EncryptedMetadata {
                    session_id,
                    ciphertext,
                    receiver_identity,
                    session_record,
                })
            }),
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(SyncWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    /// Decrypts a `BatchType::Message` chunk using the `session_record` from metadata (`oa.i`).
    pub fn decrypt_message(
        &mut self,
        ciphertext: &[u8],
        session_record: &[u8],
    ) -> Result<DecryptedMessage, SyncWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let (c_ptr, c_len) = self.write_bytes(ciphertext)?;
        let (r_ptr, r_len) = self.write_bytes(session_record)?;
        let call_res = self
            .decrypt_msg_fn
            .call(&mut self.store, (retptr, c_ptr, c_len, r_ptr, r_len));
        let result = match call_res {
            Ok(()) => self.check_result_ptr(retptr).and_then(|ptr| {
                let sid_idx = self
                    .msg_session_id_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let pt_idx = self
                    .msg_plaintext_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let session_id = self.take_string(sid_idx)?;
                let plaintext = self.take_bytes(pt_idx)?;
                let _ = self.msg_dec_free_fn.call(&mut self.store, (ptr, 0));
                Ok(DecryptedMessage {
                    session_id,
                    plaintext,
                })
            }),
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(SyncWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    /// Encrypts a message chunk using `session_record` (`oa.k`, used in roundtrip tests).
    pub fn encrypt_message(
        &mut self,
        plaintext: &[u8],
        session_record: &[u8],
    ) -> Result<EncryptedMessage, SyncWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
        let (p_ptr, p_len) = self.write_bytes(plaintext)?;
        let (r_ptr, r_len) = self.write_bytes(session_record)?;
        let call_res = self
            .encrypt_msg_fn
            .call(&mut self.store, (retptr, p_ptr, p_len, r_ptr, r_len));
        let result = match call_res {
            Ok(()) => self.check_result_ptr(retptr).and_then(|ptr| {
                let sid_idx = self
                    .msg_session_id_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let ct_idx = self
                    .msg_plaintext_fn
                    .call(&mut self.store, ptr)
                    .map_err(|e| SyncWasmError::Wasmi(e.to_string()))?;
                let session_id = self.take_string(sid_idx)?;
                let ciphertext = self.take_bytes(ct_idx)?;
                let _ = self.msg_enc_free_fn.call(&mut self.store, (ptr, 0));
                Ok(EncryptedMessage {
                    session_id,
                    ciphertext,
                })
            }),
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(SyncWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    /// Frees a guest buffer allocated via `__wbindgen_export_2`.
    #[allow(dead_code)]
    fn free_guest(&mut self, ptr: i32, len: i32) {
        let _ = self.free_mem.call(&mut self.store, (ptr, len, 1));
    }
}

fn read_guest_string(caller: &Caller<'_, HostState>, ptr: i32, len: i32) -> String {
    let Some(mem) = caller.data().memory else {
        return String::new();
    };
    let mut buf = vec![0u8; len.max(0) as usize];
    if mem.read(caller, ptr as usize, &mut buf).is_ok() {
        String::from_utf8_lossy(&buf).into_owned()
    } else {
        String::new()
    }
}

fn register_wbg_imports(linker: &mut Linker<HostState>) -> Result<(), SyncWasmError> {
    let map_err = |e: wasmi::errors::LinkerError| SyncWasmError::Wasmi(e.to_string());

    linker
        .func_wrap(
            "wbg",
            "__wbindgen_object_drop_ref",
            |mut caller: Caller<'_, HostState>, idx: i32| {
                let _ = caller.data_mut().take(idx as u32);
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbindgen_object_clone_ref",
            |mut caller: Caller<'_, HostState>, idx: i32| -> i32 {
                let val = caller.data().get(idx as u32).clone();
                caller.data_mut().alloc(val) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_new_405e22f390576ce2",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::Object(HashMap::new())) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_set_3f1d0b984ed272ed",
            |mut caller: Caller<'_, HostState>, obj_idx: i32, key_idx: i32, val_idx: i32| {
                let key = HostState::describe_val(&caller.data_mut().take(key_idx as u32));
                let val = caller.data_mut().take(val_idx as u32);
                if let Some(JsVal::Object(map)) = caller.data_mut().get_mut(obj_idx as u32) {
                    map.insert(key, val);
                }
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbindgen_string_new",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let s = read_guest_string(&caller, ptr, len);
                caller.data_mut().alloc(JsVal::String(s)) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbindgen_number_new",
            |mut caller: Caller<'_, HostState>, n: f64| -> i32 {
                caller.data_mut().alloc(JsVal::Number(n)) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_new_78feb108b6472713",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::Array(Vec::new())) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_push_737cfc8c1432c2c6",
            |mut caller: Caller<'_, HostState>, arr_idx: i32, val_idx: i32| -> i32 {
                let val = caller.data().get(val_idx as u32).clone();
                if let Some(JsVal::Array(arr)) = caller.data_mut().get_mut(arr_idx as u32) {
                    arr.push(val);
                    arr.len() as i32
                } else {
                    0
                }
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_apply_36be6a55257c99bf",
            |mut caller: Caller<'_, HostState>, _fn_idx: i32, _this_idx: i32, _args_idx: i32| -> i32 {
                caller.data_mut().alloc(JsVal::Undefined) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbindgen_memory",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::Memory) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_buffer_609cc3eee51ed158",
            |mut caller: Caller<'_, HostState>, idx: i32| -> i32 {
                let val = caller.data().get(idx as u32).clone();
                caller.data_mut().alloc(val) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_newwithbyteoffsetandlength_d97e637ebe145a9a",
            |mut caller: Caller<'_, HostState>, _buf_idx: i32, offset: i32, len: i32| -> i32 {
                caller.data_mut().alloc(JsVal::MemoryView {
                    offset: offset as u32,
                    len: len as u32,
                }) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_new_a12002a7f91c75be",
            |mut caller: Caller<'_, HostState>, src_idx: i32| -> i32 {
                let bytes = match caller.data().get(src_idx as u32).clone() {
                    JsVal::Bytes(b) => b,
                    JsVal::MemoryView { offset, len } => {
                        let mut buf = vec![0u8; len as usize];
                        if let Some(mem) = caller.data().memory {
                            let _ = mem.read(&caller, offset as usize, &mut buf);
                        }
                        buf
                    }
                    _ => Vec::new(),
                };
                caller.data_mut().alloc(JsVal::Bytes(bytes)) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_newwithlength_a381634e90c276d4",
            |mut caller: Caller<'_, HostState>, len: i32| -> i32 {
                let buf = vec![0u8; len.max(0) as usize];
                caller.data_mut().alloc(JsVal::Bytes(buf)) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_subarray_aa9065fa9dc5df96",
            |mut caller: Caller<'_, HostState>, arr_idx: i32, start: i32, end: i32| -> i32 {
                let s = start as u32;
                let e = end as u32;
                let view = match caller.data().get(arr_idx as u32).clone() {
                    JsVal::MemoryView { offset, .. } => JsVal::MemoryView {
                        offset: offset.saturating_add(s),
                        len: e.saturating_sub(s),
                    },
                    JsVal::Bytes(b) => {
                        let from = (s as usize).min(b.len());
                        let to = (e as usize).min(b.len()).max(from);
                        JsVal::Bytes(b[from..to].to_vec())
                    }
                    _ => JsVal::Bytes(Vec::new()),
                };
                caller.data_mut().alloc(view) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_set_65595bdd868b3009",
            |mut caller: Caller<'_, HostState>, dst_idx: i32, src_idx: i32, offset: i32| {
                let src_bytes = match caller.data().get(src_idx as u32).clone() {
                    JsVal::Bytes(b) => b,
                    JsVal::MemoryView { offset: mo, len } => {
                        let mut buf = vec![0u8; len as usize];
                        if let Some(mem) = caller.data().memory {
                            let _ = mem.read(&caller, mo as usize, &mut buf);
                        }
                        buf
                    }
                    _ => return,
                };
                let dst = caller.data().get(dst_idx as u32).clone();
                match dst {
                    JsVal::MemoryView { offset: mo, .. } => {
                        if let Some(mem) = caller.data().memory {
                            let write_at = (mo as usize).saturating_add(offset as usize);
                            let _ = mem.write(&mut caller, write_at, &src_bytes);
                        }
                    }
                    JsVal::Bytes(_) => {
                        if let Some(JsVal::Bytes(buf)) = caller.data_mut().get_mut(dst_idx as u32) {
                            let start = offset as usize;
                            let end = start.saturating_add(src_bytes.len()).min(buf.len());
                            if start < end {
                                buf[start..end].copy_from_slice(&src_bytes[..(end - start)]);
                            }
                        }
                    }
                    _ => {}
                }
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_getRandomValues_bcb4912f16000dc4",
            |mut caller: Caller<'_, HostState>, _crypto_idx: i32, dst_idx: i32| {
                let target = caller.data().get(dst_idx as u32).clone();
                match target {
                    JsVal::MemoryView { offset, len } => {
                        let mut rnd = vec![0u8; len as usize];
                        rand::rngs::OsRng.fill_bytes(&mut rnd);
                        if let Some(mem) = caller.data().memory {
                            let _ = mem.write(&mut caller, offset as usize, &rnd);
                        }
                    }
                    JsVal::Bytes(_) => {
                        if let Some(JsVal::Bytes(buf)) = caller.data_mut().get_mut(dst_idx as u32) {
                            rand::rngs::OsRng.fill_bytes(buf);
                        }
                    }
                    _ => {}
                }
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_randomFillSync_ab2cfe79ebbf2740",
            |mut caller: Caller<'_, HostState>, _crypto_idx: i32, dst_idx: i32| {
                let target = caller.data_mut().take(dst_idx as u32);
                if let JsVal::MemoryView { offset, len } = target {
                    let mut rnd = vec![0u8; len as usize];
                    rand::rngs::OsRng.fill_bytes(&mut rnd);
                    if let Some(mem) = caller.data().memory {
                        let _ = mem.write(&mut caller, offset as usize, &rnd);
                    }
                }
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_new_8a6f238a6ece86ea",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::Error(String::new())) as i32
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_stack_0ed75d68575b0f3c",
            |mut caller: Caller<'_, HostState>, retptr: i32, _err_idx: i32| {
                if let Some(mem) = caller.data().memory {
                    let _ = mem.write(&mut caller, retptr as usize, &0i32.to_le_bytes());
                    let _ = mem.write(&mut caller, (retptr + 4) as usize, &0i32.to_le_bytes());
                }
            },
        )
        .map_err(map_err)?;

    linker
        .func_wrap(
            "wbg",
            "__wbg_error_7534b8e9a36f1ab4",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
                let msg = read_guest_string(&caller, ptr, len);
                caller.data_mut().last_exn = Some(msg);
            },
        )
        .map_err(map_err)?;

    // Environment probing for `getrandom` (emulate browser `globalThis.crypto.getRandomValues`).
    linker
        .func_wrap(
            "wbg",
            "__wbg_static_accessor_GLOBAL_88a902d13a557d07",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::GlobalThis) as i32
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_static_accessor_GLOBAL_THIS_56578be7e9f832b0",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::GlobalThis) as i32
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_static_accessor_WINDOW_5de37043a91a9c40",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::GlobalThis) as i32
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_static_accessor_SELF_37c5d418e4bf5819",
            |mut caller: Caller<'_, HostState>| -> i32 {
                caller.data_mut().alloc(JsVal::GlobalThis) as i32
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_crypto_ed58b8e10a292839",
            |mut caller: Caller<'_, HostState>, _obj: i32| -> i32 {
                caller.data_mut().alloc(JsVal::Crypto) as i32
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_msCrypto_0a36e2ec3a343d26",
            |_caller: Caller<'_, HostState>, _obj: i32| -> i32 { 128 }, // undefined
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_process_5c1d670bc53614b8",
            |_caller: Caller<'_, HostState>, _obj: i32| -> i32 { 128 },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_versions_c71aa1626a93e0a1",
            |_caller: Caller<'_, HostState>, _obj: i32| -> i32 { 128 },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_node_02999533c4ea02e3",
            |_caller: Caller<'_, HostState>, _obj: i32| -> i32 { 128 },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_require_79b1e9274cde3c87",
            |_caller: Caller<'_, HostState>| -> i32 { 128 },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_newnoargs_105ed471475aaf50",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
                let src = read_guest_string(&caller, ptr, len);
                caller.data_mut().alloc(JsVal::Function(src)) as i32
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_call_672a4d21634d4a24",
            |mut caller: Caller<'_, HostState>, fn_idx: i32, _this_idx: i32| -> i32 {
                match caller.data().get(fn_idx as u32) {
                    JsVal::Function(body) if body.contains("this") => {
                        caller.data_mut().alloc(JsVal::GlobalThis) as i32
                    }
                    _ => 128,
                }
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbg_call_7cccdd69e0791ae2",
            |_caller: Caller<'_, HostState>, _fn_idx: i32, _this_idx: i32, _arg_idx: i32| -> i32 {
                128
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbindgen_is_object",
            |caller: Caller<'_, HostState>, idx: i32| -> i32 {
                i32::from(matches!(
                    caller.data().get(idx as u32),
                    JsVal::Object(_)
                        | JsVal::Array(_)
                        | JsVal::Bytes(_)
                        | JsVal::MemoryView { .. }
                        | JsVal::GlobalThis
                        | JsVal::Crypto
                        | JsVal::Error(_)
                ))
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbindgen_is_string",
            |caller: Caller<'_, HostState>, idx: i32| -> i32 {
                i32::from(matches!(caller.data().get(idx as u32), JsVal::String(_)))
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbindgen_is_function",
            |caller: Caller<'_, HostState>, idx: i32| -> i32 {
                i32::from(matches!(caller.data().get(idx as u32), JsVal::Function(_)))
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbindgen_is_undefined",
            |caller: Caller<'_, HostState>, idx: i32| -> i32 {
                i32::from(matches!(caller.data().get(idx as u32), JsVal::Undefined))
            },
        )
        .map_err(map_err)?;
    linker
        .func_wrap(
            "wbg",
            "__wbindgen_throw",
            |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> Result<(), wasmi::Error> {
                let msg = read_guest_string(&caller, ptr, len);
                caller.data_mut().last_exn = Some(msg.clone());
                Err(wasmi::Error::new(msg))
            },
        )
        .map_err(map_err)?;

    Ok(())
}
