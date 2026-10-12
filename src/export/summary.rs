//! `summary.json` 4+1 桶统计：总数 / 按状态分桶 / 按科室分桶 / 按剩余天数 6 桶 / AI 降级标记。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::export::atomic_rename::ExportMetadata;
use crate::schemas::{DueBucket as Bucket, ResolutionStatus};

/// summary.json 内容结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    /// 元字段（同 `ExportMetadata`）。
    #[serde(flatten)]
    pub meta: SummaryMeta,
    /// 总决议条数。
    pub total: usize,
    /// 按状态分桶（5 态：pending / in_progress / due_soon / overdue / completed）。
    pub by_status: BTreeMap<String, usize>,
    /// 按科室分桶。
    pub by_department: BTreeMap<String, usize>,
    /// 按剩余天数分桶（6 桶：lt3 / lt7 / lt14 / lt30 / gt30 / overdue）。
    pub by_due_bucket: BTreeMap<String, usize>,
    /// 逾期条数（由 by_due_bucket["overdue"] 取，冗余便于网关）。
    pub overdue_count: usize,
    /// 临近 deadline 条数（≤ 3 天；冗余便于网关）。
    pub due_soon_count: usize,
}

/// summary 内嵌的元字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryMeta {
    pub deployed_at: String,
    pub week_id: String,
    pub tool_version: String,
    pub ai_degraded: bool,
}

/// summary 写盘错误。
#[derive(Debug, Error)]
pub enum SummaryError {
    #[error("summary.json 写入失败 {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("summary.json 序列化失败：{0}")]
    Serialize(String),
}

/// 一行决议（用于构造 summary），最少需要：resolution_id + owner_department + status + bucket。
pub struct SummaryInputRow<'a> {
    pub resolution_id: &'a str,
    pub owner_department: &'a str,
    pub status: ResolutionStatus,
    pub due_bucket: Bucket,
}

/// 从输入行 + 元数据构造 `Summary`。
pub fn build_summary<'a>(
    meta: &ExportMetadata,
    rows: impl IntoIterator<Item = SummaryInputRow<'a>>,
) -> Summary {
    let mut total = 0usize;
    let mut by_status: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_department: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_due_bucket: BTreeMap<String, usize> = BTreeMap::new();
    for row in rows {
        total += 1;
        *by_status.entry(row.status.as_str().to_string()).or_insert(0) += 1;
        *by_department
            .entry(row.owner_department.to_string())
            .or_insert(0) += 1;
        *by_due_bucket
            .entry(bucket_as_str(row.due_bucket).to_string())
            .or_insert(0) += 1;
    }
    let overdue_count = by_due_bucket.get("overdue").copied().unwrap_or(0);
    let due_soon_count = by_due_bucket.get("lt3").copied().unwrap_or(0);
    Summary {
        meta: SummaryMeta {
            deployed_at: meta.deployed_at.clone(),
            week_id: meta.week_id.clone(),
            tool_version: meta.tool_version.clone(),
            ai_degraded: meta.ai_degraded,
        },
        total,
        by_status,
        by_department,
        by_due_bucket,
        overdue_count,
        due_soon_count,
    }
}

/// 写 `summary.json` 到指定路径。
pub fn write_summary(path: &Path, summary: &Summary) -> Result<(), SummaryError> {
    let j = serde_json::to_string_pretty(summary)
        .map_err(|e| SummaryError::Serialize(e.to_string()))?;
    fs::write(path, j).map_err(|e| SummaryError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn bucket_as_str(b: Bucket) -> &'static str {
    match b {
        Bucket::Lt3 => "lt3",
        Bucket::Lt7 => "lt7",
        Bucket::Lt14 => "lt14",
        Bucket::Lt30 => "lt30",
        Bucket::Gt30 => "gt30",
        Bucket::Overdue => "overdue",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schemas::ResolutionStatus::*;

    fn meta() -> ExportMetadata {
        ExportMetadata::now("2026-W41", false)
    }

    #[test]
    fn build_summary_counts_status_and_bucket() {
        let rows = vec![
            SummaryInputRow {
                resolution_id: "r1",
                owner_department: "医务科",
                status: Pending,
                due_bucket: Bucket::Lt3,
            },
            SummaryInputRow {
                resolution_id: "r2",
                owner_department: "医务科",
                status: InProgress,
                due_bucket: Bucket::Lt7,
            },
            SummaryInputRow {
                resolution_id: "r3",
                owner_department: "信息科",
                status: Overdue,
                due_bucket: Bucket::Overdue,
            },
        ];
        let s = build_summary(&meta(), rows);
        assert_eq!(s.total, 3);
        assert_eq!(s.by_status.get("pending").copied(), Some(1));
        assert_eq!(s.by_status.get("in_progress").copied(), Some(1));
        assert_eq!(s.by_status.get("overdue").copied(), Some(1));
        assert_eq!(s.by_department.get("医务科").copied(), Some(2));
        assert_eq!(s.by_department.get("信息科").copied(), Some(1));
        assert_eq!(s.by_due_bucket.get("overdue").copied(), Some(1));
        assert_eq!(s.overdue_count, 1);
        assert_eq!(s.due_soon_count, 1);
    }

    #[test]
    fn write_summary_emits_valid_json() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("summary.json");
        let s = build_summary(&meta(), std::iter::empty());
        write_summary(&p, &s).unwrap();
        let content = fs::read_to_string(&p).unwrap();
        let back: Summary = serde_json::from_str(&content).unwrap();
        assert_eq!(back.total, 0);
        assert_eq!(back.meta.week_id, "2026-W41");
    }

    #[test]
    fn summary_empty_input_has_zero_total() {
        let s = build_summary(&meta(), std::iter::empty());
        assert_eq!(s.total, 0);
        assert!(s.by_status.is_empty());
        assert_eq!(s.overdue_count, 0);
        assert_eq!(s.due_soon_count, 0);
    }
}