//! 导出包 sha256 manifest：每个文件一行 `<sha256_hex>  <filename>`，附 `DEPLOYED_AT` / `WEEK_ID` / `TOOL_VERSION` / `AI_DEGRADED` 元字段。
//!
//! 与 `audit_log.jsonl` 共用同一份 `ExportMetadata`，便于下游核对导入。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::export::atomic_rename::ExportMetadata;

/// sha256 manifest 文件结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sha256Manifest {
    #[serde(flatten)]
    pub meta: Sha256Meta,
    pub entries: Vec<Sha256Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sha256Meta {
    pub deployed_at: String,
    pub week_id: String,
    pub tool_version: String,
    pub ai_degraded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sha256Entry {
    pub path: String,
    pub sha256: String,
}

/// sha256 写盘错误。
#[derive(Debug, Error)]
pub enum Sha256Error {
    #[error("sha256 读取失败 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sha256 manifest 写入失败 {path}: {source}")]
    WriteManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sha256 manifest 序列化失败：{0}")]
    Serialize(String),
}

/// 计算单个文件的 sha256 hex（64 字符小写）。
pub fn sha256_hex_file(path: &Path) -> Result<String, Sha256Error> {
    let bytes = fs::read(path).map_err(|e| Sha256Error::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    Ok(out)
}

/// 构造 manifest：从 `exit_dir` 读出文件名列表 + 各自 sha256。
pub fn build_manifest(
    exit_dir: &Path,
    files: &[&str],
    meta: &ExportMetadata,
) -> Result<Sha256Manifest, Sha256Error> {
    let mut entries = Vec::with_capacity(files.len());
    for name in files {
        let p = exit_dir.join(name);
        let sha = sha256_hex_file(&p)?;
        entries.push(Sha256Entry {
            path: name.to_string(),
            sha256: sha,
        });
    }
    Ok(Sha256Manifest {
        meta: Sha256Meta {
            deployed_at: meta.deployed_at.clone(),
            week_id: meta.week_id.clone(),
            tool_version: meta.tool_version.clone(),
            ai_degraded: meta.ai_degraded,
        },
        entries,
    })
}

/// 写 `sha256_manifest.json`（JSON 格式 + 排序便于核对）。
pub fn write_manifest(
    path: &Path,
    manifest: &Sha256Manifest,
) -> Result<(), Sha256Error> {
    let j = serde_json::to_string_pretty(manifest)
        .map_err(|e| Sha256Error::Serialize(e.to_string()))?;
    fs::write(&path, j).map_err(|e| Sha256Error::WriteManifest {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_of_empty_string_matches_known_value() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("empty.txt");
        fs::write(&p, "").unwrap();
        let h = sha256_hex_file(&p).unwrap();
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_of_known_content_matches_known_value() {
        // SHA-256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("hello.txt");
        fs::write(&p, "hello world").unwrap();
        let h = sha256_hex_file(&p).unwrap();
        assert_eq!(
            h,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn build_and_write_manifest() {
        let t = tempfile::tempdir().unwrap();
        let exit_dir = t.path();
        fs::write(exit_dir.join("resolutions.csv"), "header\nrow1\n").unwrap();
        fs::write(exit_dir.join("summary.json"), "{}").unwrap();
        let meta = ExportMetadata::now("2026-W41", false);
        let m = build_manifest(exit_dir, &["resolutions.csv", "summary.json"], &meta).unwrap();
        assert_eq!(m.entries.len(), 2);
        // entries 按 input 顺序保留
        assert_eq!(m.entries[0].path, "resolutions.csv");
        // 写盘
        let manifest_path = exit_dir.join("sha256_manifest.json");
        write_manifest(&manifest_path, &m).unwrap();
        // roundtrip
        let content = fs::read_to_string(&manifest_path).unwrap();
        let back: Sha256Manifest = serde_json::from_str(&content).unwrap();
        assert_eq!(back.entries.len(), 2);
    }

    #[test]
    fn sha256_stable_for_same_content() {
        let t = tempfile::tempdir().unwrap();
        let p1 = t.path().join("a.txt");
        let p2 = t.path().join("b.txt");
        fs::write(&p1, "same content").unwrap();
        fs::write(&p2, "same content").unwrap();
        assert_eq!(sha256_hex_file(&p1).unwrap(), sha256_hex_file(&p2).unwrap());
    }
}