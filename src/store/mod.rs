//! Sqlite 决议推进表 + 周次幂等键层。
//!
//! 主路径（按 `spec.md` 工作闭环）：
//! 1. CLI 启动调用 `open(db_path)`，从 env `RESO_DB_PATH` 读路径（默认 `"./.reso_cache.sqlite"`）
//! 2. `migrate(&conn)` 跑 DDL（idempotent — `IF NOT EXISTS`）
//! 3. 后续 `repo`（task 26）做 `upsert_idempotent` / `get_by_week` / `count_by_bucket` /
//!    `put/get_idempotency_key`
//! 4. 本轮导出包（task 29）从 store 读决议条目 + 周次幂等键，写入
//!    `out/<week_id>/resolutions.csv` 等 6 文件
//!
//! 模块组成：
//! - `schema.rs`    — DDL 常量 + 顺序列表（`resolution_event` + `week_idempotency` + `idx_owner_bucket`）
//! - `migrate.rs`   — `migrate(&Connection)` 启动时跑 DDL
//! - `error.rs`     — `StoreError`（Open / Migrate / Config 三类）
//! - `tests.rs`     — store 层单测（migrate 幂等 + 表/索引存在性 + 列覆盖）
//!
//! 后续 task 26–28 续接 `repo.rs`（CRUD）：`upsert_idempotent` + `get_by_week` +
//! `count_by_bucket` + `put/get_idempotency_key`。
//!
//! 关键不变量（每条都在单测中守护）：
//! - 复合主键 `(week_id, resolution_id)` 保证单周决议编号唯一、跨周不互命中。
//! - DDL 全部 `IF NOT EXISTS`，二次 `migrate` 是 no-op。
//! - 物化列 `owner_department` / `due_bucket` 支撑 `idx_owner_bucket` 二级索引。
//!
//! 源产品主路径同构：源 `crates/model/src/deployment_audit_log/mod.rs` 的
//! `AUDIT_LOG_INDEX_BY_ACTION` 二级索引 + 主表复合主键 — 本模块以
//! `idx_owner_bucket (owner_department, due_bucket)` 与 `(week_id, resolution_id)`
//! 复刻同一种"主表 + 二级索引"的查询路径。

pub mod error;
pub mod migrate;
pub mod repo;
pub mod schema;

#[cfg(test)]
mod tests;

pub use error::StoreError;
pub use migrate::migrate;
pub use repo::{
    count_by_bucket, get_by_week, get_idempotency_key, put_idempotency_key, upsert_idempotent,
    ResolutionEvent, UpsertOutcome,
};
pub use schema::{ALL_DDL, CREATE_IDX_OWNER_BUCKET, CREATE_RESOLUTION_EVENT, CREATE_WEEK_IDEMPOTENCY};

/// env 变量名 — Sqlite 文件路径。
///
/// 默认 `./.reso_cache.sqlite`（本轮决议督办本地缓存，不入 HIS/EMR）。
pub const ENV_DB_PATH: &str = "RESO_DB_PATH";

/// Sqlite 默认路径（env 缺失时使用）。
pub const DEFAULT_DB_PATH: &str = "./.reso_cache.sqlite";

/// 打开 Sqlite 连接 + 跑 migrate。
///
/// 参数：
/// - `db_path` — 显式传入路径；传 `None` 时按 `ENV_DB_PATH` env 读取；
///   env 缺失时回落 `DEFAULT_DB_PATH`
///
/// 返回：
/// - `Ok(Connection)` — 连接已建立 + DDL 已 migrate 完成
/// - `Err(StoreError::Open(_))` — 路径不可写 / 权限拒绝 / 磁盘满
/// - `Err(StoreError::Migrate(_))` — DDL 执行失败
///
/// 不变量：
/// - 单次调用 `open()` 等价于「打开 + migrate」原子操作；调用方拿到的连接
///   可直接做后续 CRUD，无需再调 `migrate()`。
/// - 本任务（task 6）仅覆盖 DDL + migrate；CRUD 在 `repo.rs`（task 26+）。
pub fn open(db_path: Option<&str>) -> Result<rusqlite::Connection, StoreError> {
    let path = match db_path {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => match std::env::var(ENV_DB_PATH) {
            Ok(p) if !p.trim().is_empty() => p,
            Ok(_) | Err(_) => DEFAULT_DB_PATH.to_string(),
        },
    };

    let conn = rusqlite::Connection::open(&path)
        .map_err(|e| StoreError::Open(format!("path={path} err={e}")))?;

    migrate(&conn)?;
    Ok(conn)
}
