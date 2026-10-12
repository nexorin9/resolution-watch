//! 原子重命名 + 临时目录清理 + 元字段注入。
//!
//! ## 设计目标
//!
//! 导出包必须满足以下三条不变量：
//! 1. **不写半成品**：全链路成功前，`out/<week_id>/` 正式目录不存在或不存在部分文件。
//! 2. **失败不污染**：任何阶段失败，临时目录立即清理（不留 `.tmp/` 残骸）。
//! 3. **元字段一致**：`DEPLOYED_AT` / `WEEK_ID` / `TOOL_VERSION` / `AI_DEGRADED` 注入到所有导出文件的注释头。
//!
//! ## 流程
//!
//! 1. 解析 `out_dir`（目标目录） → 派生 `out_dir/.tmp/<random>` 临时目录
//! 2. 把临时目录路径交给调用方的闭包，让闭包写入所有导出文件
//! 3. 闭包成功返回后，`rename(临时目录, out_dir)` 一次性原子提交
//! 4. 闭包返回 `Err` 或中途 `panic`：清理临时目录（不删目标目录）
//!
//! ## 源产品主路径同构
//!
//! 源 `crates/runtime/src/prod.rs` 的强一致 deterministic 任务提交：失败立即可见、可中断。
//! 本模块把这条思路翻译为「目录原子重命名」：要么 `out/<week_id>/` 不存在，要么它包含完整 6 个文件。

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use thiserror::Error;
use tracing::{debug, warn};

/// 导出包顶部元字段。
///
/// 注入到每个导出文件（CSV 注释行 / JSON 字段）以供下游核对。
/// 字段顺序与 `README.md`「输出样例」保持一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportMetadata {
    /// 导出时间戳（RFC3339，含时区）。
    pub deployed_at: String,
    /// 周次标识符，如 `2026-W41`。
    pub week_id: String,
    /// 工具版本（取自 `CARGO_PKG_VERSION`）。
    pub tool_version: String,
    /// LLM 是否降级（true = 走了 deterministic 解析）。
    pub ai_degraded: bool,
}

impl ExportMetadata {
    /// 从当前时间与周次构造元字段。版本默认取 `CARGO_PKG_VERSION`。
    pub fn now(week_id: impl Into<String>, ai_degraded: bool) -> Self {
        let deployed_at = chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        Self {
            deployed_at,
            week_id: week_id.into(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            ai_degraded,
        }
    }

    /// 覆盖工具版本（测试 / 部署版本注入用）。
    pub fn with_tool_version(mut self, v: impl Into<String>) -> Self {
        self.tool_version = v.into();
        self
    }

    /// 渲染为 CSV 注释头（`# key: value` 形式，4 行）。
    pub fn to_csv_header(&self) -> String {
        format!(
            "# DEPLOYED_AT: {deployed_at}\n\
             # WEEK_ID: {week_id}\n\
             # TOOL_VERSION: {tool_version}\n\
             # AI_DEGRADED: {ai_degraded}\n",
            deployed_at = self.deployed_at,
            week_id = self.week_id,
            tool_version = self.tool_version,
            ai_degraded = self.ai_degraded,
        )
    }

    /// 渲染为 JSON 顶层字段（`__meta` 子对象）。
    pub fn to_json_meta(&self) -> serde_json::Value {
        serde_json::json!({
            "deployed_at": self.deployed_at,
            "week_id": self.week_id,
            "tool_version": self.tool_version,
            "ai_degraded": self.ai_degraded,
        })
    }
}

impl fmt::Display for ExportMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DEPLOYED_AT={} WEEK_ID={} TOOL_VERSION={} AI_DEGRADED={}",
            self.deployed_at, self.week_id, self.tool_version, self.ai_degraded
        )
    }
}

/// 元字段渲染错误（当前不会失败；保留供未来扩展）。
#[derive(Debug, Error)]
pub enum MetadataError {
    /// 时间戳格式错误（预留）。
    #[error("metadata 时间戳格式错误：{0}")]
    InvalidTimestamp(String),
}

/// `write_atomic` 错误：临时目录准备、闭包执行、原子重命名任一阶段失败。
#[derive(Debug, Error)]
pub enum WriteAtomicError {
    /// 父目录（`out/`）无法创建。
    #[error("无法创建父目录 {path}: {source}")]
    CreateParent {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// 临时目录无法创建。
    #[error("无法创建临时目录 {path}: {source}")]
    CreateTemp {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// 闭包执行失败。
    #[error("闭包写入失败：{0}")]
    WriterFailed(String),
    /// 原子重命名失败。
    #[error("原子重命名失败 from={from} to={to}: {source}")]
    Rename {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: io::Error,
    },
    /// 闭包返回 Ok 但目录不存在（用户错误）。
    #[error("闭包返回成功但目录 {path} 不存在")]
    TempDirMissing { path: PathBuf },
    /// 元字段错误。
    #[error("元字段错误：{0}")]
    Metadata(#[from] MetadataError),
}

/// 闭包签名：在 `temp_dir` 内写入所有导出文件。
///
/// 闭包返回 `Ok(())` 表示「临时目录内所有产物已完整写入，可以原子重命名」。
/// 返回 `Err(String)` 触发临时目录清理与 `WriterFailed` 上抛。
pub type AtomicWriter = dyn FnMut(&Path) -> Result<(), String>;

/// 原子写入导出包。
///
/// # 参数
///
/// - `out_dir`：目标目录，如 `out/2026-W41/`。
/// - `writer`：在临时目录内写入全部导出文件的闭包。
///
/// # 返回
///
/// 成功：`Ok(PathBuf)` 返回最终目标目录路径（即 `out_dir`）。
/// 失败：返回 `WriteAtomicError`，临时目录已被清理，目标目录保持原状。
///
/// # 错误语义
///
/// - 父目录创建失败 → 立即报错（`CreateParent`），不动临时目录
/// - 临时目录创建失败 → 立即报错（`CreateTemp`）
/// - 闭包执行失败 → 清理临时目录，报 `WriterFailed`
/// - 闭包返回 Ok 但目录不存在 → 报 `TempDirMissing`（理论上不可能，仅作防御）
/// - 原子重命名失败 → 清理临时目录，报 `Rename`
pub fn write_atomic<F>(out_dir: &Path, mut writer: F) -> Result<PathBuf, WriteAtomicError>
where
    F: FnMut(&Path) -> Result<(), String>,
{
    // 1. 父目录（`out/`）准备
    let parent = out_dir.parent().unwrap_or_else(|| Path::new("."));
    if !parent.as_os_str().is_empty() && !parent.exists() {
        fs::create_dir_all(parent).map_err(|e| WriteAtomicError::CreateParent {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    // 2. 临时目录：`out/.tmp/<random>/`（与 out_dir 同父，确保 fs::rename 在
    //    同父目录下原子提交；macOS fs::rename 不允许跨父目录的目录覆盖）
    let tmp_root = parent.join(".tmp");
    let tmp_dir = make_unique_dir(&tmp_root).map_err(|e| WriteAtomicError::CreateTemp {
        path: tmp_root.join("<random>"),
        source: e,
    })?;
    debug!(tmp_dir = %tmp_dir.display(), "atomic_rename: 临时目录创建成功");

    // 3. 闭包写入（带 RAII 守护：失败清理）
    let result = writer(&tmp_dir);
    match result {
        Ok(()) => {
            if !tmp_dir.exists() {
                let _ = fs::remove_dir_all(&tmp_root);
                return Err(WriteAtomicError::TempDirMissing { path: tmp_dir });
            }

            // 4. 原子重命名（fs::rename 在同文件系统 + 同父目录下是原子的）
            //    如果 out_dir 已存在（覆盖场景），先清空它（保留目录本身）
            if out_dir.exists() {
                if out_dir.is_dir() {
                    let _ = fs::remove_dir_all(out_dir);
                } else {
                    let _ = fs::remove_file(out_dir);
                }
            }
            fs::rename(&tmp_dir, out_dir).map_err(|e| {
                // 重命名失败：清理临时目录
                let _ = fs::remove_dir_all(&tmp_dir);
                let _ = fs::remove_dir_all(&tmp_root);
                WriteAtomicError::Rename {
                    from: tmp_dir.clone(),
                    to: out_dir.to_path_buf(),
                    source: e,
                }
            })?;

            // 顺手清理空的 .tmp/ 父目录（如果存在且空）
            if tmp_root.exists() {
                let _ = fs::remove_dir(&tmp_root);
            }

            debug!(out_dir = %out_dir.display(), "atomic_rename: 原子重命名成功");
            Ok(out_dir.to_path_buf())
        }
        Err(msg) => {
            // 闭包失败：清理临时目录
            warn!(error = %msg, tmp_dir = %tmp_dir.display(), "atomic_rename: 闭包失败，清理临时目录");
            let _ = fs::remove_dir_all(&tmp_dir);
            if tmp_root.exists() {
                let _ = fs::remove_dir(&tmp_root);
            }
            Err(WriteAtomicError::WriterFailed(msg))
        }
    }
}

/// 在 `parent` 下创建一个唯一命名的子目录。
///
/// 用纳秒时间戳 + 进程 id 后缀避免同毫秒内并发创建冲突。
fn make_unique_dir(parent: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(parent)?;
    let nanos = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros() * 1000);
    let pid = std::process::id();
    let name = format!("run-{nanos}-{pid}");
    let path = parent.join(name);
    fs::create_dir(&path)?;
    Ok(path)
}

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn export_metadata_now_fills_all_four_fields() {
        let m = ExportMetadata::now("2026-W41", false);
        assert_eq!(m.week_id, "2026-W41");
        assert!(!m.ai_degraded);
        assert!(!m.deployed_at.is_empty());
        assert!(!m.tool_version.is_empty());
    }

    #[test]
    fn export_metadata_with_tool_version_overrides() {
        let m = ExportMetadata::now("2026-W41", true).with_tool_version("9.9.9-test");
        assert_eq!(m.tool_version, "9.9.9-test");
        assert!(m.ai_degraded);
    }

    #[test]
    fn export_metadata_csv_header_contains_all_four_keys() {
        let m = ExportMetadata {
            deployed_at: "2026-10-12T03:00:00Z".to_string(),
            week_id: "2026-W41".to_string(),
            tool_version: "0.1.0".to_string(),
            ai_degraded: false,
        };
        let header = m.to_csv_header();
        assert!(header.contains("# DEPLOYED_AT: 2026-10-12T03:00:00Z"));
        assert!(header.contains("# WEEK_ID: 2026-W41"));
        assert!(header.contains("# TOOL_VERSION: 0.1.0"));
        assert!(header.contains("# AI_DEGRADED: false"));
        // 4 行
        assert_eq!(header.lines().count(), 4);
    }

    #[test]
    fn export_metadata_json_meta_emits_object() {
        let m = ExportMetadata::now("2026-W41", true);
        let v = m.to_json_meta();
        assert_eq!(v["week_id"], "2026-W41");
        assert_eq!(v["ai_degraded"], true);
        assert!(v["deployed_at"].is_string());
        assert!(v["tool_version"].is_string());
    }

    #[test]
    fn export_metadata_display_contains_all_four_fields() {
        let m = ExportMetadata::now("2026-W41", false);
        let s = format!("{m}");
        assert!(s.contains("WEEK_ID=2026-W41"));
        assert!(s.contains("AI_DEGRADED=false"));
    }

    #[test]
    fn write_atomic_success_renames_temp_to_target() {
        let tmp = TempDir::new().unwrap();
        let out_dir = tmp.path().join("out").join("2026-W41");

        let result = write_atomic(&out_dir, |temp| {
            fs::write(temp.join("hello.txt"), "world").map_err(|e| e.to_string())?;
            Ok(())
        });

        assert!(result.is_ok(), "write_atomic 应该成功: {result:?}");
        let final_path = result.unwrap();
        assert_eq!(final_path, out_dir);
        assert!(out_dir.exists());
        assert_eq!(
            fs::read_to_string(out_dir.join("hello.txt")).unwrap(),
            "world"
        );
        // .tmp/ 父目录应该被清掉
        assert!(!tmp.path().join("out").join(".tmp").exists());
    }

    #[test]
    fn write_atomic_closure_failure_cleans_up_no_half_product() {
        let tmp = TempDir::new().unwrap();
        let out_dir = tmp.path().join("out").join("2026-W41");

        let result = write_atomic(&out_dir, |temp| {
            fs::write(temp.join("partial.txt"), "x").map_err(|e| e.to_string())?;
            // 模拟后续失败
            Err("模拟失败".to_string())
        });

        assert!(matches!(result, Err(WriteAtomicError::WriterFailed(_))));
        // 正式目录绝对不能存在
        assert!(!out_dir.exists(), "正式目录不应存在");
        // .tmp/ 父目录应该被清掉
        assert!(!tmp.path().join("out").join(".tmp").exists(), ".tmp/ 应被清理");
    }

    #[test]
    fn write_atomic_creates_parent_dir_when_missing() {
        let tmp = TempDir::new().unwrap();
        let out_dir = tmp.path().join("deeply").join("nested").join("out").join("2026-W41");

        let result = write_atomic(&out_dir, |temp| {
            fs::write(temp.join("f.txt"), "ok").map_err(|e| e.to_string())
        });

        assert!(result.is_ok());
        assert!(out_dir.exists());
    }

    #[test]
    fn write_atomic_overwrites_existing_target_atomically() {
        let tmp = TempDir::new().unwrap();
        let out_dir = tmp.path().join("out").join("2026-W41");
        // 预先在目标目录写一个文件
        fs::create_dir_all(&out_dir).unwrap();
        fs::write(out_dir.join("old.txt"), "old").unwrap();

        // 第一次 atomic 写入新内容
        let result = write_atomic(&out_dir, |temp| {
            fs::write(temp.join("new.txt"), "new").map_err(|e| e.to_string())
        });
        assert!(result.is_ok());
        // 旧文件被替换
        assert!(!out_dir.join("old.txt").exists());
        assert!(out_dir.join("new.txt").exists());
    }

    #[test]
    fn make_unique_dir_creates_distinct_paths_under_concurrent_calls() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("runs");
        fs::create_dir_all(&parent).unwrap();

        let a = make_unique_dir(&parent).unwrap();
        let b = make_unique_dir(&parent).unwrap();
        // 同一次测试内也保证唯一（纳秒时间戳 + pid）
        assert_ne!(a, b);
        assert!(a.exists());
        assert!(b.exists());
    }

    #[test]
    fn writer_failed_error_carries_original_message() {
        let tmp = TempDir::new().unwrap();
        let out_dir = tmp.path().join("out").join("2026-W41");
        let result = write_atomic(&out_dir, |_| Err("OA 端返回 503".to_string()));
        match result {
            Err(WriteAtomicError::WriterFailed(msg)) => assert_eq!(msg, "OA 端返回 503"),
            other => panic!("应为 WriterFailed，实为 {other:?}"),
        }
    }
}
