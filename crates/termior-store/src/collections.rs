//! Type-safe atomic JSON files from spec section 7.

use crate::{assert_no_persistent_secret, atomic_write, AtomicWriteError, PersistentSecretError};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

pub const SETTINGS_FILE: &str = "Termior-settings.json";
pub const SESSIONS_FILE: &str = "Termior-ai-sessions.json";
pub const AGENTS_FILE: &str = "Termior-ai-agents.json";
pub const SNIPPETS_FILE: &str = "Termior-ai-snippets.json";
pub const TODOS_FILE: &str = "Termior-ai-todos.json";
pub const CUSTOM_THEMES_FILE: &str = "Termior-custom-themes.json";
pub const WORKSPACE_LAYOUT_FILE: &str = "Termior-workspaces.json";

#[derive(Debug, thiserror::Error)]
pub enum JsonStoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Secret(#[from] PersistentSecretError),
    #[error(transparent)]
    Atomic(#[from] AtomicWriteError),
}

#[derive(Debug, Clone)]
pub struct JsonStore<T> {
    path: PathBuf,
    _value: PhantomData<T>,
}

impl<T> JsonStore<T>
where
    T: Serialize + DeserializeOwned + Default,
{
    pub fn new(data_dir: impl AsRef<Path>, filename: &str) -> Self {
        Self {
            path: data_dir.as_ref().join(filename),
            _value: PhantomData,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<T, JsonStoreError> {
        if !self.path.exists() {
            return Ok(T::default());
        }
        let raw = std::fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, value: &T) -> Result<(), JsonStoreError> {
        let raw = serde_json::to_string_pretty(value)?;
        assert_no_persistent_secret(&raw)?;
        atomic_write(&self.path, &raw)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DataFiles {
    root: PathBuf,
}

impl DataFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, SETTINGS_FILE)
    }

    pub fn sessions<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, SESSIONS_FILE)
    }

    pub fn agents<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, AGENTS_FILE)
    }

    pub fn snippets<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, SNIPPETS_FILE)
    }

    pub fn todos<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, TODOS_FILE)
    }

    pub fn themes<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, CUSTOM_THEMES_FILE)
    }

    pub fn layouts<T>(&self) -> JsonStore<T>
    where
        T: Serialize + DeserializeOwned + Default,
    {
        JsonStore::new(&self.root, WORKSPACE_LAYOUT_FILE)
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.root.join("themes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Value {
        names: Vec<String>,
    }

    #[test]
    fn all_named_files_are_under_data_root() {
        let dir = tempfile::tempdir().unwrap();
        let files = DataFiles::new(dir.path());
        assert_eq!(
            files.sessions::<Value>().path(),
            dir.path().join(SESSIONS_FILE)
        );
        assert_eq!(files.agents::<Value>().path(), dir.path().join(AGENTS_FILE));
        assert_eq!(
            files.snippets::<Value>().path(),
            dir.path().join(SNIPPETS_FILE)
        );
        assert_eq!(files.todos::<Value>().path(), dir.path().join(TODOS_FILE));
        assert_eq!(
            files.themes::<Value>().path(),
            dir.path().join(CUSTOM_THEMES_FILE)
        );
    }

    #[test]
    fn generic_store_roundtrips_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::<Value>::new(dir.path(), TODOS_FILE);
        assert_eq!(store.load().unwrap(), Value::default());
        let value = Value {
            names: vec!["ship".into()],
        };
        store.save(&value).unwrap();
        assert_eq!(store.load().unwrap(), value);
    }

    #[test]
    fn generic_store_rejects_secret_like_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::<Value>::new(dir.path(), SETTINGS_FILE);
        let value = Value {
            names: vec!["sk-abcdefghijklmnopqrstuvwxyz123456".into()],
        };
        assert!(store.save(&value).is_err());
        assert!(!store.path().exists());
    }
}
