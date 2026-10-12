//! 启动时 DDL 迁移 — 幂等保护。
//!
//! 入口：`migrate(&Connection) -> Result<(), StoreError>`
//!
//! 设计：
//! - 按 `schema::ALL_DDL` 的顺序逐条执行（先表后索引）。
//! - 全部 DDL 自身已含 `IF NOT EXISTS`，故二次 migrate 是 no-op，不会报错。
//! - 任一语句出错立即返回 `StoreError::Migrate(err)`，不静默吞掉、不自动回退。
//!
//! 源产品主路径同构：源 `crates/db_connection/src/lib.rs` 的
//! `PersistenceSeed::run_migrations` — 本任务把 DDL 列表 + 顺序执行 + 幂等守护
//! 三件套搬到 Rust，行为一致。

use rusqlite::Connection;

use super::error::StoreError;
use super::schema::ALL_DDL;

/// 启动时 DDL 迁移。
///
/// 在 `open()` 之后、`repo` 任何 CRUD 之前调用；二次运行应 no-op。
///
/// 参数：
/// - `conn` — `open()` 返回的 rusqlite 连接（共享所有权；不消费）
///
/// 返回：
/// - `Ok(())` — 所有 DDL 执行成功
/// - `Err(StoreError::Migrate(_))` — 任一 DDL 失败立即报错
///
/// 不变量：
/// - 同一连接上多次调用 `migrate` 等价于单次（DDL 全部 `IF NOT EXISTS`）。
/// - 不在事务内包裹（DDL 在 SQLite 下不能放进事务；显式逐条提交）。
pub fn migrate(conn: &Connection) -> Result<(), StoreError> {
    for ddl in ALL_DDL {
        conn.execute(ddl, [])
            .map_err(|e| StoreError::Migrate(format!("DDL 执行失败：{e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn migrate_creates_all_tables_and_index_on_empty_db() {
        let conn = Connection::open_in_memory().expect("in-memory conn");
        migrate(&conn).expect("首次 migrate 应成功");

        // 验证 `resolution_event` 表存在
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='resolution_event'",
                [],
                |row| row.get(0),
            )
            .expect("查 resolution_event 表");
        assert_eq!(count, 1, "resolution_event 表应存在");

        // 验证 `week_idempotency` 表存在
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='week_idempotency'",
                [],
                |row| row.get(0),
            )
            .expect("查 week_idempotency 表");
        assert_eq!(count, 1, "week_idempotency 表应存在");

        // 验证 `idx_owner_bucket` 索引存在
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_owner_bucket'",
                [],
                |row| row.get(0),
            )
            .expect("查 idx_owner_bucket 索引");
        assert_eq!(count, 1, "idx_owner_bucket 索引应存在");
    }

    #[test]
    fn migrate_is_idempotent_second_run_is_noop() {
        let conn = Connection::open_in_memory().expect("in-memory conn");
        migrate(&conn).expect("首次 migrate 应成功");
        // 二次 migrate 应同样成功（IF NOT EXISTS 守护）
        migrate(&conn).expect("二次 migrate 应 no-op 成功");
        migrate(&conn).expect("三次 migrate 应 no-op 成功");
    }

    #[test]
    fn migrate_preserves_existing_rows() {
        let conn = Connection::open_in_memory().expect("in-memory conn");
        migrate(&conn).expect("首次 migrate");

        // 写入一行
        conn.execute(
            "INSERT INTO resolution_event \
             (week_id, resolution_id, action, actor, owner_department, due_at, due_bucket, status, evidence_ref) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                "2026-W41",
                "#[W41]01",
                "started",
                "E10086",
                "医务科",
                "2026-10-31",
                "lt14",
                "in_progress",
                None::<String>,
            ],
        )
        .expect("首次插入应成功");

        // 二次 migrate 不应丢失数据
        migrate(&conn).expect("二次 migrate");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM resolution_event", [], |row| row.get(0))
            .expect("计数");
        assert_eq!(count, 1, "migrate 不应清空既有数据");
    }
}
