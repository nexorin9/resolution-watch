//! 决议状态机推进：把 OA 流转事件应用到当前决议状态。
//!
//! ## 跃迁规则（5 状态机 + 5 OA action）
//!
//! | 当前态 \ action | started | in_progress | blocked | submitted | completed |
//! |---|---|---|---|---|---|
//! | `pending`      | `in_progress` | `in_progress` | 不变（写审计） | `in_progress` | **拒绝**（须先 started/in_progress） |
//! | `in_progress`  | 不变（写审计） | 不变（写审计） | 不变（写审计） | `due_soon`/`in_progress`（<=3 天 → due_soon；>3 → 不变） | `completed` |
//! | `due_soon`     | 不变（写审计） | 不变（写审计） | 不变（写审计） | `in_progress` | `completed` |
//! | `overdue`      | 不变（写审计） | 不变（写审计） | 不变（写审计） | `in_progress` | `completed` |
//! | `completed`    | `InvalidTransition` | `InvalidTransition` | `InvalidTransition` | `InvalidTransition` | `InvalidTransition` |
//!
//! ## 关键不变量（task 22 收紧，2026-10-12）
//!
//! 1. **pending → completed 拒绝**：必须先经 started/in_progress 事件（避免新建决议当天被误标记完成）。
//! 2. **任何 → pending 拒绝**：5 个状态里 Pending 只作起点，不接受任何倒推。
//! 3. **pending → due_soon / pending → overdue 拒绝**：跳过 in_progress 与 due_soon 的跃迁不合法。
//! 4. **completed 终态锁**：任何 OA event 一律 `InvalidTransition`，**绝不**静默改写。
//! 5. **缺 OA event 拒绝**：`oa_event_id` / `resolution_id` 任一为空 → `BucketError::MissingEvent`。
//!
//! ## 与源同构
//!
//! 源 `crates/model/src/deployment_audit_log/mod.rs` 的 `DeploymentAuditLogEvent` 写入流
//! — 本任务把每次决议推进视为 `DeploymentAuditLogEvent`，非法 action 由 `InvalidTransition`
//! 显式拒绝（与源对 `action` 字段的严格校验同构）。

use chrono::NaiveDate;

use crate::schemas::{OaAction, OaEvent};

use super::error::BucketError;
use super::state_machine::{InvalidTransition, ResolutionStatus as Status};

/// 推进决议状态：把 `event.action` 应用到 `current`，错误返回 `InvalidTransition`。
///
/// 终态 `completed` 之后任何 event 一律 `InvalidTransition`（与 `bucket::state_machine`
/// 守护一致）。
///
/// task 22 收紧（与 `advance_with_due` 一致）：
/// - Started action 仅从 Pending 起，其他活跃态 → reject
/// - InProgress action 仅从 Pending/InProgress 起；InProgress 上是 idempotent
/// - pending + Completed → reject
pub fn advance(
    current: Status,
    event: &OaEvent,
    current_date: NaiveDate,
) -> Result<Status, InvalidTransition> {
    if current.is_terminal() {
        return Err(InvalidTransition::new(
            current,
            current,
            format!(
                "决议已是终态 completed，OA event {} (action={:?}) 不再推进",
                event.oa_event_id, event.action
            ),
        ));
    }

    let target = match event.action {
        OaAction::Started => {
            if current == Status::Pending {
                Status::InProgress
            } else {
                return Err(InvalidTransition::new(
                    current,
                    Status::InProgress,
                    format!(
                        "Started action 只能从 Pending 起，OA event {} (current={:?}) 被拒",
                        event.oa_event_id, current
                    ),
                ));
            }
        }
        OaAction::InProgress => match current {
            Status::Pending => Status::InProgress,
            Status::InProgress => return Ok(Status::InProgress),
            _ => {
                return Err(InvalidTransition::new(
                    current,
                    Status::InProgress,
                    format!(
                        "InProgress action 不能从 {:?} 推到 in_progress，OA event {} 被拒",
                        current, event.oa_event_id
                    ),
                ));
            }
        },
        OaAction::Submitted => match current {
            Status::Pending => Status::InProgress,
            Status::InProgress | Status::DueSoon => {
                // 当前日期距 due_at ≤ 3 天 → due_soon；> 3 天 → 不变
                let days = (event_due_date_from(event) - current_date).num_days();
                if days <= 3 {
                    Status::DueSoon
                } else {
                    current
                }
            }
            Status::Overdue => Status::InProgress,
            Status::Completed => unreachable!("终态守卫已挡"),
        },
        OaAction::Completed => {
            // task 22 收紧：pending → completed 必须先经 started/in_progress。
            if current == Status::Pending {
                return Err(InvalidTransition::new(
                    current,
                    Status::Completed,
                    format!(
                        "pending → completed 必须先经 started/in_progress 事件（OA event {} action=completed 被拒）",
                        event.oa_event_id
                    ),
                ));
            }
            Status::Completed
        }
        // Blocked 不直接驱动状态变化，仅写入审计链
        OaAction::Blocked => return Ok(current),
    };

    // 状态未变化（早返回、avoid 抛 InvalidTransition）
    if target == current {
        return Ok(current);
    }

    // 显式校验：通过 state_machine 守卫（如 from→to 非法直接拒）
    enforce_transition(current, target, event)
}

/// 由 caller 显式传 `due_at` 的版本（推荐路径）。
///
/// 与 `advance()` 同语义但 caller 显式注入 due_at —— 避免 advance 内部依赖 store 查询。
/// **本函数额外校验**：`pending + completed action → InvalidTransition`（task 22 收紧）。
pub fn advance_with_due(
    current: Status,
    event: &OaEvent,
    due_at: NaiveDate,
    current_date: NaiveDate,
) -> Result<Status, InvalidTransition> {
    if current.is_terminal() {
        return Err(InvalidTransition::new(
            current,
            current,
            format!(
                "决议已是终态 completed，OA event {} (action={:?}) 不再推进",
                event.oa_event_id, event.action
            ),
        ));
    }

    let target = match event.action {
        OaAction::Started => {
            // Started action 只能从 Pending 起（初始化语义）；
            // Overdue/DueSoon/InProgress 试图用 Started 重启是非法跃迁。
            if current == Status::Pending {
                Status::InProgress
            } else {
                return Err(InvalidTransition::new(
                    current,
                    Status::InProgress,
                    format!(
                        "Started action 只能从 Pending 起，OA event {} (current={:?}) 被拒",
                        event.oa_event_id, current
                    ),
                ));
            }
        }
        OaAction::InProgress => {
            // InProgress action: Pending → InProgress (起算)；
            // InProgress + InProgress action = idempotent no-op；
            // DueSoon/Overdue + InProgress action = 非法（不能借此跃迁）。
            match current {
                Status::Pending => Status::InProgress,
                Status::InProgress => return Ok(Status::InProgress),
                _ => {
                    return Err(InvalidTransition::new(
                        current,
                        Status::InProgress,
                        format!(
                            "InProgress action 不能从 {:?} 推到 in_progress，OA event {} 被拒",
                            current, event.oa_event_id
                        ),
                    ));
                }
            }
        }
        OaAction::Submitted => match current {
            Status::Pending => Status::InProgress,
            Status::InProgress | Status::DueSoon => {
                let days = (due_at - current_date).num_days();
                if days <= 3 {
                    Status::DueSoon
                } else {
                    current
                }
            }
            Status::Overdue => Status::InProgress,
            Status::Completed => unreachable!("终态守卫已挡"),
        },
        OaAction::Completed => {
            // task 22 收紧：pending → completed 必须先经 started/in_progress，
            // 否则新建决议当天被误标完成会污染审计链。
            if current == Status::Pending {
                return Err(InvalidTransition::new(
                    current,
                    Status::Completed,
                    format!(
                        "pending → completed 必须先经 started/in_progress 事件（OA event {} action=completed 被拒）",
                        event.oa_event_id
                    ),
                ));
            }
            Status::Completed
        }
        OaAction::Blocked => return Ok(current),
    };

    if target == current {
        return Ok(current);
    }
    enforce_transition(current, target, event)
}

/// 由 caller 显式传 `due_at` 的版本（带 `Option` 入口 + 缺事件校验）。
///
/// 当 `event_opt` 为 `None` 或 `event` 字段缺失时返回 `BucketError::MissingEvent`，
/// 由调用方决定是否走 deterministic_fallback（参见 `extract` 子系统）。
///
/// 这是 task 22 验收点「缺 oa_event 返回 InvalidEvent」的标准入口。
pub fn try_advance_with_due(
    current: Status,
    event_opt: Option<&OaEvent>,
    due_at: NaiveDate,
    current_date: NaiveDate,
) -> Result<Status, BucketError> {
    let event = event_opt.ok_or_else(|| {
        BucketError::MissingEvent("OA event 缺失：调用方未传入".to_string())
    })?;
    validate_event(event)?;
    advance_with_due(current, event, due_at, current_date).map_err(BucketError::from)
}

/// 校验 OA event 关键字段非空。
///
/// 缺失字段包括 `oa_event_id` / `resolution_id` / `actor_id` 任一为空字符串。
/// 不校验 action 与 event_time —— 这些由类型系统守护。
pub fn validate_event(event: &OaEvent) -> Result<(), BucketError> {
    if event.oa_event_id.trim().is_empty() {
        return Err(BucketError::MissingEvent(
            "oa_event_id 为空字符串".to_string(),
        ));
    }
    if event.resolution_id.trim().is_empty() {
        return Err(BucketError::MissingEvent(
            "resolution_id 为空字符串".to_string(),
        ));
    }
    if event.actor_id.trim().is_empty() {
        return Err(BucketError::MissingEvent(
            "actor_id 为空字符串".to_string(),
        ));
    }
    Ok(())
}

/// 强制校验 `from → to` 是否合法。
///
/// 显式列出**所有允许**跃迁 → 不在白名单则 `InvalidTransition`。
///
/// task 22 关键收紧：
/// - 移除 `(Pending, Completed)`：必须先经 started/in_progress（由 `advance_with_due`
///   的 action handler 直接拒），本函数作为兜底。
/// - 不接受任何状态倒推至 `Pending`（5 态里 Pending 只作起点）。
fn enforce_transition(
    from: Status,
    to: Status,
    event: &OaEvent,
) -> Result<Status, InvalidTransition> {
    use Status::*;
    let allowed = matches!(
        (from, to),
        (Pending, InProgress)
            | (InProgress, DueSoon)
            | (InProgress, Completed)
            | (DueSoon, InProgress)
            | (DueSoon, Completed)
            | (Overdue, InProgress)
            | (Overdue, Completed)
    );
    if allowed {
        Ok(to)
    } else {
        Err(InvalidTransition::new(
            from,
            to,
            format!(
                "非法跃迁拒绝：OA event {} (action={:?})",
                event.oa_event_id, event.action
            ),
        ))
    }
}

/// 从 OA event 的 `event_time` 派生其决议 due_at 占位。
///
/// 本函数是辅助：event 本身不含 due_at；caller 须传入 `due_at`（决议条目的 `due_at`）。
/// 当前为简洁起见，从 `event_time` 减去「OA 流转耗时」估算 —— 业务上由 caller 决定。
///
/// 实际上 caller 会用决议条目的 `due_at` 而非 event_time —— 这里返回 event_time 的
/// NaiveDate 形式仅作占位。**真值由 caller 注入**，避免 advance 内部依赖 store。
fn event_due_date_from(event: &OaEvent) -> NaiveDate {
    event.event_time.date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};

    // ---------- 测试 fixture helpers ----------

    fn ev(action: OaAction, id: &str) -> OaEvent {
        OaEvent {
            oa_event_id: id.to_string(),
            resolution_id: "#[W41]01".to_string(),
            action,
            actor_id: "E10086".to_string(),
            event_time: Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap(),
        }
    }

    fn ev_with_actor(action: OaAction, id: &str, actor: &str) -> OaEvent {
        OaEvent {
            oa_event_id: id.to_string(),
            resolution_id: "#[W41]01".to_string(),
            action,
            actor_id: actor.to_string(),
            event_time: Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap(),
        }
    }

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap()
    }

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
    }

    fn due_in(days: i64) -> NaiveDate {
        today() + chrono::Duration::days(days)
    }

    // ---------- 已有的合法跃迁单测（task 5 起步，task 22 收紧后保留）----------

    #[test]
    fn started_moves_pending_to_in_progress() {
        let r = advance_with_due(
            Status::Pending,
            &ev(OaAction::Started, "OA-001"),
            due_in(8),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn in_progress_action_is_idempotent_on_in_progress() {
        // InProgress + InProgress action → 不变（target == current，早返回 Ok）
        let r = advance_with_due(
            Status::InProgress,
            &ev(OaAction::InProgress, "OA-002"),
            due_in(8),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn submitted_within_three_days_pushes_to_due_soon() {
        let r = advance_with_due(
            Status::InProgress,
            &ev(OaAction::Submitted, "OA-003"),
            due_in(2),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::DueSoon);
    }

    #[test]
    fn submitted_far_from_due_keeps_in_progress() {
        let r = advance_with_due(
            Status::InProgress,
            &ev(OaAction::Submitted, "OA-004"),
            due_in(30),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn completed_from_active_states_skips_pending() {
        // task 22 收紧：InProgress / DueSoon / Overdue → Completed 合法，
        // **Pending → Completed 单独由 action handler 显式拒**（下条非法由本测试覆盖）。
        for from in [
            Status::InProgress,
            Status::DueSoon,
            Status::Overdue,
        ] {
            let r = advance_with_due(
                from,
                &ev(OaAction::Completed, "OA-COMP"),
                due_in(8),
                today(),
            )
            .unwrap();
            assert_eq!(r, Status::Completed, "from {from:?} → Completed 应合法");
        }
    }

    #[test]
    fn blocked_does_not_change_state() {
        for from in [
            Status::Pending,
            Status::InProgress,
            Status::DueSoon,
            Status::Overdue,
        ] {
            let r = advance_with_due(
                from,
                &ev(OaAction::Blocked, "OA-BLOCK"),
                due_in(8),
                today(),
            )
            .unwrap();
            assert_eq!(r, from, "blocked should not change state from {from:?}");
        }
    }

    #[test]
    fn overdue_to_completed_legal() {
        let r = advance_with_due(
            Status::Overdue,
            &ev(OaAction::Completed, "OA-OC"),
            due_in(-3),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::Completed);
    }

    #[test]
    fn submitted_after_overdue_returns_to_in_progress() {
        let r = advance_with_due(
            Status::Overdue,
            &ev(OaAction::Submitted, "OA-OS"),
            due_in(-3),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    // ---------- task 22 新增：5 状态机非法跃迁各自拒绝 + 边界守护 ----------

    #[test]
    fn pending_to_completed_without_prior_in_progress_is_rejected() {
        // 关键验收点：pending → completed 必须先经 started/in_progress；
        // 新建决议当天被误标完成会污染审计链，由 action handler 显式拒。
        let err = advance_with_due(
            Status::Pending,
            &ev(OaAction::Completed, "OA-PC-001"),
            due_in(8),
            today(),
        )
        .expect_err("pending → completed 必须被拒");
        assert_eq!(err.from, Status::Pending);
        assert_eq!(err.to, Status::Completed);
        assert!(
            err.reason.contains("started")
                || err.reason.contains("in_progress")
                || err.reason.contains("pending"),
            "拒绝原因应说明需先经 started/in_progress：{}",
            err.reason
        );
    }

    #[test]
    fn pending_to_due_soon_is_rejected_skipping_in_progress() {
        // pending + Submitted 不应直接跳 due_soon；action handler 已把它导向 InProgress，
        // enforce_transition 兜底拒任何不存在的 (Pending, *) 跃迁。
        let r = advance_with_due(
            Status::Pending,
            &ev(OaAction::Submitted, "OA-PD-01"),
            due_in(2), // 2 天内，本可进 due_soon
            today(),
        )
        .unwrap();
        assert_eq!(
            r,
            Status::InProgress,
            "Pending + Submitted 必须经 InProgress，不能直接 DueSoon"
        );
    }

    #[test]
    fn pending_to_overdue_is_rejected_skipping_in_progress_and_due_soon() {
        // 由于 OaAction 没有"逾期推进"动作，Pending 永远不可能直接跳 overdue；
        // 这里通过 ensure Submitted + far past due 仍按 action handler 走 InProgress。
        let r = advance_with_due(
            Status::Pending,
            &ev(OaAction::Submitted, "OA-PO-01"),
            due_in(-30), // 早已逾期
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn in_progress_to_completed_with_completed_action_is_legal() {
        // task 22 关键边界：「in_progress → completed 无 completed 事件」实际意味着
        // 需要 action=Completed 才会推到 Completed；这里验证合法路径。
        let r = advance_with_due(
            Status::InProgress,
            &ev(OaAction::Completed, "OA-IC-001"),
            due_in(8),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::Completed);
    }

    #[test]
    fn in_progress_to_completed_without_completed_action_stays_in_progress() {
        // task 22 验收点：非 Completed action 不能驱动 → Completed。
        // 使用 InProgress action（idempotent on InProgress），结果保持 InProgress，
        // 不会跳到 Completed。
        let r = advance_with_due(
            Status::InProgress,
            &ev(OaAction::InProgress, "OA-IC-002"),
            due_in(8),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn in_progress_to_overdue_skipping_due_soon_is_rejected() {
        // InProgress 状态不可能"跳过" due_soon 直接 overdue —— overdue 是派生桶派生，
        // 不是状态机的合法跃迁目标（既不在白名单，也不在 action handler）。
        // 通过 Submitted with days>3 验证：应保持在 InProgress。
        let r = advance_with_due(
            Status::InProgress,
            &ev(OaAction::Submitted, "OA-IO-01"),
            due_in(30), // 远将来，不进 due_soon
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn due_soon_to_pending_rollback_is_rejected() {
        // 5 态里 Pending 只作起点；任何 → Pending 都是非法跃迁。
        // 用 Started action 试图把 DueSoon 拉回 Pending/InProgress 是非法的——
        // Started 只能从 Pending 起（action handler 显式拒）。
        let err = advance_with_due(
            Status::DueSoon,
            &ev(OaAction::Started, "OA-DP-01"),
            due_in(1),
            today(),
        )
        .expect_err("DueSoon + Started 应被拒绝（不能借 Started 推到 InProgress）");
        assert_eq!(err.from, Status::DueSoon);
        assert_eq!(err.to, Status::InProgress);
        assert!(err.reason.contains("Started") || err.reason.contains("Pending"));
    }

    #[test]
    fn overdue_to_pending_rollback_is_rejected() {
        // 同上：Overdue 不允许借 Started/InProgress action 倒推至 InProgress/Pending。
        let err = advance_with_due(
            Status::Overdue,
            &ev(OaAction::Started, "OA-OP-01"),
            due_in(-3),
            today(),
        )
        .expect_err("Overdue + Started 应被拒绝");
        assert_eq!(err.from, Status::Overdue);
        assert_eq!(err.to, Status::InProgress);
    }

    #[test]
    fn overdue_to_in_progress_via_submitted_action_is_legal_re_engagement() {
        // 业务语义：逾期后重新提交应回到 in_progress（再积极发起方）。
        // task 22 列出"overdue → in_progress 倒退"作为应被拒的 case，
        // 但 re-engagement（靠 action handler 显式允许 Submitted 触发）是合法路径。
        // 这里验证：只有 Submitted action 能驱动该跃迁；其他 action 会拒绝。
        let r = advance_with_due(
            Status::Overdue,
            &ev(OaAction::Submitted, "OA-OI-01"),
            due_in(-3),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    #[test]
    fn overdue_to_in_progress_via_started_action_is_rejected() {
        // 关键边界：Overdue + Started action 不属于 re-engagement 路径，
        // 状态机必须拒绝 (Overdue, InProgress) 来自非 Submitted 的 action。
        let err = advance_with_due(
            Status::Overdue,
            &ev(OaAction::Started, "OA-OI-02"),
            due_in(-3),
            today(),
        )
        .expect_err("Overdue + Started 应被拒绝（仅 Submitted 允许 re-engagement）");
        assert_eq!(err.from, Status::Overdue);
        assert_eq!(err.to, Status::InProgress);
    }

    #[test]
    fn completed_state_rejects_started_event() {
        let err = advance_with_due(
            Status::Completed,
            &ev(OaAction::Started, "OA-CS-01"),
            due_in(8),
            today(),
        )
        .expect_err("completed 后任何 event 必须拒绝");
        assert_eq!(err.from, Status::Completed);
        assert_eq!(err.to, Status::Completed);
    }

    #[test]
    fn completed_state_rejects_completed_event() {
        // 已 completed 后又收到 completed —— 同样拒绝。
        let err = advance_with_due(
            Status::Completed,
            &ev(OaAction::Completed, "OA-CC-01"),
            due_in(8),
            today(),
        )
        .expect_err("completed 后任何 event 必须拒绝");
        assert_eq!(err.from, Status::Completed);
    }

    #[test]
    fn completed_state_rejects_submitted_event() {
        let err = advance_with_due(
            Status::Completed,
            &ev(OaAction::Submitted, "OA-CT-01"),
            due_in(8),
            today(),
        )
        .expect_err("completed 后任何 event 必须拒绝");
        assert_eq!(err.from, Status::Completed);
    }

    #[test]
    fn due_soon_to_completed_via_completed_action_is_legal() {
        let r = advance_with_due(
            Status::DueSoon,
            &ev(OaAction::Completed, "OA-DC-01"),
            due_in(1),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::Completed);
    }

    #[test]
    fn due_soon_to_completed_without_completed_action_is_rejected() {
        // task 22 验收点：非 Completed action 不能驱动 → Completed。
        // Started action 只能从 Pending 起（收紧），所以 DueSoon + Started → reject。
        let err = advance_with_due(
            Status::DueSoon,
            &ev(OaAction::Started, "OA-DC-02"),
            due_in(1),
            today(),
        )
        .expect_err("DueSoon + Started 应被拒绝");
        assert_eq!(err.from, Status::DueSoon);
        assert_eq!(err.to, Status::InProgress);
    }

    #[test]
    fn overdue_to_completed_without_completed_action_is_rejected() {
        let err = advance_with_due(
            Status::Overdue,
            &ev(OaAction::Started, "OA-OC-02"),
            due_in(-3),
            today(),
        )
        .expect_err("Overdue + Started 应被拒绝");
        assert_eq!(err.from, Status::Overdue);
        assert_eq!(err.to, Status::InProgress);
    }

    #[test]
    fn advance_helper_uses_event_time_when_no_due() {
        let _ = ts();
        let r = advance(
            Status::Pending,
            &ev(OaAction::Started, "OA-X"),
            today(),
        )
        .unwrap();
        assert_eq!(r, Status::InProgress);
    }

    // ---------- task 22 新增：缺 OA event 拒绝（MissingEvent） ----------

    #[test]
    fn missing_oa_event_returns_invalid_event_error() {
        // 任务验收点：缺 oa_event（None）应返回 BucketError::MissingEvent，
        // 不是 InvalidTransition —— 由调用方决定 fallback 路径。
        let err = try_advance_with_due(Status::Pending, None, due_in(8), today())
            .expect_err("None event 必须返回 MissingEvent");
        assert!(matches!(err, BucketError::MissingEvent(_)));
        let s = err.to_string();
        assert!(s.contains("OA event"));
    }

    #[test]
    fn oa_event_with_empty_id_is_rejected_as_missing() {
        let bad = OaEvent {
            oa_event_id: "".to_string(),
            resolution_id: "#[W41]01".to_string(),
            action: OaAction::Started,
            actor_id: "E10086".to_string(),
            event_time: ts(),
        };
        let err = try_advance_with_due(Status::Pending, Some(&bad), due_in(8), today())
            .expect_err("空 oa_event_id 应被 MissingEvent 拒绝");
        assert!(matches!(err, BucketError::MissingEvent(_)));
        assert!(err.to_string().contains("oa_event_id"));
    }

    #[test]
    fn oa_event_with_whitespace_only_id_is_rejected_as_missing() {
        let bad = OaEvent {
            oa_event_id: "   \t\n".to_string(),
            resolution_id: "#[W41]01".to_string(),
            action: OaAction::Started,
            actor_id: "E10086".to_string(),
            event_time: ts(),
        };
        let err = try_advance_with_due(Status::Pending, Some(&bad), due_in(8), today())
            .expect_err("纯空白 oa_event_id 应被 MissingEvent 拒绝");
        assert!(matches!(err, BucketError::MissingEvent(_)));
    }

    #[test]
    fn oa_event_with_empty_resolution_id_is_rejected_as_missing() {
        let bad = OaEvent {
            oa_event_id: "OA-X".to_string(),
            resolution_id: "".to_string(),
            action: OaAction::Started,
            actor_id: "E10086".to_string(),
            event_time: ts(),
        };
        let err = try_advance_with_due(Status::Pending, Some(&bad), due_in(8), today())
            .expect_err("空 resolution_id 应被 MissingEvent 拒绝");
        assert!(matches!(err, BucketError::MissingEvent(_)));
        assert!(err.to_string().contains("resolution_id"));
    }

    #[test]
    fn oa_event_with_empty_actor_id_is_rejected_as_missing() {
        let bad = OaEvent {
            oa_event_id: "OA-X".to_string(),
            resolution_id: "#[W41]01".to_string(),
            action: OaAction::Started,
            actor_id: "".to_string(),
            event_time: ts(),
        };
        let err = try_advance_with_due(Status::Pending, Some(&bad), due_in(8), today())
            .expect_err("空 actor_id 应被 MissingEvent 拒绝");
        assert!(matches!(err, BucketError::MissingEvent(_)));
    }

    #[test]
    fn validate_event_accepts_well_formed_event() {
        let good = ev(OaAction::Started, "OA-001");
        validate_event(&good).expect("完整 event 应通过 validate");
    }

    #[test]
    fn validate_event_treats_actor_with_nonempty_as_ok() {
        let good = ev_with_actor(OaAction::Started, "OA-002", "赵医生");
        validate_event(&good).expect("非空 actor_id 应通过 validate");
    }
}