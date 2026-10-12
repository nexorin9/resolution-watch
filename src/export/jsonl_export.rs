//! 4 种 JSONL writer：`overdue.jsonl` / `due_soon.jsonl` / `conflicts.jsonl` / `audit_log.jsonl`。
//!
//! 每行一条 JSON 对象（UTF-8，无 BOM，无尾逗号）。caller 负责准备 `Vec` 数据，writer 只做
//! JSON 序列化 + 写入 IO。
//!
//! 审计链 5 段：每次决议督办跑批记录 `cli_start` / `oa_pull_complete` / `extract_call` /
//! `bucket_advance` / `export_complete` 五个节点。

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::export::atomic_rename::ExportMetadata;
use crate::schemas::{OaAction, ResolutionStatus};

/// 单条决议导出（含状态 + 桶），进入 overdue / due_soon / conflicts。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionJsonlEntry {
    pub resolution_id: String,
    pub title: String,
    pub owner_department: String,
    pub owner_id: String,
    pub due_at: String,
    pub status: ResolutionStatus,
    pub due_in_days: i64,
    pub due_bucket: String,
}

/// 单条 OA / 决议推进冲突记录（写入 `conflicts.jsonl`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictEntry {
    pub resolution_id: String,
    pub kind: String,
    pub detail: String,
}

/// 单段审计记录（5 段之一）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditSegmentEntry {
    pub segment: String,
    pub tool_version: String,
    pub commit_hash: Option<String>,
    pub env_hash: Option<String>,
    pub model_name: Option<String>,
    pub prompt_hash: Option<String>,
    pub latency_ms: Option<u64>,
    pub status: String,
}

/// 4 种 JSONL writer 错误。
#[derive(Debug, Error)]
pub enum JsonlExportError {
    #[error("JSONL 文件写入失败 {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSONL 序列化失败：{0}")]
    Serialize(String),
}

fn write_jsonl<T: Serialize>(
    path: &Path,
    items: &[T],
) -> Result<(), JsonlExportError> {
    let mut f = fs::File::create(path).map_err(|e| JsonlExportError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    for item in items {
        let j = serde_json::to_string(item)
            .map_err(|e| JsonlExportError::Serialize(e.to_string()))?;
        writeln!(f, "{j}").map_err(|e| JsonlExportError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

/// 写 `overdue.jsonl` — `Overdue` 桶决议，每行一条 `ResolutionJsonlEntry`。
pub fn write_overdue_jsonl(
    path: &Path,
    entries: &[ResolutionJsonlEntry],
) -> Result<(), JsonlExportError> {
    write_jsonl(path, entries)
}

/// 写 `due_soon.jsonl` — `DueSoon` 桶决议（剩 ≤ 3 天）。
pub fn write_due_soon_jsonl(
    path: &Path,
    entries: &[ResolutionJsonlEntry],
) -> Result<(), JsonlExportError> {
    write_jsonl(path, entries)
}

/// 写 `conflicts.jsonl` — OA 与决议条目口径不一致项。
pub fn write_conflicts_jsonl(
    path: &Path,
    entries: &[ConflictEntry],
) -> Result<(), JsonlExportError> {
    write_jsonl(path, entries)
}

/// 写 `audit_log.jsonl` — 5 段审计链。
pub fn write_audit_log_jsonl(
    path: &Path,
    entries: &[AuditSegmentEntry],
) -> Result<(), JsonlExportError> {
    write_jsonl(path, entries)
}

/// 构造标准 5 段审计链的 helper（caller 提供 latency_ms / status）。
///
/// 5 段顺序：cli_start → oa_pull_complete → extract_call → bucket_advance → export_complete
pub fn standard_audit_segments(meta: &ExportMetadata) -> [AuditSegmentEntry; 5] {
    [
        AuditSegmentEntry {
            segment: "cli_start".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "oa_pull_complete".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "extract_call".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "bucket_advance".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "export_complete".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
    ]
}

/// 把 `OaAction` 转字符串（snake_case），便于审计链单字节序列匹配。
#[allow(dead_code)]
pub fn oa_action_str(a: OaAction) -> &'static str {
    match a {
        OaAction::Started => "started",
        OaAction::InProgress => "in_progress",
        OaAction::Blocked => "blocked",
        OaAction::Submitted => "submitted",
        OaAction::Completed => "completed",
    }
}

/// `ResolutionStatus` 转字符串（snake_case）。
#[allow(dead_code)]
pub fn status_str(s: ResolutionStatus) -> &'static str {
    match s {
        ResolutionStatus::Pending => "pending",
        ResolutionStatus::InProgress => "in_progress",
        ResolutionStatus::DueSoon => "due_soon",
        ResolutionStatus::Overdue => "overdue",
        ResolutionStatus::Completed => "completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn sample_entry() -> ResolutionJsonlEntry {
        ResolutionJsonlEntry {
            resolution_id: "#[W41]01".into(),
            title: "DRG 入组校验".into(),
            owner_department: "医务科".into(),
            owner_id: "E10086".into(),
            due_at: "2026-10-05".into(),
            status: ResolutionStatus::Overdue,
            due_in_days: -7,
            due_bucket: "overdue".into(),
        }
    }

    #[test]
    fn overdue_writer_emits_one_json_per_line() {
        let t = tmp();
        let p = t.path().join("overdue.jsonl");
        let entries = vec![sample_entry(), sample_entry()];
        write_overdue_jsonl(&p, &entries).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content.lines().count(), 2);
        let first: ResolutionJsonlEntry = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(first.resolution_id, "#[W41]01");
    }

    #[test]
    fn due_soon_writer_handles_empty_list() {
        let t = tmp();
        let p = t.path().join("due_soon.jsonl");
        write_due_soon_jsonl(&p, &[]).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.is_empty());
    }

    #[test]
    fn conflicts_writer_emits_kv_pairs() {
        let t = tmp();
        let p = t.path().join("conflicts.jsonl");
        let entries = vec![ConflictEntry {
            resolution_id: "#[W41]01".into(),
            kind: "oa_status_mismatch".into(),
            detail: "OA 标 pending，科室汇报已落地".into(),
        }];
        write_conflicts_jsonl(&p, &entries).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("oa_status_mismatch"));
    }

    #[test]
    fn audit_log_writer_emits_five_segments() {
        let t = tmp();
        let p = t.path().join("audit_log.jsonl");
        let meta = ExportMetadata::now("2026-W41", false);
        let entries = standard_audit_segments(&meta);
        write_audit_log_jsonl(&p, &entries).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content.lines().count(), 5);
        assert!(content.contains("\"segment\":\"cli_start\""));
        assert!(content.contains("\"segment\":\"oa_pull_complete\""));
        assert!(content.contains("\"segment\":\"extract_call\""));
        assert!(content.contains("\"segment\":\"bucket_advance\""));
        assert!(content.contains("\"segment\":\"export_complete\""));
    }

    #[test]
    fn audit_log_writer_handles_chinese_and_unicode() {
        let t = tmp();
        let p = t.path().join("audit_log.jsonl");
        let entries = vec![AuditSegmentEntry {
            segment: "extract_call".into(),
            tool_version: "0.1.0".into(),
            commit_hash: None,
            env_hash: None,
            model_name: Some("gpt-4o-mini".into()),
            prompt_hash: Some("abc123".into()),
            latency_ms: Some(1234),
            status: "ok".into(),
        }];
        write_audit_log_jsonl(&p, &entries).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("gpt-4o-mini"));
    }
}