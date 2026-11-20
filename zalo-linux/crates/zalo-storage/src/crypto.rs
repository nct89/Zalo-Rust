//! AES-256 key derivation and XZ (LZMA2) decompression for Zalo backups.
//!
//! `db-cross-v4` copies the first 32 bytes of `privateKey`, uppercases ASCII
//! `a`-`z`, and uses those bytes as the AES-256 key. It does not hex-decode
//! the string. Ciphertext is AES-256-CBC with a zero IV and no padding. The
//! IV is reset at the start of every 64 KiB chunk.

use aes::Aes256;
use cbc::cipher::generic_array::GenericArray;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};

use crate::error::StorageError;

/// Bytes processed with one IV before that IV is reset to zero.
const CHUNK: usize = 0x1_0000;
const ZERO_IV: [u8; 16] = [0; 16];

/// Builds the AES-256 key Zalo's backup addon uses.
///
/// The first 32 bytes of `private_key` are copied. ASCII `a`–`z` become
/// `A`–`Z`. A shorter string is padded with zero bytes. Bytes past 32 are
/// ignored. The result is raw key material and must not be logged.
#[must_use]
pub fn derive_aes256_key_zalo_v2(private_key: &str) -> [u8; 32] {
    let mut key = [0_u8; 32];
    for (slot, byte) in key.iter_mut().zip(private_key.bytes()) {
        *slot = if byte.is_ascii_lowercase() {
            byte - b'a' + b'A'
        } else {
            byte
        };
    }
    key
}

/// Decrypts AES-256-CBC with no padding, resetting the IV to zero every 64 KiB.
///
/// # Errors
///
/// Returns [`StorageError::NotBlockAligned`] when `ciphertext` is empty or its
/// length is not a multiple of 16, and [`StorageError::AesInit`] when the key
/// cannot initialize AES-256.
pub fn aes256_cbc_decrypt_nopad_chunked(
    ciphertext: &[u8],
    key: &[u8; 32],
) -> Result<Vec<u8>, StorageError> {
    crypt_chunked(ciphertext, key, false)
}

/// Decrypts one AES-256-CBC buffer with a single zero IV and no padding.
///
/// `db-cross-v4` uses this only when the chunked path rejects the buffer.
///
/// # Errors
///
/// Returns [`StorageError::NotBlockAligned`] when `ciphertext` is empty or its
/// length is not a multiple of 16.
pub fn aes256_cbc_decrypt_nopad(
    ciphertext: &[u8],
    key: &[u8; 32],
) -> Result<Vec<u8>, StorageError> {
    let mut buffer = ciphertext.to_vec();
    crypt_inplace(&mut buffer, key, false)?;
    Ok(buffer)
}

/// Encrypts with the same chunked zero-IV AES-256-CBC used to build fixtures.
///
/// # Errors
///
/// Returns [`StorageError::NotBlockAligned`] when `plaintext` is empty or its
/// length is not a multiple of 16.
#[cfg(test)]
pub(crate) fn aes256_cbc_encrypt_nopad_chunked(
    plaintext: &[u8],
    key: &[u8; 32],
) -> Result<Vec<u8>, StorageError> {
    crypt_chunked(plaintext, key, true)
}

/// Decompresses an XZ stream (LZMA2), which follows the ZDB4 file table.
///
/// Trailing zero bytes after the XZ footer are ignored. The stream decoder in
/// `db-cross-v4` stops at the footer, and AES block alignment can leave padding.
///
/// # Errors
///
/// Returns [`StorageError::Lzma`] when the bytes are not an XZ stream.
pub fn decompress_xz_lzma2(data: &[u8]) -> Result<Vec<u8>, StorageError> {
    match xz_once(data) {
        Ok(output) => Ok(output),
        Err(first) => {
            let trimmed = xz_stream_prefix(data);
            if trimmed.len() == data.len() {
                Err(first)
            } else {
                xz_once(trimmed)
            }
        }
    }
}

/// Compresses bytes into an XZ stream for fixtures. Callers only decompress.
///
/// # Errors
///
/// Returns [`StorageError::Lzma`] when the encoder rejects the input.
#[cfg(test)]
pub(crate) fn compress_xz_lzma2(data: &[u8]) -> Result<Vec<u8>, StorageError> {
    let mut output = Vec::new();
    lzma_rs::xz_compress(&mut &data[..], &mut output)
        .map_err(|err| StorageError::Lzma(err.to_string()))?;
    Ok(output)
}

fn xz_once(data: &[u8]) -> Result<Vec<u8>, StorageError> {
    let mut output = Vec::new();
    lzma_rs::xz_decompress(&mut &data[..], &mut output)
        .map_err(|err| StorageError::Lzma(err.to_string()))?;
    Ok(output)
}

/// Drops zero padding that sits after the XZ footer magic `YZ`.
fn xz_stream_prefix(data: &[u8]) -> &[u8] {
    let mut end = data.len();
    while end > 0 && data[end - 1] == 0 {
        end -= 1;
    }
    if end >= 2 && data[end - 2] == 0x59 && data[end - 1] == 0x5a {
        &data[..end]
    } else {
        data
    }
}

fn crypt_chunked(input: &[u8], key: &[u8; 32], encrypt: bool) -> Result<Vec<u8>, StorageError> {
    if input.is_empty() || input.len() % 16 != 0 {
        return Err(StorageError::NotBlockAligned);
    }
    let mut output = Vec::with_capacity(input.len());
    for part in input.chunks(CHUNK) {
        let mut buffer = part.to_vec();
        crypt_inplace(&mut buffer, key, encrypt)?;
        output.extend_from_slice(&buffer);
    }
    Ok(output)
}

fn crypt_inplace(buffer: &mut [u8], key: &[u8; 32], encrypt: bool) -> Result<(), StorageError> {
    if buffer.is_empty() || buffer.len() % 16 != 0 {
        return Err(StorageError::NotBlockAligned);
    }
    if encrypt {
        let mut cipher = cbc::Encryptor::<Aes256>::new_from_slices(key, &ZERO_IV)
            .map_err(|_| StorageError::AesInit)?;
        for chunk in buffer.chunks_mut(16) {
            let mut block = GenericArray::clone_from_slice(chunk);
            cipher.encrypt_block_mut(&mut block);
            chunk.copy_from_slice(&block);
        }
    } else {
        let mut cipher = cbc::Decryptor::<Aes256>::new_from_slices(key, &ZERO_IV)
            .map_err(|_| StorageError::AesInit)?;
        for chunk in buffer.chunks_mut(16) {
            let mut block = GenericArray::clone_from_slice(chunk);
            cipher.decrypt_block_mut(&mut block);
            chunk.copy_from_slice(&block);
        }
    }
    Ok(())
}
