//! On-disk file storage for uploaded artifacts.
//!
//! Files are stored under `$APP_FILES_DIR` (default
//! `/var/lib/outpost/files`) with a two-level fan-out keyed by the
//! sha256 prefix — keeps directory listings small even with thousands
//! of uploads (`aa/bb/aabbccdd…ext`).

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Result of writing a file to local storage.
#[derive(Debug, Clone)]
pub struct StoredFile {
    /// Path relative to the storage root (e.g. `aa/bb/aabb…apk`).
    pub relative_path: String,
    /// Lowercase hex sha256 of the file content.
    pub sha256: String,
    /// File size in bytes.
    pub size: i64,
}

/// Write `bytes` to a content-addressed location under `root`.
/// Returns the path + sha256 + size; idempotent (overwrites if identical
/// hash already present).
pub async fn write_bytes(root: &Path, bytes: &[u8], extension: Option<&str>) -> Result<StoredFile> {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let sha256 = hex::encode(digest);

    let (a, b) = (&sha256[0..2], &sha256[2..4]);
    let mut path = PathBuf::from(root);
    path.push(a);
    path.push(b);
    fs::create_dir_all(&path)
        .await
        .with_context(|| format!("create_dir_all {}", path.display()))?;

    let file_name = match extension {
        Some(ext) if !ext.is_empty() => format!("{sha256}.{ext}"),
        _ => sha256.clone(),
    };
    path.push(&file_name);

    let mut file = fs::File::create(&path)
        .await
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes).await.context("write")?;
    file.sync_all().await.context("sync_all")?;

    let relative_path = format!("{a}/{b}/{file_name}");

    Ok(StoredFile {
        relative_path,
        sha256,
        size: bytes.len() as i64,
    })
}

/// Resolve a `relative_path` (as returned by [`write_bytes`]) to an
/// absolute filesystem path, ensuring it stays under `root` (prevents
/// path-traversal attacks via `..`).
pub fn resolve_under_root(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.contains("..") || relative.starts_with('/') || relative.contains('\\') {
        anyhow::bail!("rejected suspicious relative path: {relative}");
    }
    Ok(root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_then_read_round_trip() {
        let root = tempfile::tempdir().unwrap();
        let stored = write_bytes(root.path(), b"hello world", Some("txt"))
            .await
            .unwrap();
        assert!(stored.relative_path.ends_with(".txt"));
        assert_eq!(stored.size, 11);
        assert_eq!(
            stored.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        let abs = resolve_under_root(root.path(), &stored.relative_path).unwrap();
        let bytes = tokio::fs::read(&abs).await.unwrap();
        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn write_is_content_addressed() {
        let root = tempfile::tempdir().unwrap();
        let a = write_bytes(root.path(), b"same content", None)
            .await
            .unwrap();
        let b = write_bytes(root.path(), b"same content", None)
            .await
            .unwrap();
        assert_eq!(a.sha256, b.sha256);
        assert_eq!(a.relative_path, b.relative_path);
    }

    #[test]
    fn resolve_blocks_path_traversal() {
        let root = std::path::Path::new("/tmp/outpost");
        assert!(resolve_under_root(root, "../etc/passwd").is_err());
        assert!(resolve_under_root(root, "/etc/passwd").is_err());
        assert!(resolve_under_root(root, "ab/..\\cd").is_err());
        assert!(resolve_under_root(root, "ab/cd/file.bin").is_ok());
    }
}
