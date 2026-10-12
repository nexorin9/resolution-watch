//! Sqlite DDL — 决议推进表 + 周次幂等键 + 二级索引。
//!
//! 三件套：
//! 1. `resolution_event` — 单决议推进事件表（每周 × 决议编号唯一）
//! 2. `week_idempotency` — 周次幂等键（防跨周决议条目互相误命中）
//! 3. `idx_owner_bucket`  — 二级索引（owner_department × due_bucket，源
//!    `by_action_and_creation_time` 二级索引同构思路的本地化复刻）
//!
//! 设计要点：
//! - 所有时间字段以 ISO 8601 TEXT 存储（`YYYY-MM-DD` 或 `YYYY-MM-DDTHH:MM:SS+08:00`），
//!   不走 SQLite 内部 datetime 类型，便于跨机器/跨时区可读、可 grep。
//! - 枚举字段（action / status / due_bucket）以 snake_case TEXT 存储，与
//!   `schemas::` 层 serde 派生一致，避免魔法数字。
//! - 复合主键 `(week_id, resolution_id)` 保证单周内决议编号唯一，跨周不互命中。
//! - `owner_department` 与 `due_bucket` 在 `resolution_event` 内**物化**（即使它们可派生），
//!   支撑 `idx_owner_bucket` 二级索引的快速扫描；后续 `repo.rs`（task 26）的 `upsert`
//!   接受这两个参数显式写入。
//!
//! 源产品主路径同构：源 `crates/model/src/deployment_audit_log/mod.rs` 的
//! `AUDIT_LOG_INDEX_BY_ACTION` 二级索引 — 本模块以 `idx_owner_bucket` 复刻同一思路：
//! 主表按业务主键 `(week_id, resolution_id)` 排布，二级索引加速「按科室 × 按桶」
//! 的查询路径。

/// 决议推进事件表 DDL（幂等：IF NOT EXISTS）。
///
/// 列定义：
/// - `week_id`         — ISO 周编号（`2026-W41`）
/// - `resolution_id`   — 周内唯一决议编号（`#[W41]01`）
/// - `action`          — OA 流转动作（`started` / `in_progress` / `blocked` / `submitted` / `completed`）
/// - `actor`           — 触发人 / 工号
/// - `owner_department`— 责任科室（命中 `config/departments.yaml` 字典），物化用于二级索引
/// - `due_at`          — 截止日期 `YYYY-MM-DD`（物化便于导出包直读）
/// - `due_bucket`      — 6 桶之一（`lt3` / `lt7` / `lt14` / `lt30` / `gt30` / `overdue`），物化用于二级索引
/// - `status`          — 决议状态机 5 态（`pending` / `in_progress` / `due_soon` / `overdue` / `completed`）
/// - `evidence_ref`    — 触发该推进事件的 OA 流转 id（可空）
pub const CREATE_RESOLUTION_EVENT: &str = r#"
CREATE TABLE IF NOT EXISTS resolution_event (
    week_id          TEXT NOT NULL,
    resolution_id    TEXT NOT NULL,
    action           TEXT NOT NULL,
    actor            TEXT NOT NULL,
    owner_department TEXT NOT NULL,
    due_at           TEXT NOT NULL,
    due_bucket       TEXT NOT NULL,
    status           TEXT NOT NULL,
    evidence_ref     TEXT,
    PRIMARY KEY (week_id, resolution_id)
);
"#;

/// 周次幂等键表 DDL（幂等：IF NOT EXISTS）。
///
/// 同一周次的决议条目经 LLM 抽取 / deterministic 解析后写入此处一个 hash；
/// 二次跑同一周时按此 hash 校验决议条目是否一致，不一致时报错由人工复核。
pub const CREATE_WEEK_IDEMPOTENCY: &str = r#"
CREATE TABLE IF NOT EXISTS week_idempotency (
    week_id    TEXT PRIMARY KEY,
    hash       TEXT NOT NULL,
    created_at TEXT NOT NULL
);
"#;

/// 二级索引 DDL：按责任科室 × 6 桶快速定位决议条目。
///
/// 源产品主路径同构：源 `crates/model/src/deployment_audit_log/mod.rs` 的
/// `AUDIT_LOG_INDEX_BY_ACTION` 二级索引思路 — 主表 + 二级索引，本任务以
/// `idx_owner_bucket (owner_department, due_bucket)` 复刻同一查询路径。
pub const CREATE_IDX_OWNER_BUCKET: &str = r#"
CREATE INDEX IF NOT EXISTS idx_owner_bucket
    ON resolution_event (owner_department, due_bucket);
"#;

/// 所有 DDL 的有序列表（按依赖顺序：先表后索引）。
///
/// `migrate()` 按该顺序逐条执行，任一失败即报错（`StoreError::Migrate`）。
pub const ALL_DDL: &[&str] = &[
    CREATE_RESOLUTION_EVENT,
    CREATE_WEEK_IDEMPOTENCY,
    CREATE_IDX_OWNER_BUCKET,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddl_in_order_table_before_index() {
        // 顺序：先表后索引；若索引创建时表不存在会出错
        assert!(ALL_DDL[0].contains("CREATE TABLE IF NOT EXISTS resolution_event"));
        assert!(ALL_DDL[1].contains("CREATE TABLE IF NOT EXISTS week_idempotency"));
        assert!(ALL_DDL[2].contains("CREATE INDEX IF NOT EXISTS idx_owner_bucket"));
        assert!(ALL_DDL[2].contains("ON resolution_event"));
    }

    #[test]
    fn resolution_event_columns_match_schema() {
        // 不变量：`resolution_event` 列必须覆盖 `repo::upsert_idempotent` 的参数
        // （task 26 实现：week_id / resolution_id / action / actor / due_at / status / evidence_ref
        //  + 物化列 owner_department / due_bucket）
        for col in [
            "week_id",
            "resolution_id",
            "action",
            "actor",
            "owner_department",
            "due_at",
            "due_bucket",
            "status",
            "evidence_ref",
        ] {
            assert!(
                CREATE_RESOLUTION_EVENT.contains(col),
                "DDL 缺列 {col}"
            );
        }
    }

    #[test]
    fn week_idempotency_columns_match_spec() {
        for col in ["week_id", "hash", "created_at"] {
            assert!(
                CREATE_WEEK_IDEMPOTENCY.contains(col),
                "DDL 缺列 {col}"
            );
        }
    }

    #[test]
    fn composite_primary_key_is_week_and_resolution() {
        // 不变量：主键 = (week_id, resolution_id)，单周决议编号唯一
        assert!(CREATE_RESOLUTION_EVENT.contains("PRIMARY KEY (week_id, resolution_id)"));
    }

    #[test]
    fn all_ddl_is_idempotent() {
        // 不变量：所有 DDL 须以 `IF NOT EXISTS` 守护，二次 migrate 不报错
        for ddl in ALL_DDL {
            assert!(ddl.contains("IF NOT EXISTS"), "DDL 缺 IF NOT EXISTS 守护：{ddl}");
        }
    }
}
