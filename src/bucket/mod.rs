//! 决议状态机推进 + 6 桶剩余天数派生层。
//!
//! 主路径（按 `spec.md` 工作闭环）：
//! 1. `extract` 把会议纪要 md 段落结构化抽取出 `Resolution`（5 必填字段 + 1 选填）
//! 2. `oa_pull` 拉取该决议关联的 OA 流转事件
//! 3. `advance()` 把每个 OA event 应用到决议上，按 `state_machine` 5 态强制校验跃迁
//! 4. `bucket()` 按 `due_in_days` 派生到 6 个 `DueBucket` 之一
//! 5. `derive_all()` 串成「决议条目 + 状态机 + 桶 + 审计」全量
//!
//! 关键不变量（每条都在单测中守护）：
//! - 非法跃迁**绝不**静默改写状态；返回 `InvalidTransition { from, to, reason }`。
//! - 6 桶覆盖所有可能的 `due_in_days` 取值（含 overdue 桶的负值判定）。
//! - 终态 `Completed` 之后再收到任何 OA event 一律拒绝。
//!
//! 源产品主路径同构：源 `crates/model/src/deployment_audit_log/mod.rs` 的
//! `DeploymentAuditLogEvent` + `AUDIT_LOG_INDEX_BY_ACTION` —— 本模块把每条决议推进
//! 视为一个 audit event，状态枚举 = 事件的 action 字段；非法 action 不被静默吞掉，
//! 由 `InvalidTransition` 显式拒绝（与源对 `action` 字段的严格校验同构）。

pub mod error;
pub mod state_machine;

// 后续 task 落盘的子模块（占位以保持 mod 树稳定；空函数体保证 cargo build 通过）。
pub mod advance;
pub mod due_bucket;

pub use error::BucketError;
pub use state_machine::{InvalidTransition, ResolutionStatus};

#[cfg(test)]
mod tests;
