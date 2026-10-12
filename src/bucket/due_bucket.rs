//! 6 桶剩余天数派生。
//!
//! 把 `due_in_days = (due_at - current_date).num_days()` 映射到 6 个 `DueBucket`：
//! - `due_in_days <  0`           → `Overdue`（负值，含逾期桶边界守护）
//! - `due_in_days ∈ [0, 3]`      → `Lt3`
//! - `due_in_days ∈ (3, 7]`      → `Lt7`
//! - `due_in_days ∈ (7, 14]`     → `Lt14`
//! - `due_in_days ∈ (14, 30]`    → `Lt30`
//! - `due_in_days > 30`          → `Gt30`
//!
//! 边界按 RFC 1882（分桶函数）严格判定：`<= 3` 进 `Lt3`、`<= 7` 进 `Lt7` 等；
//! **不**模糊归并，便于下游单测守护每条 case。

use chrono::NaiveDate;

use crate::schemas::DueBucket;

/// 把 `(current_date, due_at)` 转成 `due_in_days`，再映射到 6 桶。
///
/// 负值（逾期）落到 `Overdue` 桶，**`DueInDays` 始终按 today 算**，便于单测断言。
pub fn bucket(current_date: NaiveDate, due_at: NaiveDate) -> DueBucket {
    let days = (due_at - current_date).num_days();
    from_days(days)
}

/// 按 `due_in_days` 数值映射到 6 桶（导出 + 边界单测直接调）。
///
/// 边界：
/// - `<  0`   → `Overdue`
/// - `0..=3`  → `Lt3`
/// - `4..=7`  → `Lt7`
/// - `8..=14` → `Lt14`
/// - `15..=30`→ `Lt30`
/// - `> 30`   → `Gt30`
pub fn from_days(days: i64) -> DueBucket {
    if days < 0 {
        DueBucket::Overdue
    } else if days <= 3 {
        DueBucket::Lt3
    } else if days <= 7 {
        DueBucket::Lt7
    } else if days <= 14 {
        DueBucket::Lt14
    } else if days <= 30 {
        DueBucket::Lt30
    } else {
        DueBucket::Gt30
    }
}

/// 直接由 `(current_date, due_at)` 派生 `due_in_days`（负值 = 已逾期）。
pub fn due_in_days(current_date: NaiveDate, due_at: NaiveDate) -> i64 {
    (due_at - current_date).num_days()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn overdue_bucket_for_negative_days() {
        // due < today → 逾期
        assert_eq!(from_days(-1), DueBucket::Overdue);
        assert_eq!(from_days(-30), DueBucket::Overdue);
        assert_eq!(from_days(i64::MIN / 2), DueBucket::Overdue);
    }

    #[test]
    fn lt3_bucket_covers_zero_to_three() {
        assert_eq!(from_days(0), DueBucket::Lt3);
        assert_eq!(from_days(1), DueBucket::Lt3);
        assert_eq!(from_days(2), DueBucket::Lt3);
        assert_eq!(from_days(3), DueBucket::Lt3);
    }

    #[test]
    fn lt7_bucket_covers_four_to_seven() {
        assert_eq!(from_days(4), DueBucket::Lt7);
        assert_eq!(from_days(5), DueBucket::Lt7);
        assert_eq!(from_days(6), DueBucket::Lt7);
        assert_eq!(from_days(7), DueBucket::Lt7);
    }

    #[test]
    fn lt14_bucket_covers_eight_to_fourteen() {
        assert_eq!(from_days(8), DueBucket::Lt14);
        assert_eq!(from_days(10), DueBucket::Lt14);
        assert_eq!(from_days(14), DueBucket::Lt14);
    }

    #[test]
    fn lt30_bucket_covers_fifteen_to_thirty() {
        assert_eq!(from_days(15), DueBucket::Lt30);
        assert_eq!(from_days(20), DueBucket::Lt30);
        assert_eq!(from_days(30), DueBucket::Lt30);
    }

    #[test]
    fn gt30_bucket_for_thirty_one_plus() {
        assert_eq!(from_days(31), DueBucket::Gt30);
        assert_eq!(from_days(60), DueBucket::Gt30);
        assert_eq!(from_days(365), DueBucket::Gt30);
    }

    #[test]
    fn bucket_function_uses_today_minus_due_date() {
        let today = date(2026, 10, 12);
        // due = today+0 → Lt3
        assert_eq!(bucket(today, today), DueBucket::Lt3);
        // due = today+3 → Lt3
        assert_eq!(bucket(today, date(2026, 10, 15)), DueBucket::Lt3);
        // due = today+4 → Lt7
        assert_eq!(bucket(today, date(2026, 10, 16)), DueBucket::Lt7);
        // due = today-1 → Overdue
        assert_eq!(bucket(today, date(2026, 10, 11)), DueBucket::Overdue);
        // due = today+31 → Gt30
        assert_eq!(bucket(today, date(2026, 11, 12)), DueBucket::Gt30);
    }

    #[test]
    fn due_in_days_helper_matches_subtraction() {
        let today = date(2026, 10, 12);
        let due = date(2026, 11, 11);
        assert_eq!(due_in_days(today, due), 30);
        assert_eq!(due_in_days(due, today), -30);
    }
}