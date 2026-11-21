//! Safe `wasmi` host for Electron's PC → Phone Backup crypto module
//! (`libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm`, webpack module `dGpS`).
//!
//! This module exclusively loads the Backup WASM binary (PC → phone).
//! It never loads or calls the Sync 2 WASM module (`libzproto_wasm_bg`).

use std::collections::HashMap;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rand::RngCore;
use thiserror::Error;
use wasmi::{Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

/// Exact Electron Backup WASM filename.
pub const BACKUP_WASM_FILENAME: &str =
    "libzproto_backup_wasm_bg.042497d03f0f504b6b1444e25a7db0d9.wasm";

// Electron key material (`default-main-startup-shared-worker` module `qg/u` + `shared-worker` `ta`).
const MASK_D: [u8; 64] = [
    237, 77, 150, 181, 120, 56, 16, 117, 251, 72, 16, 159, 140, 23, 6, 248, 143, 41, 87, 37, 68,
    46, 93, 78, 100, 160, 173, 188, 158, 26, 236, 241, 70, 8, 78, 69, 33, 130, 176, 167, 0, 168,
    203, 165, 50, 116, 40, 245, 27, 210, 151, 60, 237, 17, 84, 77, 236, 175, 116, 205, 209, 30,
    42, 10,
];
const PART_C: [u8; 11] = [151, 25, 175, 222, 52, 12, 96, 34, 201, 62, 94];
const PART_A: [u8; 18] = [
    168, 225, 79, 51, 137, 221, 17, 61, 102, 117, 72, 21, 125, 6, 226, 155, 219, 199,
];
const PART_TA: [u8; 35] = [
    42, 136, 188, 127, 127, 5, 125, 66, 211, 130, 212, 90, 157, 191, 227, 5, 26, 126, 193, 99,
    130, 166, 84, 167, 34, 38, 1, 218, 214, 51, 253, 179, 90, 31, 96,
];

/// Derives the 64-byte key array passed into `new ZProtoBackupCryptoSession(nonce, salt, key)`
/// (`shared-worker` lines 6472–6474).
#[must_use]
pub fn derive_backup_wasm_key() -> [u8; 64] {
    let mut raw = [0u8; 64];
    raw[..11].copy_from_slice(&PART_C);
    raw[11..29].copy_from_slice(&PART_A);
    raw[29..64].copy_from_slice(&PART_TA);
    for (b, m) in raw.iter_mut().zip(MASK_D.iter()) {
        *b ^= *m;
    }
    raw
}

/// Errors raised while instantiating or calling the Backup WASM module.
#[derive(Debug, Error)]
pub enum BackupWasmError {
    /// Failed to read `.wasm` file from disk.
    #[error("failed to read Backup wasm file: {0}")]
    Io(#[from] std::io::Error),
    /// `wasmi` compilation, linking, or execution error.
    #[error("wasmi error: {0}")]
    Wasmi(String),
    /// The WASM module threw a JS exception or returned an error slot.
    #[error("backup wasm exception: {0}")]
    Exception(String),
    /// Missing/invalid credentials or export.
    #[error("backup wasm protocol error: {0}")]
    Protocol(String),
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
                    let code = map.get("code").map(Self::describe_val).unwrap_or_default();
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

/// Live instance of Electron's Backup WASM module (`libzproto_backup_wasm_bg`).
pub struct BackupWasm {
    store: Store<HostState>,
    memory: Memory,
    add_to_stack_pointer: TypedFunc<i32, i32>,
    malloc: TypedFunc<(i32, i32), i32>,
    free_mem: TypedFunc<(i32, i32, i32), ()>,
    session_new_fn: TypedFunc<(i32, i32, i32, i32, i32, i32), ()>,
    session_encrypt_fn: TypedFunc<(i32, i32, i32, i32), ()>,
    session_decrypt_fn: TypedFunc<(i32, i32, i32, i32), ()>,
    session_free_fn: TypedFunc<(i32, i32), ()>,
}

/// Handle to an open `ZProtoBackupCryptoSession` (or raw passthrough when `server_nonce == ""`).
pub struct BackupCryptoSession {
    wasm: Option<BackupWasm>,
    session_ptr: i32,
}

impl BackupCryptoSession {
    /// Starts a backup crypto session matching `Jn.start` + `Sa.start` (`shared-worker` lines 6181 & 6469).
    ///
    /// - `client_salt_b64` MUST be non-empty (matches worker guard `missing clientSalt`).
    /// - When `encryption_enabled` is `true`, `server_nonce_b64` MUST be non-empty; empty nonce is
    ///   rejected (never silently falls back to raw mode!).
    /// - When `encryption_enabled` is `false` and `server_nonce_b64` is `""`, raw mode is used.
    pub fn start_from_file(
        wasm_path: &Path,
        server_nonce_b64: &str,
        client_salt_b64: &str,
        encryption_enabled: bool,
    ) -> Result<Self, BackupWasmError> {
        if client_salt_b64.is_empty() {
            return Err(BackupWasmError::Protocol(
                "BackupMsgHandler: missing clientSalt — CMD 631 must run before dispatching the backup task".into(),
            ));
        }
        if encryption_enabled && server_nonce_b64.is_empty() {
            return Err(BackupWasmError::Protocol(
                "encryption_enabled is true but serverNonce is empty — refusing raw fallback".into(),
            ));
        }
        if server_nonce_b64.is_empty() {
            return Ok(Self {
                wasm: None,
                session_ptr: 0,
            });
        }
        let wasm_bytes = std::fs::read(wasm_path)?;
        Self::start_from_bytes(&wasm_bytes, server_nonce_b64, client_salt_b64, encryption_enabled)
    }

    /// Same as [`Self::start_from_file`] but takes raw `.wasm` bytes.
    pub fn start_from_bytes(
        wasm_bytes: &[u8],
        server_nonce_b64: &str,
        client_salt_b64: &str,
        encryption_enabled: bool,
    ) -> Result<Self, BackupWasmError> {
        if client_salt_b64.is_empty() {
            return Err(BackupWasmError::Protocol(
                "BackupMsgHandler: missing clientSalt — CMD 631 must run before dispatching the backup task".into(),
            ));
        }
        if encryption_enabled && server_nonce_b64.is_empty() {
            return Err(BackupWasmError::Protocol(
                "encryption_enabled is true but serverNonce is empty — refusing raw fallback".into(),
            ));
        }
        if server_nonce_b64.is_empty() {
            return Ok(Self {
                wasm: None,
                session_ptr: 0,
            });
        }
        let nonce = B64
            .decode(server_nonce_b64)
            .map_err(|e| BackupWasmError::Protocol(format!("invalid serverNonce base64: {e}")))?;
        let salt = B64
            .decode(client_salt_b64)
            .map_err(|e| BackupWasmError::Protocol(format!("invalid clientSalt base64: {e}")))?;
        if nonce.is_empty() {
            return Err(BackupWasmError::Protocol(
                "decoded serverNonce is empty while encryption is enabled".into(),
            ));
        }
        let mut wasm = BackupWasm::from_bytes(wasm_bytes)?;
        let key = derive_backup_wasm_key();
        let session_ptr = wasm.new_session(&nonce, &salt, &key)?;
        Ok(Self {
            wasm: Some(wasm),
            session_ptr,
        })
    }

    /// Encrypts one compressed protobuf block (`encryptMessage`).
    pub fn encrypt_block(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, BackupWasmError> {
        match &mut self.wasm {
            Some(wasm) => wasm.encrypt_message(self.session_ptr, plaintext),
            None => Ok(plaintext.to_vec()),
        }
    }

    /// Decrypts one encrypted block (`decryptMessage`, for roundtrip verification).
    pub fn decrypt_block(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, BackupWasmError> {
        match &mut self.wasm {
            Some(wasm) => wasm.decrypt_message(self.session_ptr, ciphertext),
            None => Ok(ciphertext.to_vec()),
        }
    }
}

impl Drop for BackupCryptoSession {
    fn drop(&mut self) {
        if let Some(wasm) = &mut self.wasm {
            if self.session_ptr != 0 {
                wasm.free_session(self.session_ptr);
                self.session_ptr = 0;
            }
        }
    }
}

impl BackupWasm {
    /// Instantiates `libzproto_backup_wasm_bg` from raw `.wasm` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BackupWasmError> {
        let engine = Engine::default();
        let module = Module::new(&engine, bytes)
            .map_err(|e| BackupWasmError::Wasmi(format!("compile: {e}")))?;
        let mut store = Store::new(&engine, HostState::new());
        let mut linker: Linker<HostState> = Linker::new(&engine);
        register_wbg_imports(&mut linker)?;

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| BackupWasmError::Wasmi(format!("instantiate: {e}")))?;
        Self::from_instance(store, instance)
    }

    fn from_instance(
        mut store: Store<HostState>,
        instance: Instance,
    ) -> Result<Self, BackupWasmError> {
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| BackupWasmError::Protocol("missing exported memory".into()))?;
        store.data_mut().memory = Some(memory);

        let get_fn = |name: &str| {
            instance
                .get_func(&store, name)
                .ok_or_else(|| BackupWasmError::Protocol(format!("missing export {name}")))
        };

        let add_to_stack_pointer = get_fn("__wbindgen_add_to_stack_pointer")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let malloc = get_fn("__wbindgen_export_2")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let free_mem = get_fn("__wbindgen_export_1")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let session_new_fn = get_fn("zprotobackupcryptosession_new")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let session_encrypt_fn = get_fn("zprotobackupcryptosession_encryptMessage")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let session_decrypt_fn = get_fn("zprotobackupcryptosession_decryptMessage")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let session_free_fn = get_fn("__wbg_zprotobackupcryptosession_free")?
            .typed(&store)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;

        Ok(Self {
            store,
            memory,
            add_to_stack_pointer,
            malloc,
            free_mem,
            session_new_fn,
            session_encrypt_fn,
            session_decrypt_fn,
            session_free_fn,
        })
    }

    fn write_bytes(&mut self, data: &[u8]) -> Result<(i32, i32), BackupWasmError> {
        let len = data.len() as i32;
        let ptr = self
            .malloc
            .call(&mut self.store, (len, 1))
            .map_err(|e| BackupWasmError::Wasmi(format!("malloc({len}): {e}")))?;
        if !data.is_empty() {
            self.memory
                .write(&mut self.store, ptr as usize, data)
                .map_err(|e| BackupWasmError::Wasmi(format!("mem write: {e}")))?;
        }
        Ok((ptr, len))
    }

    fn read_i32(&self, addr: i32) -> Result<i32, BackupWasmError> {
        let mut buf = [0u8; 4];
        self.memory
            .read(&self.store, addr as usize, &mut buf)
            .map_err(|e| BackupWasmError::Wasmi(format!("mem read: {e}")))?;
        Ok(i32::from_le_bytes(buf))
    }

    fn new_session(
        &mut self,
        nonce: &[u8],
        salt: &[u8],
        key: &[u8],
    ) -> Result<i32, BackupWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let (n_ptr, n_len) = self.write_bytes(nonce)?;
        let (s_ptr, s_len) = self.write_bytes(salt)?;
        let key_idx = self.store.data_mut().alloc(JsVal::Bytes(key.to_vec())) as i32;

        let call_res = self
            .session_new_fn
            .call(&mut self.store, (retptr, n_ptr, n_len, s_ptr, s_len, key_idx));
        let result = match call_res {
            Ok(()) => {
                let ptr = self.read_i32(retptr)?;
                let err_idx = self.read_i32(retptr + 4)?;
                let is_err = self.read_i32(retptr + 8)?;
                if is_err != 0 {
                    let val = self.store.data_mut().take(err_idx as u32);
                    Err(BackupWasmError::Exception(HostState::describe_val(&val)))
                } else {
                    Ok(ptr)
                }
            }
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(BackupWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    fn call_transform(
        &mut self,
        encrypt: bool,
        session_ptr: i32,
        input: &[u8],
    ) -> Result<Vec<u8>, BackupWasmError> {
        let retptr = self
            .add_to_stack_pointer
            .call(&mut self.store, -16)
            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
        let (in_ptr, in_len) = self.write_bytes(input)?;
        let call_res = if encrypt {
            self.session_encrypt_fn
                .call(&mut self.store, (retptr, session_ptr, in_ptr, in_len))
        } else {
            self.session_decrypt_fn
                .call(&mut self.store, (retptr, session_ptr, in_ptr, in_len))
        };
        let result = match call_res {
            Ok(()) => {
                let out_ptr = self.read_i32(retptr)?;
                let out_len = self.read_i32(retptr + 4)?;
                let err_idx = self.read_i32(retptr + 8)?;
                let is_err = self.read_i32(retptr + 12)?;
                if is_err != 0 {
                    let val = self.store.data_mut().take(err_idx as u32);
                    Err(BackupWasmError::Exception(HostState::describe_val(&val)))
                } else {
                    let mut out = vec![0u8; out_len.max(0) as usize];
                    if out_len > 0 {
                        self.memory
                            .read(&self.store, out_ptr as usize, &mut out)
                            .map_err(|e| BackupWasmError::Wasmi(e.to_string()))?;
                    }
                    let _ = self.free_mem.call(&mut self.store, (out_ptr, out_len, 1));
                    Ok(out)
                }
            }
            Err(e) => {
                let msg = self
                    .store
                    .data_mut()
                    .last_exn
                    .take()
                    .unwrap_or_else(|| e.to_string());
                Err(BackupWasmError::Exception(msg))
            }
        };
        let _ = self.add_to_stack_pointer.call(&mut self.store, 16);
        result
    }

    fn encrypt_message(&mut self, session_ptr: i32, data: &[u8]) -> Result<Vec<u8>, BackupWasmError> {
        self.call_transform(true, session_ptr, data)
    }

    fn decrypt_message(&mut self, session_ptr: i32, data: &[u8]) -> Result<Vec<u8>, BackupWasmError> {
        self.call_transform(false, session_ptr, data)
    }

    fn free_session(&mut self, session_ptr: i32) {
        let _ = self.session_free_fn.call(&mut self.store, (session_ptr, 0));
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

fn register_wbg_imports(linker: &mut Linker<HostState>) -> Result<(), BackupWasmError> {
    let map_err = |e: wasmi::errors::LinkerError| BackupWasmError::Wasmi(e.to_string());

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
            "__wbg_length_a446193dc22c12f8",
            |caller: Caller<'_, HostState>, idx: i32| -> i32 {
                match caller.data().get(idx as u32) {
                    JsVal::Bytes(b) => b.len() as i32,
                    JsVal::MemoryView { len, .. } => *len as i32,
                    JsVal::Array(a) => a.len() as i32,
                    JsVal::String(s) => s.len() as i32,
                    _ => 0,
                }
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
            |mut caller: Caller<'_, HostState>, _f: i32, _t: i32, _a: i32| -> i32 {
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
            |mut caller: Caller<'_, HostState>, _buf: i32, offset: i32, len: i32| -> i32 {
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
                    JsVal::Memory => {
                        // `new Uint8Array(memory.buffer)` -> treat as MemoryView starting at 0.
                        return caller.data_mut().alloc(JsVal::MemoryView {
                            offset: 0,
                            len: u32::MAX,
                        }) as i32;
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
            |_caller: Caller<'_, HostState>, _obj: i32| -> i32 { 128 },
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
            |mut caller: Caller<'_, HostState>, fn_idx: i32, _this: i32| -> i32 {
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
            |_caller: Caller<'_, HostState>, _f: i32, _t: i32, _a: i32| -> i32 { 128 },
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
