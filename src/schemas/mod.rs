//! 决议条目 / OA 流转事件 / 桶分布 / 审计段 schema-first 类型化层。
//!
//! 该模块是整个 CLI 的"数据契约中心"：会议纪要抽取、OA 拉取、桶派生、状态机推进、
//! 导出包、审计链都依赖这里定义的强类型。修改这里的字段时需同步更新
//! `docs/integration.md` 的字段映射、`docs/examples.md` 的样例片段、
//! `tests/fixtures/` 的 fixture 内容。

pub mod resolution;

pub use resolution::{
    AuditSegment, DueBucket, OaAction, OaEvent, Resolution, ResolutionStatus,
};
