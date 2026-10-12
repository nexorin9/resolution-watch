//! 导出包层（CSV + 4 种 JSONL + summary + sha256 + atomic_rename）。
//!
//! ## 模块职责
//!
//! - [`csv_export`]      — `resolutions.csv` 列定义 + 6 必填 + 3 衍生列写入
//! - [`atomic_rename`]   — 临时目录写入 + 失败清理 + 元字段注入辅助
//! - [`jsonl_export`]    — `overdue` / `due_soon` / `conflicts` / `audit_log` 四种 JSONL writer
//! - [`summary`]         — `summary.json` 4+1 桶统计
//! - [`sha256`]          — `sha256_manifest.json` 校验和
//!
//! ## 不写半成品
//!
//! 所有导出操作走 [`atomic_rename::write_atomic`] 通道：先把全部产物写到
//! `out/<week_id>/.tmp/<random>/` 临时目录里，全链路成功后 `rename` 到正式目录；
//! 任一阶段失败立即 `remove_dir_all` 临时目录，避免半成品被读到。
//!
//! ## 源产品主路径同构
//!
//! 源 `crates/runtime/src/prod.rs` 的强一致 deterministic 任务提交（失败立即可见、可中断）
//! — 本模块以 atomic_rename 让任何中间失败都「不写半成品」，把源「失败立即可见」的本意
//! 翻译为「目录要么不存在、要么完整」。

pub mod atomic_rename;
pub mod csv_export;
pub mod jsonl_export;
pub mod sha256;
pub mod summary;

#[cfg(test)]
mod tests;

pub use atomic_rename::{write_atomic, ExportMetadata, MetadataError, WriteAtomicError};
pub use csv_export::{ResolutionRow, ResolutionRowBuilder, CsvExportError, RESOLUTIONS_HEADER};
pub use jsonl_export::{
    write_audit_log_jsonl, write_conflicts_jsonl, write_due_soon_jsonl, write_overdue_jsonl,
    AuditSegmentEntry, ConflictEntry, JsonlExportError, ResolutionJsonlEntry,
};
pub use sha256::{
    build_manifest, sha256_hex_file, write_manifest, Sha256Entry, Sha256Error, Sha256Manifest,
    Sha256Meta,
};
pub use summary::{
    build_summary, write_summary, Summary, SummaryError, SummaryInputRow, SummaryMeta,
};