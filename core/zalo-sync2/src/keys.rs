//! Signal-style key pairs used by sync v2.
//!
//! A private key is 32 random bytes; the public key is `0x05 ‖ X25519(private)`
//! (33 bytes). Zalo PC makes one identity pair (`ik`) per app run and one
//! ephemeral pair (`ek`) per sync session.

use base64::Engine as _;
use rand::RngCore;

use crate::wasm::ZprotoWasm;
use crate::Sync2Error;

/// One key pair.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyPair {
    /// 33-byte public key.
    pub public: Vec<u8>,
    /// 32-byte private key.
    pub private: Vec<u8>,
}

impl std::fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyPair")
            .field("public", &self.public_base64())
            .finish_non_exhaustive()
    }
}

impl KeyPair {
    /// Makes a new random pair. The public key is computed by the wasm module so
    /// it matches Zalo's encoding byte for byte.
    ///
    /// # Errors
    ///
    /// Returns [`Sync2Error`] when the wasm call fails.
    pub fn generate(wasm: &mut ZprotoWasm) -> Result<Self, Sync2Error> {
        let mut private = vec![0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut private);
        let public = wasm.derive_public_key(&private)?;
        Ok(Self { public, private })
    }

    /// Public key as sent in `ek` / `ik`.
    pub fn public_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(&self.public)
    }
}

/// The two pairs of one sync session.
#[derive(Clone, Debug)]
pub struct SyncKeys {
    /// Identity pair (`ik`).
    pub identity: KeyPair,
    /// Ephemeral pair (`ek`).
    pub ephemeral: KeyPair,
}
