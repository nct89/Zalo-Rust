//! Runs Zalo PC's `libzproto_wasm_bg.*.wasm` (the sync v2 crypto) with `wasmi`.
//!
//! The module was built with `wasm-bindgen`, so it expects a JavaScript host:
//! byte arrays come back as `Uint8Array` objects, errors as plain objects, and
//! randomness comes from `globalThis.crypto.getRandomValues`. [`ZprotoWasm`]
//! provides a small stand-in for that host: a heap of [`JsValue`]s indexed the
//! way `wasm-bindgen` indexes its JS heap, and the 39 imports the module uses.
//!
//! The wasm file is Zalo's, so it is not stored in this repository. Pass the
//! file from a Zalo PC install (`resources/app/pc-dist/libs/`) or from
//! the desktop UI assets (`pc-dist/libs/`).

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use rand::RngCore;
use wasmi::{Caller, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

use crate::Sync2Error;

/// Index of `undefined` in the wasm-bindgen heap.
const IDX_UNDEFINED: i32 = 128;
/// First index handed out for new objects; lower ones are reserved.
const FIRST_FREE: usize = 132;

/// A value on the emulated JS heap.
#[derive(Clone, Debug)]
enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Str(String),
    Object(Vec<(String, JsValue)>),
    Array(Vec<JsValue>),
    Error,
    GlobalThis,
    Crypto,
    Memory,
    Buffer,
    /// `new Uint8Array(memory.buffer, offset, len)`.
    MemView {
        offset: u32,
        len: u32,
    },
    /// A `Uint8Array` that owns its bytes, or a `subarray` of one.
    Bytes {
        data: Rc<RefCell<Vec<u8>>>,
        start: usize,
        end: usize,
    },
}

impl JsValue {
    fn text(&self) -> String {
        match self {
            JsValue::Str(s) => s.clone(),
            JsValue::Number(n) => n.to_string(),
            JsValue::Bool(b) => b.to_string(),
            other => format!("{other:?}"),
        }
    }
}

/// The wasm-bindgen heap: slots 128..132 hold `undefined, null, true, false`.
#[derive(Default)]
struct Host {
    slots: Vec<Option<JsValue>>,
    free: Vec<usize>,
    memory: Option<Memory>,
}

impl Host {
    fn new() -> Self {
        let mut slots = vec![None; FIRST_FREE];
        slots[128] = Some(JsValue::Undefined);
        slots[129] = Some(JsValue::Null);
        slots[130] = Some(JsValue::Bool(true));
        slots[131] = Some(JsValue::Bool(false));
        Self {
            slots,
            free: Vec::new(),
            memory: None,
        }
    }

    fn add(&mut self, value: JsValue) -> i32 {
        let idx = match self.free.pop() {
            Some(idx) => {
                self.slots[idx] = Some(value);
                idx
            }
            None => {
                self.slots.push(Some(value));
                self.slots.len() - 1
            }
        };
        idx as i32
    }

    fn get(&self, idx: i32) -> JsValue {
        usize::try_from(idx)
            .ok()
            .and_then(|i| self.slots.get(i))
            .and_then(Clone::clone)
            .unwrap_or(JsValue::Undefined)
    }

    fn take(&mut self, idx: i32) -> JsValue {
        let value = self.get(idx);
        if let Ok(i) = usize::try_from(idx) {
            if i >= FIRST_FREE && i < self.slots.len() && self.slots[i].is_some() {
                self.slots[i] = None;
                self.free.push(i);
            }
        }
        value
    }
}

fn trap(message: impl Into<String>) -> wasmi::Error {
    wasmi::Error::new(message.into())
}

fn read_str(mem: &[u8], ptr: i32, len: i32) -> Result<String, wasmi::Error> {
    let bytes = slice(mem, ptr as u32, len as u32)?;
    String::from_utf8(bytes.to_vec()).map_err(|_| trap("invalid utf-8 from wasm"))
}

fn slice(mem: &[u8], offset: u32, len: u32) -> Result<&[u8], wasmi::Error> {
    let start = offset as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| trap("overflow"))?;
    mem.get(start..end).ok_or_else(|| trap("out of bounds"))
}

/// Bytes of a `Uint8Array`-like value.
fn bytes_of(value: &JsValue, mem: &[u8]) -> Result<Vec<u8>, wasmi::Error> {
    match value {
        JsValue::MemView { offset, len } => Ok(slice(mem, *offset, *len)?.to_vec()),
        JsValue::Bytes { data, start, end } => Ok(data.borrow()[*start..*end].to_vec()),
        other => Err(trap(format!("expected Uint8Array, got {other:?}"))),
    }
}

fn host_memory(caller: &Caller<'_, Host>) -> Result<Memory, wasmi::Error> {
    caller.data().memory.ok_or_else(|| trap("memory not ready"))
}

/// Defines every import of `libzproto_wasm_bg` on `linker`.
fn define_imports(linker: &mut Linker<Host>) -> Result<(), wasmi::Error> {
    const M: &str = "wbg";
    let unsupported = |name: &'static str| {
        move || -> Result<i32, wasmi::Error> { Err(trap(format!("unsupported import {name}"))) }
    };
    linker.func_wrap(
        M,
        "__wbindgen_object_drop_ref",
        |mut c: Caller<'_, Host>, i: i32| {
            c.data_mut().take(i);
        },
    )?;
    linker.func_wrap(
        M,
        "__wbindgen_object_clone_ref",
        |mut c: Caller<'_, Host>, i: i32| {
            let v = c.data().get(i);
            c.data_mut().add(v)
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_new_405e22f390576ce2",
        |mut c: Caller<'_, Host>| c.data_mut().add(JsValue::Object(Vec::new())),
    )?;
    linker.func_wrap(
        M,
        "__wbg_set_3f1d0b984ed272ed",
        |mut c: Caller<'_, Host>, o: i32, k: i32, v: i32| {
            let host = c.data_mut();
            let key = host.take(k).text();
            let value = host.take(v);
            if let Some(Some(JsValue::Object(fields))) =
                usize::try_from(o).ok().and_then(|i| host.slots.get_mut(i))
            {
                fields.push((key, value));
            }
        },
    )?;
    linker.func_wrap(
        M,
        "__wbindgen_string_new",
        |mut c: Caller<'_, Host>, p: i32, l: i32| -> Result<i32, wasmi::Error> {
            let mem = host_memory(&c)?;
            let s = read_str(mem.data(&c), p, l)?;
            Ok(c.data_mut().add(JsValue::Str(s)))
        },
    )?;
    linker.func_wrap(
        M,
        "__wbindgen_number_new",
        |mut c: Caller<'_, Host>, n: f64| c.data_mut().add(JsValue::Number(n)),
    )?;
    linker.func_wrap(
        M,
        "__wbg_new_78feb108b6472713",
        |mut c: Caller<'_, Host>| c.data_mut().add(JsValue::Array(Vec::new())),
    )?;
    linker.func_wrap(
        M,
        "__wbg_push_737cfc8c1432c2c6",
        |mut c: Caller<'_, Host>, a: i32, v: i32| {
            let host = c.data_mut();
            let value = host.get(v);
            match usize::try_from(a).ok().and_then(|i| host.slots.get_mut(i)) {
                Some(Some(JsValue::Array(items))) => {
                    items.push(value);
                    items.len() as i32
                }
                _ => 0,
            }
        },
    )?;
    linker.func_wrap(M, "__wbindgen_memory", |mut c: Caller<'_, Host>| {
        c.data_mut().add(JsValue::Memory)
    })?;
    linker.func_wrap(
        M,
        "__wbg_buffer_609cc3eee51ed158",
        |mut c: Caller<'_, Host>, _m: i32| c.data_mut().add(JsValue::Buffer),
    )?;
    linker.func_wrap(
        M,
        "__wbg_newwithbyteoffsetandlength_d97e637ebe145a9a",
        |mut c: Caller<'_, Host>, _b: i32, offset: i32, len: i32| {
            c.data_mut().add(JsValue::MemView {
                offset: offset as u32,
                len: len as u32,
            })
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_new_a12002a7f91c75be",
        |mut c: Caller<'_, Host>, i: i32| -> Result<i32, wasmi::Error> {
            let mem = host_memory(&c)?;
            let (data, host) = mem.data_and_store_mut(&mut c);
            let source = host.get(i);
            if let JsValue::Buffer = source {
                // `new Uint8Array(memory.buffer)` is a view of the whole memory.
                let len = u32::try_from(data.len()).map_err(|_| trap("memory too large"))?;
                return Ok(host.add(JsValue::MemView { offset: 0, len }));
            }
            // `new Uint8Array(array)` copies.
            let bytes = bytes_of(&source, data)?;
            let end = bytes.len();
            Ok(host.add(JsValue::Bytes {
                data: Rc::new(RefCell::new(bytes)),
                start: 0,
                end,
            }))
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_newwithlength_a381634e90c276d4",
        |mut c: Caller<'_, Host>, n: i32| {
            let n = n as u32 as usize;
            c.data_mut().add(JsValue::Bytes {
                data: Rc::new(RefCell::new(vec![0; n])),
                start: 0,
                end: n,
            })
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_subarray_aa9065fa9dc5df96",
        |mut c: Caller<'_, Host>, a: i32, s: i32, e: i32| -> Result<i32, wasmi::Error> {
            let (s, e) = (s as u32 as usize, e as u32 as usize);
            let sub = match c.data().get(a) {
                JsValue::Bytes { data, start, end } => {
                    let new_start = (start + s).min(end);
                    let new_end = (start + e).min(end).max(new_start);
                    JsValue::Bytes {
                        data,
                        start: new_start,
                        end: new_end,
                    }
                }
                JsValue::MemView { offset, len } => {
                    let s = s.min(len as usize) as u32;
                    let e = (e.min(len as usize) as u32).max(s);
                    JsValue::MemView {
                        offset: offset + s,
                        len: e - s,
                    }
                }
                other => return Err(trap(format!("subarray on {other:?}"))),
            };
            Ok(c.data_mut().add(sub))
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_set_65595bdd868b3009",
        |mut c: Caller<'_, Host>, dst: i32, src: i32, off: i32| -> Result<(), wasmi::Error> {
            let mem = host_memory(&c)?;
            let (data, host) = mem.data_and_store_mut(&mut c);
            let bytes = bytes_of(&host.get(src), data)?;
            match host.get(dst) {
                JsValue::MemView { offset, len } => {
                    let start = (offset + off as u32) as usize;
                    if bytes.len() + off as u32 as usize > len as usize {
                        return Err(trap("set out of range"));
                    }
                    data.get_mut(start..start + bytes.len())
                        .ok_or_else(|| trap("set out of bounds"))?
                        .copy_from_slice(&bytes);
                }
                JsValue::Bytes {
                    data: target,
                    start,
                    end,
                } => {
                    let at = start + off as u32 as usize;
                    if at + bytes.len() > end {
                        return Err(trap("set out of range"));
                    }
                    target.borrow_mut()[at..at + bytes.len()].copy_from_slice(&bytes);
                }
                other => return Err(trap(format!("set on {other:?}"))),
            }
            Ok(())
        },
    )?;
    let fill_random = |c: Caller<'_, Host>, _crypto: i32, a: i32| -> Result<(), wasmi::Error> {
        match c.data().get(a) {
            JsValue::Bytes { data, start, end } => {
                rand::rngs::OsRng.fill_bytes(&mut data.borrow_mut()[start..end]);
                Ok(())
            }
            other => Err(trap(format!("getRandomValues on {other:?}"))),
        }
    };
    linker.func_wrap(M, "__wbg_getRandomValues_bcb4912f16000dc4", fill_random)?;
    linker.func_wrap(M, "__wbg_randomFillSync_ab2cfe79ebbf2740", fill_random)?;
    linker.func_wrap(
        M,
        "__wbg_new_8a6f238a6ece86ea",
        |mut c: Caller<'_, Host>| c.data_mut().add(JsValue::Error),
    )?;
    linker.func_wrap(
        M,
        "__wbg_stack_0ed75d68575b0f3c",
        |mut c: Caller<'_, Host>, ret: i32, _e: i32| -> Result<(), wasmi::Error> {
            // An empty `String`: dangling pointer 1, length 0.
            let mem = host_memory(&c)?;
            let data = mem.data_mut(&mut c);
            let at = ret as u32 as usize;
            data.get_mut(at..at + 8)
                .ok_or_else(|| trap("stack out of bounds"))?
                .copy_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
            Ok(())
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_error_7534b8e9a36f1ab4",
        |_c: Caller<'_, Host>, _p: i32, _l: i32| {},
    )?;
    linker.func_wrap(
        M,
        "__wbg_crypto_ed58b8e10a292839",
        |mut c: Caller<'_, Host>, _g: i32| c.data_mut().add(JsValue::Crypto),
    )?;
    linker.func_wrap(M, "__wbindgen_is_object", |c: Caller<'_, Host>, i: i32| {
        matches!(
            c.data().get(i),
            JsValue::Object(_)
                | JsValue::Array(_)
                | JsValue::Error
                | JsValue::GlobalThis
                | JsValue::Crypto
        ) as i32
    })?;
    linker.func_wrap(M, "__wbindgen_is_string", |c: Caller<'_, Host>, i: i32| {
        matches!(c.data().get(i), JsValue::Str(_)) as i32
    })?;
    linker.func_wrap(
        M,
        "__wbindgen_is_function",
        |_c: Caller<'_, Host>, _i: i32| 0,
    )?;
    linker.func_wrap(
        M,
        "__wbindgen_is_undefined",
        |c: Caller<'_, Host>, i: i32| matches!(c.data().get(i), JsValue::Undefined) as i32,
    )?;
    for name in [
        "__wbg_process_5c1d670bc53614b8",
        "__wbg_versions_c71aa1626a93e0a1",
        "__wbg_node_02999533c4ea02e3",
        "__wbg_msCrypto_0a36e2ec3a343d26",
    ] {
        linker.func_wrap(M, name, |_c: Caller<'_, Host>, _i: i32| IDX_UNDEFINED)?;
    }
    for name in [
        "__wbg_static_accessor_GLOBAL_88a902d13a557d07",
        "__wbg_static_accessor_WINDOW_5de37043a91a9c40",
        "__wbg_static_accessor_SELF_37c5d418e4bf5819",
    ] {
        linker.func_wrap(M, name, |_c: Caller<'_, Host>| 0)?;
    }
    linker.func_wrap(
        M,
        "__wbg_static_accessor_GLOBAL_THIS_56578be7e9f832b0",
        |mut c: Caller<'_, Host>| c.data_mut().add(JsValue::GlobalThis),
    )?;
    linker.func_wrap(
        M,
        "__wbindgen_throw",
        |c: Caller<'_, Host>, p: i32, l: i32| -> Result<(), wasmi::Error> {
            let mem = host_memory(&c)?;
            Err(trap(read_str(mem.data(&c), p, l)?))
        },
    )?;
    linker.func_wrap(M, "__wbg_require_79b1e9274cde3c87", unsupported("require"))?;
    linker.func_wrap(
        M,
        "__wbg_apply_36be6a55257c99bf",
        |_c: Caller<'_, Host>, _a: i32, _b: i32, _d: i32| -> Result<i32, wasmi::Error> {
            Err(trap("unsupported import apply"))
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_call_7cccdd69e0791ae2",
        |_c: Caller<'_, Host>, _a: i32, _b: i32, _d: i32| -> Result<i32, wasmi::Error> {
            Err(trap("unsupported import call"))
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_call_672a4d21634d4a24",
        |_c: Caller<'_, Host>, _a: i32, _b: i32| -> Result<i32, wasmi::Error> {
            Err(trap("unsupported import call"))
        },
    )?;
    linker.func_wrap(
        M,
        "__wbg_newnoargs_105ed471475aaf50",
        |_c: Caller<'_, Host>, _a: i32, _b: i32| -> Result<i32, wasmi::Error> {
            Err(trap("unsupported import Function()"))
        },
    )?;
    Ok(())
}

/// Result of `zprotoSync2DecryptMetadata`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataDecrypted {
    /// Session id chosen by the sender (base64 text).
    pub session_id: String,
    /// Decrypted chunk (zstd-compressed protobuf).
    pub plaintext: Vec<u8>,
    /// Sender's identity public key (33 bytes).
    pub sender_identity: Vec<u8>,
    /// State needed to decrypt the session's later `Message` batches.
    pub session_record: Vec<u8>,
}

/// Result of `zprotoSync2CreateMetadataCipher` (sender side; used in tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataEncrypted {
    /// Session id (base64 text).
    pub session_id: String,
    /// Encrypted chunk.
    pub ciphertext: Vec<u8>,
    /// Receiver identity the chunk was encrypted to.
    pub receiver_identity: Vec<u8>,
    /// Sender's session state for `zprotoSync2EncryptMessage`.
    pub session_record: Vec<u8>,
}

type Ret = (i32, i32, i32, i32);

/// A loaded instance of `libzproto_wasm_bg`.
pub struct ZprotoWasm {
    store: Store<Host>,
    instance: Instance,
    memory: Memory,
    stack: TypedFunc<i32, i32>,
    malloc: TypedFunc<(i32, i32), i32>,
    free: TypedFunc<(i32, i32, i32), ()>,
}

impl ZprotoWasm {
    /// Loads the module from a file.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Io`] or [`Sync2Error::Wasm`].
    pub fn from_file(path: &Path) -> Result<Self, Sync2Error> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// Loads the module from its bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Wasm`] when the module does not compile or does not
    /// have the expected exports.
    pub fn from_bytes(wasm: &[u8]) -> Result<Self, Sync2Error> {
        let engine = Engine::default();
        let module = Module::new(&engine, wasm)?;
        let mut linker = Linker::<Host>::new(&engine);
        define_imports(&mut linker)?;
        let mut store = Store::new(&engine, Host::new());
        let instance = linker.instantiate_and_start(&mut store, &module)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| Sync2Error::Wasm("no memory export".into()))?;
        store.data_mut().memory = Some(memory);
        let stack = instance.get_typed_func(&store, "__wbindgen_add_to_stack_pointer")?;
        let malloc = instance.get_typed_func(&store, "__wbindgen_export_2")?;
        let free = instance.get_typed_func(&store, "__wbindgen_export_1")?;
        Ok(Self {
            store,
            instance,
            memory,
            stack,
            malloc,
            free,
        })
    }

    fn func<P: wasmi::WasmParams, R: wasmi::WasmResults>(
        &self,
        name: &str,
    ) -> Result<TypedFunc<P, R>, Sync2Error> {
        Ok(self.instance.get_typed_func::<P, R>(&self.store, name)?)
    }

    /// Copies `bytes` into a fresh wasm allocation (ownership moves to wasm).
    fn put(&mut self, bytes: &[u8]) -> Result<(i32, i32), Sync2Error> {
        let len =
            i32::try_from(bytes.len()).map_err(|_| Sync2Error::Wasm("input too large".into()))?;
        let ptr = self.malloc.call(&mut self.store, (len, 1))?;
        let start = ptr as u32 as usize;
        self.memory
            .data_mut(&mut self.store)
            .get_mut(start..start + bytes.len())
            .ok_or_else(|| Sync2Error::Wasm("malloc out of bounds".into()))?
            .copy_from_slice(bytes);
        Ok((ptr, len))
    }

    fn read_ret(&self, ret: i32) -> Result<Ret, Sync2Error> {
        let data = self.memory.data(&self.store);
        let at = ret as u32 as usize;
        let word = |i: usize| -> Result<i32, Sync2Error> {
            let b = data
                .get(at + 4 * i..at + 4 * i + 4)
                .ok_or_else(|| Sync2Error::Wasm("retptr out of bounds".into()))?;
            Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        Ok((word(0)?, word(1)?, word(2)?, word(3)?))
    }

    /// Runs `f` with a 16-byte return slot on the wasm shadow stack.
    fn with_ret<T>(
        &mut self,
        f: impl FnOnce(&mut Self, i32) -> Result<T, Sync2Error>,
    ) -> Result<T, Sync2Error> {
        let ret = self.stack.call(&mut self.store, -16)?;
        let out = f(self, ret);
        self.stack.call(&mut self.store, 16)?;
        out
    }

    /// Turns an error object on the heap into [`Sync2Error::Crypto`].
    fn take_error(&mut self, idx: i32) -> Sync2Error {
        let value = self.store.data_mut().take(idx);
        let field = |name: &str| match &value {
            JsValue::Object(fields) => fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.text()),
            _ => None,
        };
        Sync2Error::Crypto {
            code: field("code").unwrap_or_default(),
            message: field("message").unwrap_or_else(|| value.text()),
        }
    }

    /// Calls a struct getter that returns a `Uint8Array` and copies it out.
    fn getter(&mut self, name: &str, ptr: i32) -> Result<Vec<u8>, Sync2Error> {
        let idx = self.func::<i32, i32>(name)?.call(&mut self.store, ptr)?;
        let value = self.store.data_mut().take(idx);
        Ok(bytes_of(&value, self.memory.data(&self.store))?)
    }

    /// Calls a struct getter that returns a string.
    fn string_getter(&mut self, name: &str, ptr: i32) -> Result<String, Sync2Error> {
        let idx = self.func::<i32, i32>(name)?.call(&mut self.store, ptr)?;
        match self.store.data_mut().take(idx) {
            JsValue::Str(s) => Ok(s),
            other => {
                let bytes = bytes_of(&other, self.memory.data(&self.store))?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
    }

    fn free_struct(&mut self, name: &str, ptr: i32) -> Result<(), Sync2Error> {
        self.func::<(i32, i32), ()>(name)?
            .call(&mut self.store, (ptr, 1))?;
        Ok(())
    }

    /// `zprotoEd25519DerivePublicKey`: 32-byte private key → 33-byte `05‖X25519` public key.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Crypto`] for a bad key.
    pub fn derive_public_key(&mut self, private_key: &[u8]) -> Result<Vec<u8>, Sync2Error> {
        let (p, l) = self.put(private_key)?;
        let f = self.func::<(i32, i32, i32), ()>("zprotoEd25519DerivePublicKey")?;
        self.with_ret(|this, ret| {
            f.call(&mut this.store, (ret, p, l))?;
            let (ptr, len, err, is_err) = this.read_ret(ret)?;
            if is_err != 0 {
                return Err(this.take_error(err));
            }
            let out = slice(this.memory.data(&this.store), ptr as u32, len as u32)?.to_vec();
            this.free.call(&mut this.store, (ptr, len, 1))?;
            Ok(out)
        })
    }

    /// `zprotoSync2DecryptMetadata`: decrypts a frame of a `MetadataEmbedded` batch.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Crypto`] when the frame was not made for these keys.
    pub fn decrypt_metadata(
        &mut self,
        ciphertext: &[u8],
        identity_public: &[u8],
        identity_private: &[u8],
        ephemeral_private: &[u8],
    ) -> Result<MetadataDecrypted, Sync2Error> {
        let a = self.put(ciphertext)?;
        let b = self.put(identity_public)?;
        let c = self.put(identity_private)?;
        let d = self.put(ephemeral_private)?;
        type P = (i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32);
        let f = self.func::<P, ()>("zprotoSync2DecryptMetadata")?;
        self.with_ret(|this, ret| {
            f.call(
                &mut this.store,
                (ret, a.0, a.1, b.0, b.1, c.0, c.1, d.0, d.1, 0, 0),
            )?;
            let (ptr, err, is_err, _) = this.read_ret(ret)?;
            if is_err != 0 {
                return Err(this.take_error(err));
            }
            let out = MetadataDecrypted {
                session_id: this.string_getter("zprotosyncmetadatadecrypted_sessionId", ptr)?,
                plaintext: this.getter("zprotosyncmetadatadecrypted_plaintext", ptr)?,
                sender_identity: this.getter("zprotosyncmetadatadecrypted_senderIdentity", ptr)?,
                session_record: this.getter("zprotosyncmetadatadecrypted_sessionRecord", ptr)?,
            };
            this.free_struct("__wbg_zprotosyncmetadatadecrypted_free", ptr)?;
            Ok(out)
        })
    }

    /// `zprotoSync2DecryptMessage`: decrypts a frame of a `Message` batch.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Crypto`] when the record does not match.
    pub fn decrypt_message(
        &mut self,
        ciphertext: &[u8],
        session_record: &[u8],
    ) -> Result<Vec<u8>, Sync2Error> {
        let a = self.put(ciphertext)?;
        let b = self.put(session_record)?;
        let f = self.func::<(i32, i32, i32, i32, i32), ()>("zprotoSync2DecryptMessage")?;
        self.with_ret(|this, ret| {
            f.call(&mut this.store, (ret, a.0, a.1, b.0, b.1))?;
            let (ptr, err, is_err, _) = this.read_ret(ret)?;
            if is_err != 0 {
                return Err(this.take_error(err));
            }
            let out = this.getter("zprotosyncmessagedecrypted_plaintext", ptr)?;
            this.free_struct("__wbg_zprotosyncmessagedecrypted_free", ptr)?;
            Ok(out)
        })
    }

    /// `zprotoSync2CreateMetadataCipher`: the phone's side of
    /// [`decrypt_metadata`](Self::decrypt_metadata). Only useful to simulate a
    /// phone in tests.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Crypto`] for bad keys.
    pub fn create_metadata_cipher(
        &mut self,
        plaintext: &[u8],
        sender_identity_public: &[u8],
        sender_identity_private: &[u8],
        receiver_identity_public: &[u8],
        receiver_ephemeral_public: &[u8],
    ) -> Result<MetadataEncrypted, Sync2Error> {
        let a = self.put(plaintext)?;
        let b = self.put(sender_identity_public)?;
        let c = self.put(sender_identity_private)?;
        let d = self.put(receiver_identity_public)?;
        let e = self.put(receiver_ephemeral_public)?;
        type P = (
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
            i32,
        );
        let f = self.func::<P, ()>("zprotoSync2CreateMetadataCipher")?;
        self.with_ret(|this, ret| {
            f.call(
                &mut this.store,
                (ret, a.0, a.1, b.0, b.1, c.0, c.1, d.0, d.1, e.0, e.1, 0, 0),
            )?;
            let (ptr, err, is_err, _) = this.read_ret(ret)?;
            if is_err != 0 {
                return Err(this.take_error(err));
            }
            let out = MetadataEncrypted {
                session_id: this.string_getter("zprotosyncmetadataencrypted_sessionId", ptr)?,
                ciphertext: this.getter("zprotosyncmetadataencrypted_ciphertext", ptr)?,
                receiver_identity: this
                    .getter("zprotosyncmetadataencrypted_receiverIdentity", ptr)?,
                session_record: this.getter("zprotosyncmetadataencrypted_sessionRecord", ptr)?,
            };
            this.free_struct("__wbg_zprotosyncmetadataencrypted_free", ptr)?;
            Ok(out)
        })
    }

    /// `zprotoSync2EncryptMessage`: the phone's side of
    /// [`decrypt_message`](Self::decrypt_message). Only useful in tests.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error::Crypto`] for a bad record.
    pub fn encrypt_message(
        &mut self,
        plaintext: &[u8],
        session_record: &[u8],
    ) -> Result<Vec<u8>, Sync2Error> {
        let a = self.put(plaintext)?;
        let b = self.put(session_record)?;
        let f = self.func::<(i32, i32, i32, i32, i32), ()>("zprotoSync2EncryptMessage")?;
        self.with_ret(|this, ret| {
            f.call(&mut this.store, (ret, a.0, a.1, b.0, b.1))?;
            let (ptr, err, is_err, _) = this.read_ret(ret)?;
            if is_err != 0 {
                return Err(this.take_error(err));
            }
            let out = this.getter("zprotosyncmessageencrypted_ciphertext", ptr)?;
            this.free_struct("__wbg_zprotosyncmessageencrypted_free", ptr)?;
            Ok(out)
        })
    }
}

/// Looks for `libzproto_wasm_bg.*.wasm` in `$ZALO_SYNC2_WASM` (a file) or in an
/// `assets/wasm/` folder above `start`.
pub fn find_wasm(start: &Path) -> Option<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("ZALO_SYNC2_WASM") {
        return Some(path.into());
    }
    for dir in start.ancestors() {
        let libs = dir.join("assets/wasm");
        if let Ok(entries) = std::fs::read_dir(&libs) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("libzproto_wasm_bg") && name.ends_with(".wasm") {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}
