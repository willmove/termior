//! OS 钥匙串后端的 `SecretStore` 实现（FR-PROV-04 / FR-SEC-06 / INV-5）。
//!
//! 仅在 `keyring-backend` feature 开启时编译。后端：
//! - macOS Keychain（service `Termior-ai`）
//! - Windows Credential Manager
//! - Linux Secret Service（D-Bus）
//!
//! 密钥只进钥匙串与调用瞬间的内存；任何持久化文件、日志、panic 报告不出现 key 明文（INV-5）。

#![cfg(feature = "keyring-backend")]

use crate::secret_store::{SecretStore, SecretStoreError, SERVICE};

/// keyring 后端的 `SecretStore`。
pub struct KeyringSecretStore;

impl KeyringSecretStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, SecretStoreError> {
        keyring::Entry::new(SERVICE, key).map_err(|e| SecretStoreError::Backend(e.to_string()))
    }
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        let entry = self.entry(key)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretStoreError::Backend(e.to_string())),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        let entry = self.entry(key)?;
        entry
            .set_password(value)
            .map_err(|e| SecretStoreError::Backend(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        let entry = self.entry(key)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretStoreError::Backend(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 这组测试会真正触碰 OS 钥匙串。在 CI/headless 环境里钥匙串可能不可用（返回
    /// Backend 错误），此时跳过而非失败。本机有钥匙串时验证存取往返。
    #[test]
    fn keyring_roundtrip_or_skip() {
        let store = KeyringSecretStore::new();
        let key = "termior_test_roundtrip";
        // 先清理可能残留
        let _ = store.delete(key);
        match store.set(key, "secret-value") {
            Ok(()) => {
                assert_eq!(store.get(key).unwrap().as_deref(), Some("secret-value"));
                store.delete(key).unwrap();
                assert!(store.get(key).unwrap().is_none());
            }
            Err(SecretStoreError::Backend(_)) => {
                // 钥匙串不可用（headless/CI）：跳过
            }
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }

    #[test]
    fn get_missing_returns_none_or_skip() {
        let store = KeyringSecretStore::new();
        let key = "termior_test_definitely_missing_xyz";
        let _ = store.delete(key);
        match store.get(key) {
            Ok(v) => assert!(v.is_none()),
            Err(SecretStoreError::Backend(_)) => {}
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
}
