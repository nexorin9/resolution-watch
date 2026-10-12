//! bucket 子系统单元测试（task 5 收尾：3+ 单测；task 22/23/24/25 续扩到 12+）。
//!
//! 覆盖：
//! - 5 态 `ResolutionStatus` 的 `Display` 与 `FromStr`（snake_case 与 kebab-case 兼容）
//! - `InvalidTransition` 错误结构字段齐全
//! - `ResolutionStatus::is_terminal` / `is_active` 边界
//! - 6 桶 `DueBucket` 派生 + overdue 桶负值判定（task 23）

use crate::bucket::advance::{advance, advance_with_due, try_advance_with_due};
use crate::bucket::due_bucket::{bucket, due_in_days, from_days};
use crate::bucket::{InvalidTransition, ResolutionStatus, due_bucket};
use crate::schemas::{OaAction, OaEvent};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};

// ---------- ResolutionStatus Display + FromStr ----------

#[test]
fn resolution_status_display_matches_serde_snake_case() {
    // 5 态各自 Display 必须与 serde snake_case 完全一致（导出包 / 审计链共用一份字符串）。
    assert_eq!(ResolutionStatus::Pending.to_string(), "pending");
    assert_eq!(ResolutionStatus::InProgress.to_string(), "in_progress");
    assert_eq!(ResolutionStatus::DueSoon.to_string(), "due_soon");
    assert_eq!(ResolutionStatus::Overdue.to_string(), "overdue");
    assert_eq!(ResolutionStatus::Completed.to_string(), "completed");
}

#[test]
fn resolution_status_fromstr_accepts_snake_and_kebab() {
    // snake_case（与序列化一致；导出包 / 审计链以此形态回流）
    let pending: ResolutionStatus = "pending".parse().unwrap();
    assert_eq!(pending, ResolutionStatus::Pending);
    let in_progress: ResolutionStatus = "in_progress".parse().unwrap();
    assert_eq!(in_progress, ResolutionStatus::InProgress);
    let due_soon: ResolutionStatus = "due_soon".parse().unwrap();
    assert_eq!(due_soon, ResolutionStatus::DueSoon);
    let overdue: ResolutionStatus = "overdue".parse().unwrap();
    assert_eq!(overdue, ResolutionStatus::Overdue);
    let completed: ResolutionStatus = "completed".parse().unwrap();
    assert_eq!(completed, ResolutionStatus::Completed);

    // kebab-case（外部系统偶尔用连字符；前端 / 邮件里也常出现）
    assert_eq!(
        "in-progress".parse::<ResolutionStatus>().unwrap(),
        ResolutionStatus::InProgress
    );
    assert_eq!(
        "due-soon".parse::<ResolutionStatus>().unwrap(),
        ResolutionStatus::DueSoon
    );

    // 未知值必须返回 Err，不能静默落到 Pending / Completed 等任何态。
    let err = "non-existent-state".parse::<ResolutionStatus>().unwrap_err();
    assert!(err.contains("未知的 ResolutionStatus"), "错误信息: {err}");
}

// ---------- InvalidTransition 错误结构 ----------

#[test]
fn invalid_transition_constructs_with_all_three_fields() {
    let err = InvalidTransition::new(
        ResolutionStatus::Completed,
        ResolutionStatus::InProgress,
        "终态后不再回退",
    );
    // 三个字段必须各自正确落位，便于导出包 / 审计链的机器可读 + 人类可读。
    assert_eq!(err.from, ResolutionStatus::Completed);
    assert_eq!(err.to, ResolutionStatus::InProgress);
    assert_eq!(err.reason, "终态后不再回退");
    // Display 同时输出 from / to / reason
    let s = err.to_string();
    assert!(s.contains("completed"));
    assert!(s.contains("in_progress"));
    assert!(s.contains("终态后不再回退"));
}

#[test]
fn invalid_transition_serializes_to_json() {
    // 关键不变量：InvalidTransition 须可被 `serde_json` 序列化进入审计链 / 导出包。
    let err = InvalidTransition::new(
        ResolutionStatus::Pending,
        ResolutionStatus::Completed,
        "缺 started / in_progress 事件",
    );
    let j = serde_json::to_string(&err).unwrap();
    assert!(j.contains("\"from\":\"pending\""));
    assert!(j.contains("\"to\":\"completed\""));
    assert!(j.contains("\"reason\":\"缺 started / in_progress 事件\""));
    // 反序列化必须 roundtrip（导出包 / 审计链回流时不丢失字段）。
    let back: InvalidTransition = serde_json::from_str(&j).unwrap();
    assert_eq!(back, err);
}

// ---------- ResolutionStatus 边界 ----------

#[test]
fn resolution_status_terminal_and_active_boundaries() {
    // 仅 Completed 是终态。
    assert!(ResolutionStatus::Completed.is_terminal());
    assert!(!ResolutionStatus::Completed.is_active());

    // 其余 4 态均为活跃态。
    for active in [
        ResolutionStatus::Pending,
        ResolutionStatus::InProgress,
        ResolutionStatus::DueSoon,
        ResolutionStatus::Overdue,
    ] {
        assert!(!active.is_terminal(), "{active:?} 不应是终态");
        assert!(active.is_active(), "{active:?} 应该是活跃态");
    }
}

// ---------- task 23 · 6 桶剩余天数派生 + overdue 桶负值判定 ----------

/// 6 桶边界各自正确：每个桶的临界值与桶内/桶外样本必须各自落到正确桶，
/// 边界条件（0/4/8/15/31、`< 0`）按 RFC 1882 严格判定，不模糊归并。
#[test]
fn task23_due_bucket_six_buckets_each_correct_at_boundary() {
    use crate::schemas::DueBucket;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    let today = date(2026, 10, 12);

    // Overdue：负值（< 0，含逾期守护）
    assert_eq!(bucket(today, date(2026, 10, 11)), DueBucket::Overdue);
    assert_eq!(bucket(today, date(2026, 9, 12)), DueBucket::Overdue); // -30
    assert_eq!(bucket(today, date(2025, 10, 12)), DueBucket::Overdue); // 跨年 -365

    // Lt3：[0, 3] 闭区间
    assert_eq!(bucket(today, today), DueBucket::Lt3); // 0
    assert_eq!(bucket(today, date(2026, 10, 13)), DueBucket::Lt3); // 1
    assert_eq!(bucket(today, date(2026, 10, 14)), DueBucket::Lt3); // 2
    assert_eq!(bucket(today, date(2026, 10, 15)), DueBucket::Lt3); // 3

    // Lt7：(3, 7] 右闭区间 —— 4 进 Lt7，7 仍进 Lt7
    assert_eq!(bucket(today, date(2026, 10, 16)), DueBucket::Lt7); // 4
    assert_eq!(bucket(today, date(2026, 10, 19)), DueBucket::Lt7); // 7

    // Lt14：(7, 14] 右闭区间 —— 8 进 Lt14，14 仍进 Lt14
    assert_eq!(bucket(today, date(2026, 10, 20)), DueBucket::Lt14); // 8
    assert_eq!(bucket(today, date(2026, 10, 26)), DueBucket::Lt14); // 14

    // Lt30：(14, 30] 右闭区间 —— 15 进 Lt30，30 仍进 Lt30
    assert_eq!(bucket(today, date(2026, 10, 27)), DueBucket::Lt30); // 15
    assert_eq!(bucket(today, date(2026, 11, 11)), DueBucket::Lt30); // 30

    // Gt30：> 30 —— 31、60、365 都进 Gt30
    assert_eq!(bucket(today, date(2026, 11, 12)), DueBucket::Gt30); // 31
    assert_eq!(bucket(today, date(2026, 12, 11)), DueBucket::Gt30); // 60
    assert_eq!(bucket(today, date(2027, 10, 12)), DueBucket::Gt30); // 365
}

/// Overdue 桶负值判定：任何 `due_at < today` 的情况都进 `Overdue`，不静默归并到其他桶。
/// 包括 -1、-30、-365、跨年大负数（含闰年与非闰年）。
#[test]
fn task23_overdue_bucket_negative_days_detection() {
    use crate::schemas::DueBucket;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    let today = date(2026, 10, 12);

    // -1：刚过期 1 天
    assert_eq!(bucket(today, date(2026, 10, 11)), DueBucket::Overdue);
    // -30：过期 1 个月
    assert_eq!(bucket(today, date(2026, 9, 12)), DueBucket::Overdue);
    // -100：过期 100 天
    assert_eq!(bucket(today, date(2026, 7, 4)), DueBucket::Overdue);
    // -365：去年今日
    assert_eq!(bucket(today, date(2025, 10, 12)), DueBucket::Overdue);
    // 跨闰年 2024（闰年）/2023（非闰年）大负值
    assert_eq!(bucket(today, date(2024, 2, 29)), DueBucket::Overdue);
    assert_eq!(bucket(today, date(2023, 2, 28)), DueBucket::Overdue);

    // from_days 极小值也不溢出
    assert_eq!(from_days(-1), DueBucket::Overdue);
    assert_eq!(from_days(-1_000), DueBucket::Overdue);
}

/// `bucket(current_date, due_at)` 与 `from_days(due_in_days)` 行为一致：
/// `bucket` 派生路径严格等于 `from_days((due_at - current_date).num_days())`。
#[test]
fn task23_bucket_function_matches_from_days_subtraction() {
    use crate::schemas::DueBucket;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    let cases: &[(NaiveDate, NaiveDate, DueBucket)] = &[
        (date(2026, 10, 12), date(2026, 10, 12), DueBucket::Lt3),
        (date(2026, 10, 12), date(2026, 10, 19), DueBucket::Lt7),
        (date(2026, 10, 12), date(2026, 10, 26), DueBucket::Lt14),
        (date(2026, 10, 12), date(2026, 11, 11), DueBucket::Lt30),
        (date(2026, 10, 12), date(2026, 11, 12), DueBucket::Gt30),
        (date(2026, 10, 12), date(2026, 10, 11), DueBucket::Overdue),
    ];

    for (today, due, expected) in cases {
        let days = due_in_days(*today, *due);
        assert_eq!(from_days(days), *expected, "days={days} today={today} due={due}");
        assert_eq!(bucket(*today, *due), *expected, "bucket 直接派生 today={today} due={due}");
    }
}

/// 6 桶间相邻边界的过渡：每两个相邻桶的临界点必须落到正确桶，
/// 不允许"模糊归并"或"差 1 落到错误桶"。
#[test]
fn task23_six_buckets_transitions_have_no_fuzzy_merging() {
    use crate::schemas::DueBucket;

    // 临界：0..=3 与 (3, 7] 之间 day=4 必须进 Lt7，不能进 Lt3
    assert_ne!(from_days(3), DueBucket::Lt7);
    assert_eq!(from_days(4), DueBucket::Lt7);

    // 临界：(3, 7] 与 (7, 14] 之间 day=8 必须进 Lt14，不能进 Lt7
    assert_ne!(from_days(7), DueBucket::Lt14);
    assert_eq!(from_days(8), DueBucket::Lt14);

    // 临界：(7, 14] 与 (14, 30] 之间 day=15 必须进 Lt30，不能进 Lt14
    assert_ne!(from_days(14), DueBucket::Lt30);
    assert_eq!(from_days(15), DueBucket::Lt30);

    // 临界：(14, 30] 与 > 30 之间 day=31 必须进 Gt30，不能进 Lt30
    assert_ne!(from_days(30), DueBucket::Gt30);
    assert_eq!(from_days(31), DueBucket::Gt30);

    // 负值边界：-1 进 Overdue，0 进 Lt3
    assert_eq!(from_days(-1), DueBucket::Overdue);
    assert_ne!(from_days(0), DueBucket::Overdue);
    assert_eq!(from_days(0), DueBucket::Lt3);
}

/// `due_in_days` 与 (due_at - current_date).num_days() 完全等价，
/// 包括正/负/零/跨月/跨年的所有方向。
#[test]
fn task23_due_in_days_helper_returns_chrono_subtraction() {
    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    // 正向：today < due_at
    assert_eq!(due_in_days(date(2026, 10, 12), date(2026, 11, 11)), 30);
    // 零：同一天
    assert_eq!(due_in_days(date(2026, 10, 12), date(2026, 10, 12)), 0);
    // 反向：today > due_at → 负值
    assert_eq!(due_in_days(date(2026, 10, 12), date(2026, 10, 11)), -1);
    assert_eq!(due_in_days(date(2026, 10, 12), date(2026, 9, 12)), -30);
    // 跨年
    assert_eq!(due_in_days(date(2026, 1, 1), date(2025, 12, 31)), -1);
    // 闰年附近
    assert_eq!(due_in_days(date(2024, 3, 1), date(2024, 2, 29)), -1);
}

/// 桶派生层默认从 `bucket` 子模块导出，
/// 调用方通过 `bucket::due_bucket::*` 或 `use crate::bucket::due_bucket` 都能调到。
#[test]
fn task23_due_bucket_module_reexports_helpers() {
    // 公开符号：bucket / from_days / due_in_days
    let today = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
    let due = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
    let _ = bucket(today, due);
    let _ = from_days(0);
    let _ = due_in_days(today, due);

    // due_bucket 子模块本体可访问
    let _ = due_bucket::from_days;
}

// ---------- task 24 · OA event → Resolution status 应用规则 + 跨状态派生 ----------

/// 测试 fixture helpers（task 24 集中管理一份，省得各测试重复代码）
mod task24_fixtures {
    use super::*;

    pub fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 12).unwrap()
    }

    pub fn event_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap()
    }

    pub fn ev(action: OaAction, id: &str) -> OaEvent {
        OaEvent {
            oa_event_id: id.to_string(),
            resolution_id: "#[W41]01".to_string(),
            action,
            actor_id: "E10086".to_string(),
            event_time: event_time(),
        }
    }

    pub fn today_date() -> NaiveDate {
        today()
    }

    pub fn due_in(days: i64) -> NaiveDate {
        today() + chrono::Duration::days(days)
    }
}

/// task 24 · 验收点 1：5 种 OA action 各自在标准起始态上的转换正确。
///
/// `started` / `in_progress` / `submitted (≤3 天 → due_soon)` /
/// `submitted (>3 天 → 保持)` / `completed`（合法 from 上）/ `blocked`（不变）。
/// 这是 task 24 的核心验收点：OA event 的 5 类 action 都能被映射到正确的 `ResolutionStatus`。
#[test]
fn task24_advance_maps_five_actions_to_correct_status() {
    use task24_fixtures::*;

    // started: pending → in_progress
    let r = advance_with_due(
        ResolutionStatus::Pending,
        &ev(OaAction::Started, "OA-T24-S"),
        due_in(8),
        today_date(),
    )
    .expect("started from pending 应合法");
    assert_eq!(r, ResolutionStatus::InProgress);

    // in_progress: pending → in_progress（idempotent 起算）
    let r = advance_with_due(
        ResolutionStatus::Pending,
        &ev(OaAction::InProgress, "OA-T24-IP"),
        due_in(8),
        today_date(),
    )
    .expect("in_progress from pending 应合法");
    assert_eq!(r, ResolutionStatus::InProgress);

    // submitted (≤ 3 天) → due_soon
    let r = advance_with_due(
        ResolutionStatus::InProgress,
        &ev(OaAction::Submitted, "OA-T24-SUB-NEAR"),
        due_in(2),
        today_date(),
    )
    .expect("submitted near due 应合法 → due_soon");
    assert_eq!(r, ResolutionStatus::DueSoon);

    // submitted (> 3 天) → 保持 in_progress
    let r = advance_with_due(
        ResolutionStatus::InProgress,
        &ev(OaAction::Submitted, "OA-T24-SUB-FAR"),
        due_in(30),
        today_date(),
    )
    .expect("submitted far due 应合法 → in_progress");
    assert_eq!(r, ResolutionStatus::InProgress);

    // completed (from in_progress) → completed
    let r = advance_with_due(
        ResolutionStatus::InProgress,
        &ev(OaAction::Completed, "OA-T24-COMP-IP"),
        due_in(8),
        today_date(),
    )
    .expect("completed from in_progress 应合法");
    assert_eq!(r, ResolutionStatus::Completed);

    // blocked：不变（4 个非终态都保持自身）
    for from in [
        ResolutionStatus::Pending,
        ResolutionStatus::InProgress,
        ResolutionStatus::DueSoon,
        ResolutionStatus::Overdue,
    ] {
        let r = advance_with_due(
            from,
            &ev(OaAction::Blocked, "OA-T24-BLOCK"),
            due_in(8),
            today_date(),
        )
        .expect("blocked 不应报错");
        assert_eq!(r, from, "blocked 应保持原状态 from {from:?}");
    }
}

/// task 24 · 验收点 2：非法 OA event / OA event + 状态组合返回 `InvalidTransition`，
/// 绝不静默改写 `current` 决议状态。
///
/// 覆盖 task 24 的核心不变量：
/// - pending + completed → 拒绝（必须先经 started/in_progress）
/// - completed 后任何 action → 拒绝（终态锁）
/// - due_soon + started → 拒绝（不能借 Started 倒推到 InProgress）
/// - overdue + started → 拒绝（re-engagement 须靠 Submitted）
#[test]
fn task24_advance_rejects_illegal_action_combinations() {
    use task24_fixtures::*;

    // pending + completed 必须先经 started/in_progress
    let err = advance_with_due(
        ResolutionStatus::Pending,
        &ev(OaAction::Completed, "OA-T24-PEND-COMP"),
        due_in(8),
        today_date(),
    )
    .expect_err("pending + completed 应被拒");
    assert_eq!(err.from, ResolutionStatus::Pending);
    assert_eq!(err.to, ResolutionStatus::Completed);
    assert!(
        err.reason.contains("started") || err.reason.contains("in_progress"),
        "reason 应说明需先经 started/in_progress: {}",
        err.reason
    );

    // completed + started：终态锁
    let err = advance_with_due(
        ResolutionStatus::Completed,
        &ev(OaAction::Started, "OA-T24-COMP-STARTED"),
        due_in(8),
        today_date(),
    )
    .expect_err("completed + started 应被拒");
    assert_eq!(err.from, ResolutionStatus::Completed);
    assert_eq!(err.to, ResolutionStatus::Completed); // 终态守卫把 to 也设为自身

    // completed + submitted：终态锁
    let err = advance_with_due(
        ResolutionStatus::Completed,
        &ev(OaAction::Submitted, "OA-T24-COMP-SUB"),
        due_in(8),
        today_date(),
    )
    .expect_err("completed + submitted 应被拒");
    assert_eq!(err.from, ResolutionStatus::Completed);

    // completed + completed：终态锁（同一 action 也不允许）
    let err = advance_with_due(
        ResolutionStatus::Completed,
        &ev(OaAction::Completed, "OA-T24-COMP-COMP"),
        due_in(8),
        today_date(),
    )
    .expect_err("completed + completed 应被拒");
    assert_eq!(err.from, ResolutionStatus::Completed);

    // due_soon + started：不能借 Started 倒推到 InProgress
    let err = advance_with_due(
        ResolutionStatus::DueSoon,
        &ev(OaAction::Started, "OA-T24-DUE-STARTED"),
        due_in(1),
        today_date(),
    )
    .expect_err("due_soon + started 应被拒");
    assert_eq!(err.from, ResolutionStatus::DueSoon);
    assert_eq!(err.to, ResolutionStatus::InProgress);

    // overdue + started：re-engagement 须走 Submitted，其他 action 拒绝
    let err = advance_with_due(
        ResolutionStatus::Overdue,
        &ev(OaAction::Started, "OA-T24-OVER-STARTED"),
        due_in(-3),
        today_date(),
    )
    .expect_err("overdue + started 应被拒（仅 Submitted 允许 re-engagement）");
    assert_eq!(err.from, ResolutionStatus::Overdue);
    assert_eq!(err.to, ResolutionStatus::InProgress);
}

/// task 24 · 验收点 3：跨状态派生 —— `advance()`（无显式 due_at）与 `advance_with_due`
/// 在「Pending + Started」标准路径上语义一致；try_advance_with_due 成功路径穿过
/// validate_event + advance_with_due；空 OA event_id / resolution_id 走 MissingEvent 分支。
///
/// 这一组测试守住 task 24 的"跨状态派生"边界：
/// - `advance()` 是无显式 due 的简洁入口，Pending+Started 落到 InProgress；
/// - `try_advance_with_due(Some(&event))` 走完整合法路径；
/// - `try_advance_with_due(None)` 与空字段事件走 `BucketError::MissingEvent`
///   而**不是** `InvalidTransition`（区分两种错误语义）。
#[test]
fn task24_advance_cross_state_derivation_and_missing_event_branch() {
    use task24_fixtures::*;

    // 跨状态派生 1：`advance()` 无 due_at 路径在 Pending+Started 上落到 InProgress
    let r = advance(
        ResolutionStatus::Pending,
        &ev(OaAction::Started, "OA-T24-CROSS-1"),
        today_date(),
    )
    .expect("advance() Pending+Started 应合法");
    assert_eq!(r, ResolutionStatus::InProgress);

    // 跨状态派生 2：`try_advance_with_due(Some(&event))` 成功穿过
    let r = try_advance_with_due(
        ResolutionStatus::Pending,
        Some(&ev(OaAction::Started, "OA-T24-CROSS-2")),
        due_in(8),
        today_date(),
    )
    .expect("try_advance_with_due 完整 event 应成功");
    assert_eq!(r, ResolutionStatus::InProgress);

    // 缺事件：None → MissingEvent（不是 InvalidTransition）
    let err = try_advance_with_due(ResolutionStatus::Pending, None, due_in(8), today_date())
        .expect_err("None event 必须返回 MissingEvent");
    assert!(
        matches!(err, crate::bucket::BucketError::MissingEvent(_)),
        "应为 MissingEvent，实际是: {err:?}"
    );

    // 空 oa_event_id → MissingEvent（不是 InvalidTransition）
    let bad = OaEvent {
        oa_event_id: "".to_string(),
        resolution_id: "#[W41]01".to_string(),
        action: OaAction::Started,
        actor_id: "E10086".to_string(),
        event_time: event_time(),
    };
    let err = try_advance_with_due(ResolutionStatus::Pending, Some(&bad), due_in(8), today_date())
        .expect_err("空 oa_event_id 必须返回 MissingEvent");
    assert!(
        matches!(err, crate::bucket::BucketError::MissingEvent(_)),
        "应为 MissingEvent，实际是: {err:?}"
    );
}
