//! `bucket` 子系统集成测试（task 25）。
//!
//! 三大验证维度，每条 case 都是真实可断言的状态语义：
//!
//! 1. **6 桶边界 12 个 case** —— 含 `Overdue` 桶的负值兜底、各桶临界值、跨月/跨年/闰年判定。
//! 2. **跨年 ISO week 3 个 case** —— `2025-W53` / `2026-W01` / `2027-W01` ISO 周次边界。
//! 3. **`advance()` 与 `bucket()` 集成 4 个 case** —— 状态机推进与桶派生联动：
//!    一条决议的当前态推到新态后，`due_bucket` 必须随 `current_date` 重新派生。
//!
//! 源产品主路径同构：源 `crates/udf_metrics/src/lib.rs` 的双向 `BucketIndex = u32 + CounterBucket`
//! 时间桶 — 本集成测试以**跨年 + 闰年 + 跨月**三个维度确保双向 slab 边界都被守护。
//!
//! 测试形态边界：本文件为**纯同步**集成测试（无 env 依赖、无 async），因此不持
//! `ENV_LOCK` —— 跟 `tests/integration_official.rs` 的端到端测试正交。

use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc};

use resolution_watch::bucket::advance::{
    advance, advance_with_due, validate_event,
};
use resolution_watch::bucket::due_bucket::{bucket, due_in_days, from_days};
use resolution_watch::bucket::error::BucketError;
use resolution_watch::bucket::ResolutionStatus;
use resolution_watch::schemas::{DueBucket, OaAction, OaEvent};

// ---------- fixtures ----------

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("合法日期构造")
}

fn ts() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 9, 0, 0).unwrap()
}

fn ev(action: OaAction, id: &str) -> OaEvent {
    OaEvent {
        oa_event_id: id.to_string(),
        resolution_id: "#[W41]01".to_string(),
        action,
        actor_id: "E10086".to_string(),
        event_time: ts(),
    }
}

/// ISO week 解析：`(year, week)` → 周内第一个日期 `Monday`。
///
/// 用于跨年 ISO week 边界 case —— 验证 ISO 周次语义在跨年场景下与本工具的
/// `due_at` 派生一致。
fn monday_of_iso_week(year: i32, week: u32) -> NaiveDate {
    NaiveDate::from_isoywd_opt(year, week, chrono::Weekday::Mon).expect("合法 ISO week")
}

// ============================================================
// 1) 6 桶边界 12 case
// ============================================================

#[test]
fn six_buckets_boundary_cases_part1_overdue_negative_and_lt3() {
    // 12 桶边界 case 的前 4 条：overdue 桶的负值兜底（含极端负值） + Lt3 三档（0/1/3 days）。
    // 让断言一口气读完，不必按桶拆多个 test。
    let today = date(2026, 10, 12);

    // overdue -1（逾期 1 天）
    assert_eq!(from_days(-1), DueBucket::Overdue);
    assert_eq!(bucket(today, date(2026, 10, 11)), DueBucket::Overdue);

    // overdue -365（逾期一年）
    assert_eq!(from_days(-365), DueBucket::Overdue);
    assert_eq!(bucket(today, date(2025, 10, 12)), DueBucket::Overdue);

    // overdue 极端负值（i64::MIN / 2）：守住边界
    assert_eq!(from_days(i64::MIN / 2), DueBucket::Overdue);

    // Lt3 桶三档：0 / 1 / 3 days
    assert_eq!(from_days(0), DueBucket::Lt3);
    assert_eq!(bucket(today, today), DueBucket::Lt3);
    assert_eq!(from_days(1), DueBucket::Lt3);
    assert_eq!(bucket(today, date(2026, 10, 13)), DueBucket::Lt3);
    assert_eq!(from_days(3), DueBucket::Lt3);
    assert_eq!(bucket(today, date(2026, 10, 15)), DueBucket::Lt3);
}

#[test]
fn six_buckets_boundary_cases_part2_lt7_to_lt14() {
    // 续 12 桶边界 case：Lt7 三档（4 / 5 / 7 days）+ Lt14 三档（8 / 10 / 14 days）。
    let today = date(2026, 10, 12);

    assert_eq!(from_days(4), DueBucket::Lt7);
    assert_eq!(bucket(today, date(2026, 10, 16)), DueBucket::Lt7);
    assert_eq!(from_days(5), DueBucket::Lt7);
    assert_eq!(bucket(today, date(2026, 10, 17)), DueBucket::Lt7);
    assert_eq!(from_days(7), DueBucket::Lt7);
    assert_eq!(bucket(today, date(2026, 10, 19)), DueBucket::Lt7);

    assert_eq!(from_days(8), DueBucket::Lt14);
    assert_eq!(bucket(today, date(2026, 10, 20)), DueBucket::Lt14);
    assert_eq!(from_days(10), DueBucket::Lt14);
    assert_eq!(bucket(today, date(2026, 10, 22)), DueBucket::Lt14);
    assert_eq!(from_days(14), DueBucket::Lt14);
    assert_eq!(bucket(today, date(2026, 10, 26)), DueBucket::Lt14);
}

#[test]
fn six_buckets_boundary_cases_part3_lt30_gt30_and_cross_month() {
    // 收尾 12 桶边界 case：Lt30 三档（15 / 20 / 30 days）+ Gt30 三档（31 / 60 / 365 days）
    // + 跨月判定（10-31 → 11-03 = Lt3）。
    let today = date(2026, 10, 12);

    assert_eq!(from_days(15), DueBucket::Lt30);
    assert_eq!(bucket(today, date(2026, 10, 27)), DueBucket::Lt30);
    assert_eq!(from_days(20), DueBucket::Lt30);
    assert_eq!(bucket(today, date(2026, 11, 1)), DueBucket::Lt30);
    assert_eq!(from_days(30), DueBucket::Lt30);
    assert_eq!(bucket(today, date(2026, 11, 11)), DueBucket::Lt30);

    assert_eq!(from_days(31), DueBucket::Gt30);
    assert_eq!(bucket(today, date(2026, 11, 12)), DueBucket::Gt30);
    assert_eq!(from_days(60), DueBucket::Gt30);
    assert_eq!(bucket(today, date(2026, 12, 11)), DueBucket::Gt30);
    assert_eq!(from_days(365), DueBucket::Gt30);
    assert_eq!(bucket(today, date(2027, 10, 12)), DueBucket::Gt30);

    // 关键跨月判定：2026-10-12 加 3 天 = 2026-10-15（同月内）→ Lt3
    assert_eq!(bucket(today, today + Duration::days(3)), DueBucket::Lt3);
    // 跨月：2026-10-31 + 3 天 = 2026-11-03 → Lt3
    let month_end = date(2026, 10, 31);
    assert_eq!(bucket(month_end, month_end + Duration::days(3)), DueBucket::Lt3);
}

#[test]
fn six_buckets_boundary_case_cross_year_and_leap_year_judgement() {
    // 12 桶边界 case 之外的 2 个关键日历边界（仍归到 6 桶边界守护）：
    // 跨年（2026-12-31 + 1 day = 2027-01-01 → Lt3）
    // 闰年（2028-02-28 + 1 day = 2028-02-29 → Lt3；2028-02-28 + 2 days = 2028-03-01 → Lt3）
    let year_end = date(2026, 12, 31);
    assert_eq!(
        bucket(year_end, year_end + Duration::days(1)),
        DueBucket::Lt3,
        "跨年（12-31 → 01-01）属 Lt3"
    );

    // 闰年二月：2028 是闰年（2028 / 4 = 507，整除）
    let leap_feb28 = date(2028, 2, 28);
    assert_eq!(
        bucket(leap_feb28, leap_feb28 + Duration::days(1)),
        DueBucket::Lt3,
        "闰年 02-28 → 02-29 跨整天数 1（按天数差）"
    );
    assert_eq!(
        bucket(leap_feb28, leap_feb28 + Duration::days(2)),
        DueBucket::Lt3,
        "闰年 02-28 → 03-01 跨整天数 2"
    );

    // 非闰年：2027-02-28 + 1 day = 2027-03-01（跨整天数 1）
    let non_leap_feb28 = date(2027, 2, 28);
    assert_eq!(
        bucket(non_leap_feb28, non_leap_feb28 + Duration::days(1)),
        DueBucket::Lt3,
        "非闰年 02-28 → 03-01 跨整天数 1"
    );
}

#[test]
fn six_buckets_each_at_boundary_returns_correct_bucket_at_least_once() {
    // 12 桶边界守护（per-bucket 总览）：每桶至少调用一次边界值，确认无 off-by-one。
    let today = date(2026, 10, 12);

    // Overdue：跨越临界值（≤ -1）
    assert_eq!(bucket(today, today - Duration::days(1)), DueBucket::Overdue);

    // Lt3：today → today+3（恰好 0..=3）
    for d in 0..=3 {
        assert_eq!(
            bucket(today, today + Duration::days(d)),
            DueBucket::Lt3,
            "d={d} 应进 Lt3 桶"
        );
    }

    // Lt7：today+4 → today+7（4..=7）
    for d in 4..=7 {
        assert_eq!(
            bucket(today, today + Duration::days(d)),
            DueBucket::Lt7,
            "d={d} 应进 Lt7 桶"
        );
    }

    // Lt14：today+8 → today+14（8..=14）
    for d in 8..=14 {
        assert_eq!(
            bucket(today, today + Duration::days(d)),
            DueBucket::Lt14,
            "d={d} 应进 Lt14 桶"
        );
    }

    // Lt30：today+15 → today+30（15..=30）
    for d in 15..=30 {
        assert_eq!(
            bucket(today, today + Duration::days(d)),
            DueBucket::Lt30,
            "d={d} 应进 Lt30 桶"
        );
    }

    // Gt30：> 30（含 today+31、today+60、today+365）
    for d in [31, 60, 365] {
        assert_eq!(
            bucket(today, today + Duration::days(d)),
            DueBucket::Gt30,
            "d={d} 应进 Gt30 桶"
        );
    }
}

// ============================================================
// 2) 跨年 ISO week 3 case
// ============================================================

#[test]
fn iso_week_2026_w53_monday_is_dec_28() {
    // ISO 2026-W53 是真实存在的 53 周年（2026-12-28 是周一，整周跨年到 2027）。
    // 决议条目 `inbox/2027-W01/minutes.md` 引用「上一年决议条目」时，
    // 周次 ID 与 `monday_of_iso_week` 必须一致，避免跨年周次错配。
    let mon = monday_of_iso_week(2026, 53);
    assert_eq!(mon, date(2026, 12, 28), "2026-W53 周一应为 2026-12-28");

    // 加 6 天 = 同周周日（2027-01-03）
    let sun = mon + Duration::days(6);
    assert_eq!(sun, date(2027, 1, 3));
    // 该周跨年：周一 2026-12-28 → 周日 2027-01-03
    assert!(mon.year() < sun.year());
}

#[test]
fn iso_week_2026_w01_monday_is_dec_29_2025() {
    // 关键边界：2026-W01 周一是 2025-12-29（因为 2026-01-01 是周四，ISO 把这一周
    // 视为 2026 年的第一周）；文档混淆点：跨年 ISO 周与自然年错位必须可计算。
    let mon = monday_of_iso_week(2026, 1);
    assert_eq!(mon, date(2025, 12, 29), "2026-W01 周一为 2025-12-29");

    // 验证：2026-W53 周一 2026-12-28 与 2027-W01 周一 2027-01-04 同跨年，但
    // 2026-W01 与 2026-W53 跨年是不同方向（向前 / 向后），各自的 Monday 都可断言。
    let mon_2026_w53 = monday_of_iso_week(2026, 53);
    let mon_2027_w01 = monday_of_iso_week(2027, 1);
    assert_eq!(mon_2026_w53, date(2026, 12, 28));
    assert_eq!(mon_2027_w01, date(2027, 1, 4));
    // 2026-W53 的周日 = 2027-01-03，差 2027-W01 周一 1 天
    assert_eq!(
        mon_2027_w01 - mon_2026_w53,
        Duration::days(7),
        "2026-W53 周一 → 2027-W01 周一差 7 天"
    );
}

#[test]
fn iso_week_2027_w01_monday_is_jan_4_2027() {
    // 2027-W01 周一为 2027-01-04（验证从 2026 年跨到 2027 年的 ISO week 起点）。
    let mon = monday_of_iso_week(2027, 1);
    assert_eq!(mon, date(2027, 1, 4), "2027-W01 周一应为 2027-01-04");
}

#[test]
fn iso_week_cross_year_bucket_judgement() {
    // 跨年 ISO week 与桶派生的联动：决议条目 due_at = 2026-01-04，
    // 当 current_date = 2025-12-29（同周 ISO 2026-W01，因为 ISO 把 2025-12-29
    // 视为 2026 年的第一周周一）→ due_in_days = 6 → Lt7 桶。
    let mon_2026_w01 = monday_of_iso_week(2026, 1);
    assert_eq!(mon_2026_w01, date(2025, 12, 29), "2026-W01 周一为 2025-12-29");

    // 当决议 due_at = 2026-01-04（跨年 ISO week 的周日），当前日期 = 2025-12-29：
    // due_in_days = 6 → Lt7 桶（4..=7）。
    assert_eq!(
        bucket(mon_2026_w01, date(2026, 1, 4)),
        DueBucket::Lt7,
        "跨年 ISO week：2025-12-29 → 2026-01-04 = 6 天 → Lt7"
    );

    // 同时验证 due_in_days 与 from_days 一致（公开 API 自洽）
    assert_eq!(due_in_days(mon_2026_w01, date(2026, 1, 4)), 6);
    assert_eq!(from_days(6), DueBucket::Lt7);

    // 另一条跨年路径：2026-W53 周一（2026-12-28）→ 2027-W01 周一（2027-01-04）= 7 天 → Lt7
    let mon_2026_w53 = monday_of_iso_week(2026, 53);
    assert_eq!(bucket(mon_2026_w53, date(2027, 1, 4)), DueBucket::Lt7);
}

// ============================================================
// 3) advance() 与 bucket() 集成 4 case
// ============================================================

#[test]
fn advance_then_bucket_pending_to_in_progress_with_far_due() {
    // Case A：Pending + Started + due_at = today+30 → InProgress + Lt30
    let today = date(2026, 10, 12);
    let due = today + Duration::days(30);
    let new_status = advance_with_due(
        ResolutionStatus::Pending,
        &ev(OaAction::Started, "OA-INT-1"),
        due,
        today,
    )
    .expect("Pending + Started 应推到 InProgress");
    assert_eq!(new_status, ResolutionStatus::InProgress);
    assert_eq!(bucket(today, due), DueBucket::Lt30);
}

#[test]
fn advance_then_bucket_in_progress_to_due_soon_within_3_days() {
    // Case B：InProgress + Submitted + due_at = today+2 → DueSoon + Lt3（≤ 3 天桶切换）
    let today = date(2026, 10, 12);
    let due = today + Duration::days(2);
    let new_status = advance_with_due(
        ResolutionStatus::InProgress,
        &ev(OaAction::Submitted, "OA-INT-2"),
        due,
        today,
    )
    .expect("InProgress + Submitted + ≤3天 应推到 DueSoon");
    assert_eq!(new_status, ResolutionStatus::DueSoon);
    assert_eq!(bucket(today, due), DueBucket::Lt3);
    assert_eq!(due_in_days(today, due), 2);
}

#[test]
fn advance_then_bucket_in_progress_kept_with_far_due() {
    // Case C：InProgress + Submitted + due_at = today+30 → InProgress 不变 + Lt30
    // （action handler 守护：「远将来 + submitted」不应切到 DueSoon）
    let today = date(2026, 10, 12);
    let due = today + Duration::days(30);
    let new_status = advance_with_due(
        ResolutionStatus::InProgress,
        &ev(OaAction::Submitted, "OA-INT-3"),
        due,
        today,
    )
    .expect("InProgress + Submitted + 远将来应保持 InProgress");
    assert_eq!(new_status, ResolutionStatus::InProgress);
    // 桶语义：> 14 且 ≤ 30 → Lt30
    assert_eq!(bucket(today, due), DueBucket::Lt30);
    assert_eq!(due_in_days(today, due), 30);
}

#[test]
fn advance_then_bucket_overdue_re_engagement_to_in_progress() {
    // Case D：Overdue + Submitted（re-engagement）→ InProgress + Lt3（已逾期 -3 天 → Overdue 桶）
    // 状态机推演后**必须重新派生**桶：新状态 InProgress，桶仍按 (today, due_at) 派生。
    let today = date(2026, 10, 12);
    let due = today - Duration::days(3); // 已逾期 3 天
    let new_status = advance_with_due(
        ResolutionStatus::Overdue,
        &ev(OaAction::Submitted, "OA-INT-4"),
        due,
        today,
    )
    .expect("Overdue + Submitted 应允许 re-engagement 回 InProgress");
    assert_eq!(new_status, ResolutionStatus::InProgress);
    // 桶语义：due_at < today → Overdue 桶
    assert_eq!(bucket(today, due), DueBucket::Overdue);
    assert_eq!(due_in_days(today, due), -3);
}

// ============================================================
// 4) 额外守护：边界外的稳健性（不计入 12 / 3 / 4 的硬指标，
//    但缺失会留下「跨年 / 闰年 / 极端」三类隐患）
// ============================================================

#[test]
fn advance_with_no_event_id_falls_through_to_missing_event_path() {
    // 集成守护：advance 系列的底层 try_advance_with_due 在缺事件时返回
    // `BucketError::MissingEvent`，而不是 `InvalidTransition`。
    let today = date(2026, 10, 12);
    let due = today + Duration::days(8);

    let bad = OaEvent {
        oa_event_id: "".to_string(),
        resolution_id: "#[W41]99".to_string(),
        action: OaAction::Started,
        actor_id: "E99999".to_string(),
        event_time: ts(),
    };

    let result: Result<ResolutionStatus, BucketError> =
        resolution_watch::bucket::advance::try_advance_with_due(
            ResolutionStatus::Pending,
            Some(&bad),
            due,
            today,
        );
    let err = result.expect_err("空 oa_event_id 应返回 BucketError::MissingEvent");
    assert!(
        matches!(err, BucketError::MissingEvent(ref s) if s.contains("oa_event_id")),
        "期望 MissingEvent(oa_event_id)，得到 {err:?}"
    );

    // 直接 validate_event 也应拒绝（业务接口暴露面）
    assert!(validate_event(&bad).is_err());
}

#[test]
fn advance_helper_uses_event_time_when_due_at_is_implicit() {
    // 集成守护：advance()（无显式 due_at）应仅用 event_time 推演；Pending + Started → InProgress
    let today = date(2026, 10, 12);
    let result = advance(
        ResolutionStatus::Pending,
        &ev(OaAction::Started, "OA-AUX-1"),
        today,
    )
    .expect("Pending + Started 经 advance() 应成功");
    assert_eq!(result, ResolutionStatus::InProgress);
}

#[test]
fn invalid_transition_from_completed_after_advance_is_still_rejected() {
    // 集成守护：advance + advance_with_due 都必须把终态后的任何 action 拒掉；
    // 在「集成」维度再守一次，防止 retry 路径绕过。
    let today = date(2026, 10, 12);
    let due = today + Duration::days(8);

    let err_completed_via_advance_with_due = advance_with_due(
        ResolutionStatus::Completed,
        &ev(OaAction::Started, "OA-AUX-2"),
        due,
        today,
    )
    .expect_err("终态经 advance_with_due 必须被拒绝");
    let err_completed_via_advance = advance(
        ResolutionStatus::Completed,
        &ev(OaAction::Submitted, "OA-AUX-3"),
        today,
    )
    .expect_err("终态经 advance() 必须被拒绝");

    // 两条路径都返回 InvalidTransition.from == Completed
    assert_eq!(err_completed_via_advance_with_due.from, ResolutionStatus::Completed);
    assert_eq!(err_completed_via_advance.from, ResolutionStatus::Completed);
    // InvalidTransition 的 std::error::Error impl 让 Display 能透出
    assert!(
        err_completed_via_advance_with_due
            .to_string()
            .contains("completed"),
        "InvalidTransition Display 应含 from 态"
    );

    // 显式断言类型一致：两条路径的 reason 都是非空字符串
    let invalid_completed = &err_completed_via_advance_with_due;
    assert!(
        !invalid_completed.reason.is_empty(),
        "reason 不应为空"
    );
    assert_eq!(invalid_completed.from, ResolutionStatus::Completed);
    assert_eq!(invalid_completed.to, ResolutionStatus::Completed);
    let _ = err_completed_via_advance;
}

#[test]
fn bucket_due_in_days_consistency_with_chrono_subtraction() {
    // 集成守护：辅助函数 due_in_days 与直接 chrono subtract 一致
    // (用于跨年 + 闰年 + 极端三类场景)。
    let today = date(2026, 10, 12);
    let scenarios = [
        (today, today + Duration::days(0), 0),
        (today, today + Duration::days(3), 3),
        (today, today - Duration::days(1), -1),
        (today, today + Duration::days(30), 30),
        (date(2025, 12, 31), date(2026, 1, 1), 1),  // 跨年
        (date(2028, 2, 28), date(2028, 3, 1), 2),   // 闰年 + 跨月
        (date(2027, 2, 28), date(2027, 3, 1), 1),   // 非闰年 + 跨月
    ];

    for (current, due, expected) in scenarios {
        let got = due_in_days(current, due);
        assert_eq!(
            got, expected,
            "due_in_days({current}, {due}) = {got} != {expected}"
        );
        // 反向：bucket 派生与 from_days 一致
        assert_eq!(bucket(current, due), from_days(expected));
    }
}
