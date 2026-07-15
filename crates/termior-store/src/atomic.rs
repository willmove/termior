//! 原子写（FR-DATA：全部原子写，temp + rename）。
//!
//! 先写入同目录临时文件，fsync 后 rename 覆盖目标，保证崩溃后文件完好。

use std::io::Write;
use std::path::Path;

/// 原子写错误。
#[derive(Debug, thiserror::Error)]
pub enum AtomicWriteError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("persist temp file failed: {0}")]
    Persist(String),
}

/// 原子写入文本到 `path`。
///
/// 流程：在 `path` 同目录创建临时文件 → 写入 → flush+sync → rename 覆盖。
/// 同目录保证 rename 是原子操作（同文件系统）。
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), AtomicWriteError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    // 用 tempfile 在同目录建临时文件，确保同文件系统 rename。
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?; // 真实运行时保证数据落盘
    tmp.flush()?;

    // 持久化：rename 覆盖目标
    persist_temp(tmp.into_temp_path(), path)
}

fn persist_temp(temp: tempfile::TempPath, target: &Path) -> Result<(), AtomicWriteError> {
    // Windows 上 rename 覆盖已存在文件可能失败；用 with_renamed 重试语义。
    match persist_named(temp, target) {
        Ok(()) => Ok(()),
        Err(e) => Err(AtomicWriteError::Persist(format!("{target:?}: {e}"))),
    }
}

fn persist_named(temp: tempfile::TempPath, target: &Path) -> Result<(), std::io::Error> {
    // tempfile 的 TempPath::persist 会做 rename；在 Windows 上若目标存在会先尝试删除。
    temp.persist(target).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

/// 读取一个文件为字符串（便捷封装）。
pub fn read_text(path: &Path) -> Result<String, std::io::Error> {
    std::fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        atomic_write(&path, r#"{"k":1}"#).unwrap();
        assert_eq!(read_text(&path).unwrap(), r#"{"k":1}"#);
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/settings.json");
        atomic_write(&path, "x").unwrap();
        assert_eq!(read_text(&path).unwrap(), "x");
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        atomic_write(&path, "first").unwrap();
        atomic_write(&path, "second").unwrap();
        assert_eq!(read_text(&path).unwrap(), "second");
    }

    #[test]
    fn atomic_write_no_partial_on_success() {
        // 写入大文本，成功后读取应完整
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let big = "A".repeat(100_000);
        atomic_write(&path, &big).unwrap();
        assert_eq!(read_text(&path).unwrap().len(), 100_000);
    }
}
