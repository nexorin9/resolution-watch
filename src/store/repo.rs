//! 决议推进表 CRUD + 周次幂等键层。
//!
//! ## 核心 API
//!
//! - `upsert_idempotent` — 按 `(week_id, resolution_id)` 主键查重；命中返回 `UpsertOutcome::Existed` 不写回
//! - `get_by_week`       — 拉取该周所有决议推进事件
//! - `count_by_bucket`   — 按 `DueBucket` 枚举键聚合计数（6 桶全景）
//! - `put_idempotency_key` / `get_idempotency_key` — 周次幂等键的写 / 读
//!                         （避免不同周决议条目误命中；二次跑批校验一致性）
//!
//! ## 与源同构
//!
//! 源 `crates/model/src/deployment_audit_log/mod.rs` 的 `AUDIT_LOG_INDEX_BY_ACTION` 二级索引
//! — 本模块 `upsert_idempotent` 按 `(week_id, resolution_id)` 主键查重 + `count_by_bucket`
//! 按二级索引聚合，复刻同一查询路径。

use std::collections::HashMap;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};

use crate::schemas::{DueBucket, OaAction, ResolutionStatus};

use super::error::StoreError;

/// upsert 产出（区分新建 vs 既存）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// 决议条目首次写入。
    Inserted,
    /// 已存在同 `(week_id, resolution_id)`，不写回。
    Existed,
}

/// 单条决议推进事件（从 store 读出的形式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionEvent {
    pub week_id: String,
    pub resolution_id: String,
    pub action: OaAction,
    pub actor: String,
    pub owner_department: String,
    pub due_at: NaiveDate,
    pub due_bucket: DueBucket,
    pub status: ResolutionStatus,
    pub evidence_ref: Option<String>,
}

/// upsert_idempotent — 按 `(week_id, resolution_id)` 主键查重。
///
/// 命中 → 返回 `UpsertOutcome::Existed`（不覆盖既有）；
/// 未命中 → INSERT 并返回 `UpsertOutcome::Inserted`。
pub fn upsert_idempotent(
    conn: &Connection,
    week_id: &str,
    resolution_id: &str,
    action: OaAction,
    actor: &str,
    owner_department: &str,
    due_at: NaiveDate,
    due_bucket: DueBucket,
    status: ResolutionStatus,
    evidence_ref: Option<&str>,
) -> Result<UpsertOutcome, StoreError> {
    // 先查重
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM resolution_event WHERE week_id = ?1 AND resolution_id = ?2",
            params![week_id, resolution_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StoreError::Migrate(format!("查重失败：{e}")))?;

    if exists.is_some() {
        return Ok(UpsertOutcome::Existed);
    }

    conn.execute(
        "INSERT INTO resolution_event
            (week_id, resolution_id, action, actor, owner_department, due_at, due_bucket, status, evidence_ref)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            week_id,
            resolution_id,
            action.as_str(),
            actor,
            owner_department,
            due_at.format("%Y-%m-%d").to_string(),
            due_bucket.as_str(),
            status.as_str(),
            evidence_ref,
        ],
    )
    .map_err(|e| StoreError::Migrate(format!("insert 失败：{e}")))?;

    Ok(UpsertOutcome::Inserted)
}

/// 拉取指定周次所有决议推进事件（按行 6 必填 + 3 衍生形式读出）。
pub fn get_by_week(conn: &Connection, week_id: &str) -> Result<Vec<ResolutionEvent>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT week_id, resolution_id, action, actor, owner_department, due_at, due_bucket, status, evidence_ref
             FROM resolution_event
             WHERE week_id = ?1
             ORDER BY resolution_id",
        )
        .map_err(|e| StoreError::Migrate(format!("prepare 失败：{e}")))?;

    let rows = stmt
        .query_map(params![week_id], |row| {
            let action_str: String = row.get(2)?;
            let due_at_str: String = row.get(5)?;
            let due_bucket_str: String = row.get(6)?;
            let status_str: String = row.get(7)?;
            Ok(ResolutionEvent {
                week_id: row.get(0)?,
                resolution_id: row.get(1)?,
                action: parse_action(&action_str)?,
                actor: row.get(3)?,
                owner_department: row.get(4)?,
                due_at: NaiveDate::parse_from_str(&due_at_str, "%Y-%m-%d")
                    .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
                due_bucket: parse_bucket(&due_bucket_str)?,
                status: parse_status(&status_str)?,
                evidence_ref: row.get(8)?,
            })
        })
        .map_err(|e| StoreError::Migrate(format!("query_map 失败：{e}")))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| StoreError::Migrate(format!("row 解析失败：{e}")))?);
    }
    Ok(out)
}

/// 6 桶计数（key = `DueBucket` 枚举；6 桶未出现即 HashMap 中无该项）。
///
/// 返回 `HashMap<DueBucket, usize>` 而不是字符串字典：调用方传 `DueBucket::Lt30`
/// 直接作为 key 查计数，不必再做字符串转换。空周次返回空 HashMap。
pub fn count_by_bucket(
    conn: &Connection,
    week_id: &str,
) -> Result<HashMap<DueBucket, usize>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT due_bucket, COUNT(*) FROM resolution_event
             WHERE week_id = ?1
             GROUP BY due_bucket",
        )
        .map_err(|e| StoreError::Migrate(format!("prepare 失败：{e}")))?;

    let rows = stmt
        .query_map(params![week_id], |row| {
            let bucket_str: String = row.get(0)?;
            let v: i64 = row.get(1)?;
            // bucket_str 来自物化列 due_bucket；
            // 异常值（DB 里的字符串未匹配任一 DueBucket）→ 走 StoreError::Migrate。
            let bucket = match bucket_str.as_str() {
                "lt3" => DueBucket::Lt3,
                "lt7" => DueBucket::Lt7,
                "lt14" => DueBucket::Lt14,
                "lt30" => DueBucket::Lt30,
                "gt30" => DueBucket::Gt30,
                "overdue" => DueBucket::Overdue,
                other => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("未知 due_bucket 值：{other}"),
                        )),
                    ));
                }
            };
            Ok((bucket, v as usize))
        })
        .map_err(|e| StoreError::Migrate(format!("query_map 失败：{e}")))?;

    let mut out: HashMap<DueBucket, usize> = HashMap::new();
    for r in rows {
        let (k, v) = r.map_err(|e| StoreError::Migrate(format!("row 解析失败：{e}")))?;
        out.insert(k, v);
    }
    Ok(out)
}

/// 写入周次幂等键（决议条目 hash，便于二次跑批校验一致）。
pub fn put_idempotency_key(
    conn: &Connection,
    week_id: &str,
    hash: &str,
) -> Result<(), StoreError> {
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    conn.execute(
        "INSERT OR REPLACE INTO week_idempotency (week_id, hash, created_at)
         VALUES (?1, ?2, ?3)",
        params![week_id, hash, now],
    )
    .map_err(|e| StoreError::Migrate(format!("put_idempotency_key 失败：{e}")))?;
    Ok(())
}

/// 读取周次幂等键；不存在返回 None。
pub fn get_idempotency_key(conn: &Connection, week_id: &str) -> Result<Option<String>, StoreError> {
    let hash: Option<String> = conn
        .query_row(
            "SELECT hash FROM week_idempotency WHERE week_id = ?1",
            params![week_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| StoreError::Migrate(format!("get_idempotency_key 失败：{e}")))?;
    Ok(hash)
}

// ---------- 内部：枚举字符串 ↔ enum ----------

fn parse_action(s: &str) -> rusqlite::Result<OaAction> {
    match s {
        "started" => Ok(OaAction::Started),
        "in_progress" => Ok(OaAction::InProgress),
        "blocked" => Ok(OaAction::Blocked),
        "submitted" => Ok(OaAction::Submitted),
        "completed" => Ok(OaAction::Completed),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("未知 OaAction：{other}"),
            )),
        )),
    }
}

fn parse_bucket(s: &str) -> rusqlite::Result<DueBucket> {
    match s {
        "lt3" => Ok(DueBucket::Lt3),
        "lt7" => Ok(DueBucket::Lt7),
        "lt14" => Ok(DueBucket::Lt14),
        "lt30" => Ok(DueBucket::Lt30),
        "gt30" => Ok(DueBucket::Gt30),
        "overdue" => Ok(DueBucket::Overdue),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("未知 DueBucket：{other}"),
            )),
        )),
    }
}

fn parse_status(s: &str) -> rusqlite::Result<ResolutionStatus> {
    match s {
        "pending" => Ok(ResolutionStatus::Pending),
        "in_progress" => Ok(ResolutionStatus::InProgress),
        "due_soon" => Ok(ResolutionStatus::DueSoon),
        "overdue" => Ok(ResolutionStatus::Overdue),
        "completed" => Ok(ResolutionStatus::Completed),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("未知 ResolutionStatus：{other}"),
            )),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::open;

    fn temp_db() -> Connection {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("test.sqlite");
        // 避免 TempDir drop → file still open
        let p_str = p.to_str().unwrap().to_string();
        std::mem::forget(tmp);
        open(Some(&p_str)).unwrap()
    }

    #[test]
    fn upsert_first_time_inserts() {
        let conn = temp_db();
        let out = upsert_idempotent(
            &conn,
            "2026-W41",
            "#[W41]01",
            OaAction::Started,
            "E10086",
            "医务科",
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            DueBucket::Lt30,
            ResolutionStatus::InProgress,
            Some("OA-001"),
        )
        .unwrap();
        assert_eq!(out, UpsertOutcome::Inserted);
    }

    #[test]
    fn upsert_second_time_returns_existed() {
        let conn = temp_db();
        upsert_idempotent(
            &conn,
            "2026-W41",
            "#[W41]01",
            OaAction::Started,
            "E10086",
            "医务科",
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            DueBucket::Lt30,
            ResolutionStatus::InProgress,
            Some("OA-001"),
        )
        .unwrap();
        let out2 = upsert_idempotent(
            &conn,
            "2026-W41",
            "#[W41]01",
            OaAction::Submitted,
            "E10086",
            "医务科",
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            DueBucket::Lt30,
            ResolutionStatus::DueSoon,
            Some("OA-002"),
        )
        .unwrap();
        assert_eq!(out2, UpsertOutcome::Existed);
    }

    #[test]
    fn get_by_week_returns_inserted_events() {
        let conn = temp_db();
        upsert_idempotent(
            &conn,
            "2026-W41",
            "#[W41]01",
            OaAction::Started,
            "E10086",
            "医务科",
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            DueBucket::Lt30,
            ResolutionStatus::InProgress,
            Some("OA-001"),
        )
        .unwrap();
        upsert_idempotent(
            &conn,
            "2026-W41",
            "#[W41]02",
            OaAction::Started,
            "E20002",
            "信息科",
            NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
            DueBucket::Gt30,
            ResolutionStatus::InProgress,
            None,
        )
        .unwrap();
        let events = get_by_week(&conn, "2026-W41").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].resolution_id, "#[W41]01");
        assert_eq!(events[1].resolution_id, "#[W41]02");
    }

    #[test]
    fn count_by_bucket_groups_by_due_bucket() {
        let conn = temp_db();
        for (rid, bucket) in [
            ("#[W41]01", DueBucket::Lt3),
            ("#[W41]02", DueBucket::Lt3),
            ("#[W41]03", DueBucket::Lt7),
            ("#[W41]04", DueBucket::Overdue),
        ] {
            upsert_idempotent(
                &conn,
                "2026-W41",
                rid,
                OaAction::Started,
                "E1",
                "医务科",
                NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
                bucket,
                ResolutionStatus::InProgress,
                None,
            )
            .unwrap();
        }
        let counts = count_by_bucket(&conn, "2026-W41").unwrap();
        assert_eq!(counts.get(&DueBucket::Lt3).copied(), Some(2));
        assert_eq!(counts.get(&DueBucket::Lt7).copied(), Some(1));
        assert_eq!(counts.get(&DueBucket::Overdue).copied(), Some(1));
        assert_eq!(counts.len(), 3);
    }

    #[test]
    fn idempotency_key_roundtrip() {
        let conn = temp_db();
        assert!(get_idempotency_key(&conn, "2026-W41").unwrap().is_none());
        put_idempotency_key(&conn, "2026-W41", "abc123").unwrap();
        let got = get_idempotency_key(&conn, "2026-W41").unwrap();
        assert_eq!(got.as_deref(), Some("abc123"));
    }

    #[test]
    fn upsert_cross_week_does_not_collide() {
        let conn = temp_db();
        let o1 = upsert_idempotent(
            &conn,
            "2026-W41",
            "#[W41]01",
            OaAction::Started,
            "E1",
            "医务科",
            NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            DueBucket::Lt30,
            ResolutionStatus::InProgress,
            None,
        )
        .unwrap();
        let o2 = upsert_idempotent(
            &conn,
            "2026-W42",
            "#[W42]01",
            OaAction::Started,
            "E1",
            "医务科",
            NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
            DueBucket::Gt30,
            ResolutionStatus::InProgress,
            None,
        )
        .unwrap();
        assert_eq!(o1, UpsertOutcome::Inserted);
        assert_eq!(o2, UpsertOutcome::Inserted);
    }
}