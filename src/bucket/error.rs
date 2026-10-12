//! 决议状态机 / 桶派生层错误类型。
//!
//! 3 类错误：
//! - `InvalidTransition` — 状态机拒绝非法的状态跃迁（详见 `state_machine.rs`）
//! - `InvalidDate`      — `due_at` / `current_date` 解析失败或超出合法范围
//! - `Unreachable`      — 不应到达的代码路径（防御性保留，便于 catch-all 排查）
//!
//! 与源 `crates/common/src/runtime/mod.rs` 的「确定性失败可见」思路同构：
//! 任一错误分类都不允许「静默重试 / 静默降级」，由调用方显式 `match` 处理。
//!
//! 退出码映射（见 `cli/error.rs`，task 11 落盘）：
//! - `InvalidTransition` → 退出码 4（schema / 状态校验失败）
//! - `InvalidDate`       → 退出码 4
//! - `Unreachable`       → 退出码 5（桶派生失败）

use thiserror::Error;

use super::state_machine::InvalidTransition;

/// 决议状态机 / 桶派生层错误枚举。
#[derive(Debug, Error)]
pub enum BucketError {
    /// 状态机非法跃迁（决议条目 + OA event 推演时遇到不允许的转移）。
    #[error("决议状态机非法跃迁：{0}")]
    InvalidTransition(#[from] InvalidTransition),

    /// OA event 缺失关键字段（`oa_event_id` / `resolution_id` / `actor_id` 任一为空）。
    /// 状态机绝不允许「缺事件」却仍推进决议 —— 由 `bucket::advance` 显式拒绝。
    #[error("OA event 关键字段缺失：{0}")]
    MissingEvent(String),

    /// 日期解析失败或越界（`due_at` / `current_date` 不合法）。
    #[error("决议日期非法：{0}")]
    InvalidDate(String),

    /// 不应到达的代码路径（防御性保留，便于 catch-all 排查）。
    #[error("决议桶派生内部错误：{0}")]
    Unreachable(String),
}

impl BucketError {
    /// 是否可重试：仅 `Unreachable` 在理论上是确定性的代码 bug，**不可**重试；
    /// `InvalidTransition` / `InvalidDate` 均为业务数据驱动，也不可重试。
    ///
    /// 因此本类型**永远**返回 `false`，由上游 `cli/error.rs` 据此统一映射退出码 4/5。
    pub fn is_retryable(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bucket::ResolutionStatus;

    #[test]
    fn invalid_transition_display_contains_arrow() {
        let err = InvalidTransition::new(
            ResolutionStatus::Completed,
            ResolutionStatus::InProgress,
            "终态后不再回退",
        );
        let s = err.to_string();
        assert!(s.contains("completed"), "Display 应含 from 态: {s}");
        assert!(s.contains("in_progress"), "Display 应含 to 态: {s}");
        assert!(s.contains("→"), "Display 应含箭头: {s}");
    }

    #[test]
    fn bucket_error_wraps_invalid_transition() {
        let inner = InvalidTransition::new(
            ResolutionStatus::Pending,
            ResolutionStatus::Completed,
            "缺 started 事件",
        );
        let wrapped: BucketError = inner.clone().into();
        match &wrapped {
            BucketError::InvalidTransition(t) => assert_eq!(t, &inner),
            other => panic!("expected InvalidTransition, got {other:?}"),
        }
    }

    #[test]
    fn bucket_error_never_retryable() {
        assert!(!BucketError::InvalidTransition(InvalidTransition::new(
            ResolutionStatus::Pending,
            ResolutionStatus::Overdue,
            "缺 in_progress 事件",
        ))
        .is_retryable());
        assert!(!BucketError::InvalidDate("2026-13-40".into()).is_retryable());
        assert!(!BucketError::Unreachable("should-not-happen".into()).is_retryable());
        // 新增：MissingEvent 也属于业务数据驱动，不可重试。
        assert!(!BucketError::MissingEvent("oa_event_id 为空".into()).is_retryable());
    }

    #[test]
    fn missing_event_display_contains_field_name() {
        let err = BucketError::MissingEvent("oa_event_id 为空".into());
        let s = err.to_string();
        assert!(s.contains("OA event 关键字段缺失"), "Display 应含错误描述: {s}");
        assert!(s.contains("oa_event_id 为空"), "Display 应含字段名: {s}");
    }

    #[test]
    fn missing_event_is_distinct_variant_from_invalid_transition() {
        // 关键不变量：MissingEvent 与 InvalidTransition 必须各自有独立类型，
        // 调用方据此区分「缺事件」与「非法跃迁」。
        let missing = BucketError::MissingEvent("resolution_id 为空".into());
        let invalid = BucketError::InvalidTransition(InvalidTransition::new(
            ResolutionStatus::Pending,
            ResolutionStatus::Completed,
            "缺 started 事件",
        ));
        assert!(matches!(missing, BucketError::MissingEvent(_)));
        assert!(matches!(invalid, BucketError::InvalidTransition(_)));
        assert_ne!(format!("{:?}", missing).split('(').next(), format!("{:?}", invalid).split('(').next());
    }
}
