//! 决议状态机 5 态 + Display + FromStr + 非法跃迁错误结构。
//!
//! `ResolutionStatus` 由 `schemas::resolution` 集中定义（与序列化层共用一份真源）；
//! 本模块**重新导出**并在之上加 `Display` / `FromStr` / `InvalidTransition`。
//!
//! 与源 `crates/model/src/deployment_audit_log/mod.rs` 的
//! `DeploymentAuditLogEvent` 同构：每次决议推进视为一个 audit event，
//! `ResolutionStatus` 枚举的 5 态对应事件的 5 类 action。
//! 非法 action 由 `InvalidTransition` 显式拒绝（源对 `action` 字段的严格校验）。

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ---------- 类型重导出 ----------

/// 决议状态机 5 态：4 活跃 + 1 终态。
///
/// 完整定义在 `schemas::ResolutionStatus`（与序列化层共用一份真源）；
/// 这里 `pub use` 重新导出，便于 `use bucket::ResolutionStatus` 引用。
pub use crate::schemas::ResolutionStatus;

// `Status` 是较短的别名，供模块内部使用；外部推荐用 `bucket::ResolutionStatus`。
#[allow(dead_code)]
pub(crate) type Status = ResolutionStatus;

impl ResolutionStatus {
    /// 是否终态：终态后任何推进一律拒绝。
    pub fn is_terminal(&self) -> bool {
        matches!(self, ResolutionStatus::Completed)
    }

    /// 是否活跃态：4 个非终态之一。
    pub fn is_active(&self) -> bool {
        !self.is_terminal()
    }
}

impl fmt::Display for ResolutionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 与 serde snake_case 序列化保持一致：`due_soon` / `in_progress`。
        let s = match self {
            ResolutionStatus::Pending => "pending",
            ResolutionStatus::InProgress => "in_progress",
            ResolutionStatus::DueSoon => "due_soon",
            ResolutionStatus::Overdue => "overdue",
            ResolutionStatus::Completed => "completed",
        };
        f.write_str(s)
    }
}

impl FromStr for ResolutionStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // 接受 snake_case（与序列化一致）以及 kebab-case 容忍外部输入。
        match s {
            "pending" => Ok(ResolutionStatus::Pending),
            "in_progress" | "in-progress" | "inProgress" => Ok(ResolutionStatus::InProgress),
            "due_soon" | "due-soon" | "dueSoon" => Ok(ResolutionStatus::DueSoon),
            "overdue" => Ok(ResolutionStatus::Overdue),
            "completed" | "complete" | "done" => Ok(ResolutionStatus::Completed),
            other => Err(format!("未知的 ResolutionStatus：{other}")),
        }
    }
}

// ---------- 非法跃迁错误结构 ----------

/// 非法跃迁错误：状态机拒绝把决议从 `from` 推到 `to`。
///
/// 含 3 字段：`from` 当前态 / `to` 目标态 / `reason` 拒绝原因（人类可读）。
/// **绝不** 静默改写决议状态；由 `bucket::advance()` 强制返回该错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidTransition {
    /// 当前决议状态。
    pub from: ResolutionStatus,
    /// 试图推进到的目标状态。
    pub to: ResolutionStatus,
    /// 拒绝原因（人类可读，例如「缺 started 事件」「终态不再回退」）。
    pub reason: String,
}

impl InvalidTransition {
    /// 构造一个非法跃迁错误。
    pub fn new(
        from: ResolutionStatus,
        to: ResolutionStatus,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            from,
            to,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "非法跃迁：{} → {}（{}）",
            self.from, self.to, self.reason
        )
    }
}

impl std::error::Error for InvalidTransition {}
