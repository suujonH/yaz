//! Encrypts sensitive text such as the API key and password items in plugin settings with DPAPI
//! (current Windows user).

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

use crate::error::{Result, StoreError};

pub fn protect_key(api_key: &str) -> Result<Vec<u8>> {
    let input = blob(api_key.as_bytes());
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input points to the bytes of api_key and is valid for the duration of the call; on
    // success the system allocates output, which take_blob frees.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| StoreError::Crypto(e.message()))?;
        Ok(take_blob(output))
    }
}

/// Returns [`StoreError::Crypto`] if the data was not encrypted by the current Windows user or is
/// corrupt.
pub fn unprotect_key(data: &[u8]) -> Result<String> {
    let input = blob(data);
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: same as protect_key.
    let plain = unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| StoreError::Crypto(e.message()))?;
        take_blob(output)
    };
    String::from_utf8(plain).map_err(|e| StoreError::Crypto(e.to_string()))
}

fn blob(bytes: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    }
}

/// Copies the system-allocated data and frees the original memory.
///
/// # Safety
/// `blob` must be the output of a successful DPAPI call.
unsafe fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller guarantees pbData points to valid memory of cbData bytes.
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    // SAFETY: pbData was allocated by the system with LocalAlloc.
    unsafe { LocalFree(Some(HLOCAL(blob.pbData.cast()))) };
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_does_not_store_plain_text() {
        let encrypted = protect_key("secret-key").unwrap();
        assert!(!encrypted.is_empty());
        assert!(!encrypted.windows(10).any(|w| w == b"secret-key"));
        assert_eq!(unprotect_key(&encrypted).unwrap(), "secret-key");
    }

    #[test]
    fn rejects_foreign_data() {
        assert!(matches!(
            unprotect_key(b"not dpapi data"),
            Err(StoreError::Crypto(_))
        ));
        assert!(matches!(unprotect_key(&[]), Err(StoreError::Crypto(_))));
    }

    #[test]
    fn handles_unicode() {
        let encrypted = protect_key("密钥-key").unwrap();
        assert_eq!(unprotect_key(&encrypted).unwrap(), "密钥-key");
    }
}
