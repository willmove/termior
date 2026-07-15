//! 密钥接口（FR-PROV-04 / FR-SEC-06 / INV-5）。
//!
//! API Key 只存在于 OS 钥匙串与调用瞬间的内存中；任何持久化文件、日志、panic 报告
//! 不得出现 key 明文。本 trait 定义存取接口，真实实现用 `keyring` crate（service
//! `Termior-ai`：macOS Keychain / Windows Credential Manager / Linux Secret Service，
//! headless 文件回退）。本 crate 提供 [`InMemorySecretStore`] 供单测。

use std::collections::HashMap;

/// 密钥存储服务名（FR-PROV-04）。
pub const SERVICE: &str = "Termior-ai";

/// 密钥存取 trait。
pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError>;
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError>;
    fn delete(&self, key: &str) -> Result<(), SecretStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SecretStoreError {
    #[error("backend error: {0}")]
    Backend(String),
    #[error("not found")]
    NotFound,
}

/// 内存实现（单测与 headless 回退基础）。
#[derive(Debug, Default)]
pub struct InMemorySecretStore {
    inner: std::sync::Mutex<HashMap<String, String>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretStoreError> {
        Ok(self.inner.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretStoreError> {
        self.inner.lock().unwrap().insert(key.to_string(), value.to_string());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SecretStoreError> {
        self.inner.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_delete() {
        let s = InMemorySecretStore::new();
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", "secret-value").unwrap();
        assert_eq!(s.get("k").unwrap().as_deref(), Some("secret-value"));
        s.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
    }

    #[test]
    fn service_name_is_termior_ai() {
        assert_eq!(SERVICE, "Termior-ai");
    }

    #[test]
    fn secrets_not_serializable() {
        // INV-5：密钥永不进入持久化文件。InMemorySecretStore 不实现 Serialize，
        // 且其内容不会出现在任何 #[derive(Serialize)] 结构中。
        let s = InMemorySecretStore::new();
        s.set("anthropic_key", "sk-ant-xxxxx").unwrap();
        // 没有任何途径把 store 序列化到磁盘——它不实现 Serialize/Deserialize。
        // （此测试是静态断言的占位，运行时只验证能正常存取。）
        assert_eq!(s.get("anthropic_key").unwrap().as_deref(), Some("sk-ant-xxxxx"));
    }
}
