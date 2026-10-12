//! `resolutions.csv` 列定义 + 6 必填 + 3 衍生列写入。
//!
//! ## 列定义（9 列固定）
//!
//! | # | 列名 | 来源 | 类型 | 备注 |
//! |---|------|------|------|------|
//! | 1 | `resolution_id` | 必填 | String | 周内唯一，派生式 `#[WL]'NN` |
//! | 2 | `title` | 必填 | String | 决议标题 |
//! | 3 | `owner_department` | 必填 | String | 责任科室 |
//! | 4 | `owner_id` | 必填 | String | 责任人 / 工号 |
//! | 5 | `due_at` | 必填 | `YYYY-MM-DD` | 截止日期 |
//! | 6 | `acceptance` | 必填 | String | 验收标准 |
//! | 7 | `status` | 衍生 | snake_case enum | pending / in_progress / due_soon / overdue / completed |
//! | 8 | `due_in_days` | 衍生 | i64 | 当前日期距 `due_at` 的天数（负值 = 已逾期） |
//! | 9 | `due_bucket` | 衍生 | snake_case enum | lt3 / lt7 / lt14 / lt30 / gt30 / overdue |
//!
//! ## 关键不变量
//!
//! - **列顺序固定**：下游 OA / 文档系统按位置取列；不得改顺序
//! - **元字段头**：CSV 顶部 4 行注释头（`DEPLOYED_AT` / `WEEK_ID` / `TOOL_VERSION` / `AI_DEGRADED`）
//! - **数值列空值**：决议状态未派生时，3 衍生列以空字符串占位（`status=""` / `due_in_days=""` / `due_bucket=""`）
//! - **逗号/换行转义**：使用 Rust 标准库的 CSV 写入器（自动 RFC 4180 转义）

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schemas::{DueBucket, Resolution, ResolutionStatus};

use super::atomic_rename::ExportMetadata;

/// `resolutions.csv` 表头（9 列固定，逗号分隔）。
pub const RESOLUTIONS_HEADER: &str =
    "resolution_id,title,owner_department,owner_id,due_at,acceptance,status,due_in_days,due_bucket";

/// 单行决议导出记录：6 必填 + 3 衍生列的扁平化视图。
///
/// 用于在 `extract` / `bucket` 之间传递「已经派生好状态与桶」的决议行。
/// 3 衍生列可空（未派生时为空字符串），便于在导出阶段按可用度渐进填充。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionRow {
    /// 决议编号（必填）。
    pub resolution_id: String,
    /// 决议标题（必填）。
    pub title: String,
    /// 责任科室（必填）。
    pub owner_department: String,
    /// 责任人 / 工号（必填）。
    pub owner_id: String,
    /// 截止日期（必填）。
    pub due_at: NaiveDate,
    /// 验收标准（必填）。
    pub acceptance: String,
    /// 当前状态（衍生；未派生时为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ResolutionStatus>,
    /// 剩余天数（衍生；负值 = 逾期）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_in_days: Option<i64>,
    /// 剩余天数分桶（衍生）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_bucket: Option<DueBucket>,
}

impl ResolutionRow {
    /// 从 `Resolution` 构造（衍生列均为 `None`，调用方可后续填充）。
    pub fn from_resolution(r: &Resolution) -> Self {
        Self {
            resolution_id: r.resolution_id.clone(),
            title: r.title.clone(),
            owner_department: r.owner_department.clone(),
            owner_id: r.owner_id.clone(),
            due_at: r.due_at,
            acceptance: r.acceptance.clone(),
            status: None,
            due_in_days: None,
            due_bucket: None,
        }
    }
}

impl fmt::Display for ResolutionRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 调试用：单行紧凑表示
        write!(
            f,
            "{}|{}|{}|{}|{}|{}|status={:?}|days={:?}|bucket={:?}",
            self.resolution_id,
            self.title,
            self.owner_department,
            self.owner_id,
            self.due_at,
            self.acceptance,
            self.status,
            self.due_in_days,
            self.due_bucket,
        )
    }
}

/// `ResolutionRow` 链式构造器：在 `extract` 后 `bucket` 前逐步填充衍生列。
#[derive(Debug)]
pub struct ResolutionRowBuilder {
    row: ResolutionRow,
}

impl Default for ResolutionRowBuilder {
    fn default() -> Self {
        Self {
            row: ResolutionRow {
                resolution_id: String::new(),
                title: String::new(),
                owner_department: String::new(),
                owner_id: String::new(),
                due_at: NaiveDate::from_ymd_opt(1970, 1, 1)
                    .expect("1970-01-01 is a valid NaiveDate"),
                acceptance: String::new(),
                status: None,
                due_in_days: None,
                due_bucket: None,
            },
        }
    }
}

impl ResolutionRowBuilder {
    /// 从 `Resolution` 起步。
    pub fn from_resolution(r: &Resolution) -> Self {
        Self {
            row: ResolutionRow::from_resolution(r),
        }
    }

    /// 设置状态。
    pub fn status(mut self, s: ResolutionStatus) -> Self {
        self.row.status = Some(s);
        self
    }

    /// 设置剩余天数。
    pub fn due_in_days(mut self, days: i64) -> Self {
        self.row.due_in_days = Some(days);
        self
    }

    /// 设置剩余天数分桶。
    pub fn due_bucket(mut self, b: DueBucket) -> Self {
        self.row.due_bucket = Some(b);
        self
    }

    /// 一步到位：同时设置三个衍生列。
    pub fn derived(mut self, s: ResolutionStatus, days: i64, b: DueBucket) -> Self {
        self.row.status = Some(s);
        self.row.due_in_days = Some(days);
        self.row.due_bucket = Some(b);
        self
    }

    /// 取出构造好的行。
    pub fn build(self) -> ResolutionRow {
        self.row
    }
}

/// CSV 导出错误。
#[derive(Debug, Error)]
pub enum CsvExportError {
    /// 文件 IO 错误。
    #[error("CSV 文件写入失败 {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// CSV 序列化错误（标准库 csv crate 不返回 std::error::Error，故预留）。
    #[error("CSV 序列化错误：{0}")]
    Serialize(String),
}

/// 把一行 `ResolutionRow` 格式化为 CSV 单元字符串。
///
/// 处理规则：
/// - 含逗号 / 双引号 / 换行的字段用双引号包裹，内部双引号转义为 `""`
/// - `None` 衍生列渲染为空字符串
fn escape_csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_row<W: Write>(w: &mut W, row: &ResolutionRow) -> Result<(), CsvExportError> {
    let status = row
        .status
        .map(|s| serde_json::to_value(s).ok().and_then(|v| v.as_str().map(String::from)))
        .flatten()
        .unwrap_or_default();
    let due_in_days = row
        .due_in_days
        .map(|d| d.to_string())
        .unwrap_or_default();
    let due_bucket = row
        .due_bucket
        .map(|b| serde_json::to_value(b).ok().and_then(|v| v.as_str().map(String::from)))
        .flatten()
        .unwrap_or_default();

    let line = format!(
        "{rid},{title},{dept},{owner},{due},{acc},{status},{days},{bucket}\n",
        rid = escape_csv_field(&row.resolution_id),
        title = escape_csv_field(&row.title),
        dept = escape_csv_field(&row.owner_department),
        owner = escape_csv_field(&row.owner_id),
        due = row.due_at.format("%Y-%m-%d"),
        acc = escape_csv_field(&row.acceptance),
        status = escape_csv_field(&status),
        days = escape_csv_field(&due_in_days),
        bucket = escape_csv_field(&due_bucket),
    );
    w.write_all(line.as_bytes()).map_err(|e| CsvExportError::Io {
        path: std::path::PathBuf::from("<stream>"),
        source: e,
    })?;
    Ok(())
}

/// 把 `resolutions.csv` 写入指定路径。
///
/// 顶部 4 行注释头（`DEPLOYED_AT` / `WEEK_ID` / `TOOL_VERSION` / `AI_DEGRADED`）+ 表头 + 数据行。
pub fn write_resolutions_csv(
    path: &Path,
    rows: &[ResolutionRow],
    meta: &ExportMetadata,
) -> Result<(), CsvExportError> {
    let mut f = fs::File::create(path).map_err(|e| CsvExportError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    f.write_all(meta.to_csv_header().as_bytes())
        .map_err(|e| CsvExportError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    f.write_all(RESOLUTIONS_HEADER.as_bytes())
        .map_err(|e| CsvExportError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    f.write_all(b"\n").map_err(|e| CsvExportError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    for row in rows {
        write_row(&mut f, row)?;
    }
    Ok(())
}

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_row() -> ResolutionRow {
        let r = Resolution {
            resolution_id: "#[W41]01".to_string(),
            title: "试点开展 DRG 入组校验".to_string(),
            owner_department: "医务科".to_string(),
            owner_id: "E10086".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            acceptance: "骨科 3 个病组入组一致率 ≥ 95%".to_string(),
            evidence_ref: None,
        };
        ResolutionRow::from_resolution(&r)
    }

    #[test]
    fn header_is_nine_columns_in_fixed_order() {
        assert_eq!(
            RESOLUTIONS_HEADER,
            "resolution_id,title,owner_department,owner_id,due_at,acceptance,status,due_in_days,due_bucket"
        );
        // 9 列固定
        assert_eq!(RESOLUTIONS_HEADER.split(',').count(), 9);
    }

    #[test]
    fn from_resolution_copies_six_required_fields() {
        let row = sample_row();
        assert_eq!(row.resolution_id, "#[W41]01");
        assert_eq!(row.title, "试点开展 DRG 入组校验");
        assert_eq!(row.owner_department, "医务科");
        assert_eq!(row.owner_id, "E10086");
        assert_eq!(row.due_at, NaiveDate::from_ymd_opt(2026, 10, 31).unwrap());
        assert!(row.acceptance.contains("≥ 95%"));
        // 衍生列均为 None
        assert!(row.status.is_none());
        assert!(row.due_in_days.is_none());
        assert!(row.due_bucket.is_none());
    }

    #[test]
    fn builder_chains_three_derived_fields() {
        let row = ResolutionRowBuilder::from_resolution(&Resolution {
            resolution_id: "#[W41]02".to_string(),
            title: "门诊电子病历升级".to_string(),
            owner_department: "信息科".to_string(),
            owner_id: "E20001".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            acceptance: "门诊 5 个科室全部切换到 EMR 5.0".to_string(),
            evidence_ref: None,
        })
        .derived(ResolutionStatus::InProgress, 21, DueBucket::Lt30)
        .build();
        assert_eq!(row.status, Some(ResolutionStatus::InProgress));
        assert_eq!(row.due_in_days, Some(21));
        assert_eq!(row.due_bucket, Some(DueBucket::Lt30));
    }

    #[test]
    fn builder_step_by_step_chain() {
        let r = Resolution {
            resolution_id: "#[W41]03".to_string(),
            title: "采购呼吸机".to_string(),
            owner_department: "设备科".to_string(),
            owner_id: "E30007".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
            acceptance: "5 台呼吸机到货验收".to_string(),
            evidence_ref: None,
        };
        let row = ResolutionRowBuilder::from_resolution(&r)
            .status(ResolutionStatus::Pending)
            .due_in_days(50)
            .due_bucket(DueBucket::Gt30)
            .build();
        assert_eq!(row.status, Some(ResolutionStatus::Pending));
        assert_eq!(row.due_in_days, Some(50));
        assert_eq!(row.due_bucket, Some(DueBucket::Gt30));
    }

    #[test]
    fn escape_csv_field_handles_commas_quotes_newlines() {
        assert_eq!(escape_csv_field("plain"), "plain");
        assert_eq!(escape_csv_field("a,b"), "\"a,b\"");
        assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv_field("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(escape_csv_field("a\r\nb"), "\"a\r\nb\"");
    }

    #[test]
    fn write_resolutions_csv_emits_metadata_header_table_header_and_data() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("resolutions.csv");
        let meta = ExportMetadata {
            deployed_at: "2026-10-12T03:00:00Z".to_string(),
            week_id: "2026-W41".to_string(),
            tool_version: "0.1.0-test".to_string(),
            ai_degraded: false,
        };
        let row = ResolutionRowBuilder::from_resolution(&Resolution {
            resolution_id: "#[W41]01".to_string(),
            title: "试点开展 DRG 入组校验".to_string(),
            owner_department: "医务科".to_string(),
            owner_id: "E10086".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            acceptance: "骨科 3 个病组入组一致率 ≥ 95%".to_string(),
            evidence_ref: None,
        })
        .derived(ResolutionStatus::InProgress, 19, DueBucket::Lt30)
        .build();

        write_resolutions_csv(&path, &[row], &meta).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // 4 行元字段 + 1 行表头 + 1 行数据 = 6 行
        assert_eq!(content.lines().count(), 6);
        // 元字段头
        assert!(content.starts_with("# DEPLOYED_AT: 2026-10-12T03:00:00Z"));
        assert!(content.contains("# WEEK_ID: 2026-W41"));
        assert!(content.contains("# TOOL_VERSION: 0.1.0-test"));
        assert!(content.contains("# AI_DEGRADED: false"));
        // 表头
        assert!(content.contains(RESOLUTIONS_HEADER));
        // 数据行
        assert!(content.contains("#[W41]01"));
        assert!(content.contains("医务科"));
        assert!(content.contains("E10086"));
        assert!(content.contains("2026-10-31"));
        assert!(content.contains("in_progress"));
        assert!(content.contains("19"));
        assert!(content.contains("lt30"));
    }

    #[test]
    fn write_resolutions_csv_empty_rows_still_writes_header() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty.csv");
        let meta = ExportMetadata::now("2026-W41", false);
        write_resolutions_csv(&path, &[], &meta).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        // 4 行元 + 1 行表头 = 5 行
        assert_eq!(content.lines().count(), 5);
        assert!(content.contains(RESOLUTIONS_HEADER));
    }

    #[test]
    fn write_resolutions_csv_quotes_field_containing_comma() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("comma.csv");
        let meta = ExportMetadata::now("2026-W41", true);
        let row = ResolutionRow {
            resolution_id: "#[W41]01".to_string(),
            title: "标题, 含逗号".to_string(), // 故意带逗号
            owner_department: "医务科".to_string(),
            owner_id: "E10086".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            acceptance: "标准".to_string(),
            status: Some(ResolutionStatus::Pending),
            due_in_days: Some(0),
            due_bucket: Some(DueBucket::Lt3),
        };
        write_resolutions_csv(&path, &[row], &meta).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        // 含逗号字段被双引号包裹
        assert!(content.contains("\"标题, 含逗号\""));
    }

    #[test]
    fn write_resolutions_csv_missing_derived_columns_render_as_empty() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("partial.csv");
        let meta = ExportMetadata::now("2026-W41", false);
        let row = sample_row(); // 无衍生列
        write_resolutions_csv(&path, &[row], &meta).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        // 数据行末尾应该是 ",,," 三个连续逗号（status, due_in_days, due_bucket 都为空）
        let data_line = content
            .lines()
            .last()
            .expect("应有数据行");
        assert!(data_line.ends_with(",,,"), "data_line = {data_line}");
    }

    #[test]
    fn csv_export_error_io_carries_path_and_source() {
        // 试图写到不存在的目录
        let result = write_resolutions_csv(
            std::path::Path::new("/this/does/not/exist/foo.csv"),
            &[],
            &ExportMetadata::now("2026-W41", false),
        );
        assert!(matches!(result, Err(CsvExportError::Io { .. })));
    }

    #[test]
    fn resolution_row_display_includes_all_nine_fields() {
        let row = ResolutionRowBuilder::from_resolution(&Resolution {
            resolution_id: "R1".to_string(),
            title: "T".to_string(),
            owner_department: "D".to_string(),
            owner_id: "O".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            acceptance: "A".to_string(),
            evidence_ref: None,
        })
        .derived(ResolutionStatus::Completed, -3, DueBucket::Overdue)
        .build();
        let s = format!("{row}");
        assert!(s.contains("R1"));
        assert!(s.contains("status=Some(Completed)"));
        assert!(s.contains("days=Some(-3)"));
        assert!(s.contains("bucket=Some(Overdue)"));
    }

    #[test]
    fn resolution_row_serializes_roundtrip() {
        let row = ResolutionRowBuilder::from_resolution(&Resolution {
            resolution_id: "R1".to_string(),
            title: "T".to_string(),
            owner_department: "D".to_string(),
            owner_id: "O".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            acceptance: "A".to_string(),
            evidence_ref: None,
        })
        .derived(ResolutionStatus::Completed, -3, DueBucket::Overdue)
        .build();
        let j = serde_json::to_string(&row).unwrap();
        let back: ResolutionRow = serde_json::from_str(&j).unwrap();
        assert_eq!(row, back);
    }
}
