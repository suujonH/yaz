//! 用 DPAPI（当前 Windows 用户）加密 API key、插件设置中的密码项等敏感文本。

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

use crate::error::{Result, StoreError};

pub fn protect_key(api_key: &str) -> Result<Vec<u8>> {
    let input = blob(api_key.as_bytes());
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input 指向 api_key 的字节且在调用期间有效；成功后 output 由系统分配，由 take_blob 释放。
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

/// 数据不是当前 Windows 用户加密的，或已损坏时返回 [`StoreError::Crypto`]。
pub fn unprotect_key(data: &[u8]) -> Result<String> {
    let input = blob(data);
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: 同 protect_key。
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

/// 复制系统分配的数据并释放原内存。
///
/// # Safety
/// `blob` 必须是 DPAPI 成功返回的输出。
unsafe fn take_blob(blob: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: 调用方保证 pbData 指向 cbData 字节的有效内存。
    let bytes = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    // SAFETY: pbData 由系统用 LocalAlloc 分配。
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
