//! store 子系统集成测试（task 28）。
//!
//! 覆盖 spec 必答的 8 项 + 3 项额外补强：
//!
//! ## spec 必答
//! 1. **migrate 幂等**：从空库走到 migrate + 二次 migrate + 多次 migrate，DDL 全部 `IF NOT EXISTS` 守护
//! 2. **upsert_idempotent 同周二次不覆盖**：同 `(week_id, resolution_id)` 第二次写入返回 `UpsertOutcome::Existed`
//! 3. **跨周不互命中**：同 `resolution_id` 但不同 `week_id` 各自返回 `UpsertOutcome::Inserted`
//! 4. **6 桶 count 准确**：6 桶各自插 ≥1 条决议后 `count_by_bucket` 返回 HashMap 6 桶全景
//! 5. **周次幂等键读写**：PUT/GET 命中 / 二次 PUT 覆盖 / 跨周次互不命中
//! 6. **凭据缺失返回错误**：`open` 路径不可写时返 `StoreError::Open`
//! 7. **决议编号冲突处理**：同 `(week_id, resolution_id)` 二次 upsert 不静默、不覆盖、不 panic；
//!    业务层可据此判定「该决议条目已被记录」
//! 8. **env `RESO_DB_PATH` 默认值生效**：env 缺失时回落 `DEFAULT_DB_PATH`，不报错
//!
//! ## 额外补强
//! 9. **完整管线**：`open` → `migrate` → `upsert` → `get_by_week` → `count_by_bucket` → `put_idempotency_key`
//!    → `get_idempotency_key` 全链路跑通
//! 10. **跨 open 数据持久化**：连接 1 写入数据 → drop → 连接 2 重新 open 同一路径 → 数据可读
//! 11. **`idx_owner_bucket` 二级索引查询**：跨科室 + 跨桶维度命中

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::NaiveDate;
use rusqlite::Connection;
use tempfile::TempDir;

use resolution_watch::schemas::{DueBucket, OaAction, ResolutionStatus};
use resolution_watch::store::{
    count_by_bucket, get_by_week, get_idempotency_key, migrate, open, put_idempotency_key,
    upsert_idempotent, ENV_DB_PATH,
};

// 全进程级 env 互斥锁：env vars 是进程级共享，
// 并行 cargo test 会互相污染（oa_pull/store/cli 都共享同一锁约定）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 临时 Sqlite 文件路径助手（每个测试独立隔离，避免 TempDir drop 顺序问题）。
struct TempSqlite {
    _dir: TempDir,
    path: String,
}

impl TempSqlite {
    fn new(label: &str) -> Self {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join(format!("{label}.sqlite"));
        let path_str = path.to_str().unwrap().to_string();
        Self {
            _dir: dir,
            path: path_str,
        }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

/// 在指定 conn 上插入一条决议（spec 必答 case 4 / 7 等共用）。
fn insert_resolution(
    conn: &Connection,
    week_id: &str,
    resolution_id: &str,
    bucket: DueBucket,
    due_year: i32,
    due_month: u32,
    due_day: u32,
    department: &str,
) {
    upsert_idempotent(
        conn,
        week_id,
        resolution_id,
        OaAction::Started,
        "E10086",
        department,
        NaiveDate::from_ymd_opt(due_year, due_month, due_day).unwrap(),
        bucket,
        ResolutionStatus::InProgress,
        None,
    )
    .expect("upsert_idempotent 写决议条目");
}

// ===== Test 1: migrate 幂等（多次调用 no-op） =====

#[test]
fn store_integration_migrate_idempotent_end_to_end() {
    let tmp = TempSqlite::new("migrate_idempotent");

    // 第一次 open + migrate → 建表
    let conn = open(Some(tmp.path())).expect("open 应成功");
    drop(conn); // 关闭连接，避免后续 open 报「database is locked」

    // 重新 open 同一路径 + 二次 migrate
    let conn2 = open(Some(tmp.path())).expect("二次 open 应成功");
    migrate(&conn2).expect("二次 migrate 应 no-op");
    migrate(&conn2).expect("三次 migrate 应 no-op");

    // 验证表与索引各只一份
    let count: i64 = conn2
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='resolution_event'",
            [],
            |row| row.get(0),
        )
        .expect("查表");
    assert_eq!(count, 1, "重复 migrate 不应创建多份 resolution_event 表");

    let count: i64 = conn2
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='week_idempotency'",
            [],
            |row| row.get(0),
        )
        .expect("查表");
    assert_eq!(count, 1, "重复 migrate 不应创建多份 week_idempotency 表");

    let count: i64 = conn2
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_owner_bucket'",
            [],
            |row| row.get(0),
        )
        .expect("查索引");
    assert_eq!(count, 1, "重复 migrate 不应创建多份 idx_owner_bucket 索引");
}

// ===== Test 2: upsert_idempotent 同周二次不覆盖 =====

#[test]
fn store_integration_upsert_idempotent_same_week_second_time_returns_existed() {
    let tmp = TempSqlite::new("upsert_same_week");
    let conn = open(Some(tmp.path())).expect("open 应成功");

    // 第一次 upsert：新建
    let out1 = upsert_idempotent(
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
    .expect("首次 upsert");
    assert_eq!(out1, resolution_watch::store::UpsertOutcome::Inserted);

    // 第二次 upsert：相同主键但内容完全不同的字段
    // （即使 actor/due_at/due_bucket/status/evidence_ref 都不同，复合主键命中仍应返回 Existed）
    let out2 = upsert_idempotent(
        &conn,
        "2026-W41",
        "#[W41]01",
        OaAction::Completed, // ← 故意不同 action
        "E99999",            // ← 故意不同 actor
        "护理部",             // ← 故意不同 owner_department
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(), // ← 故意不同 due_at
        DueBucket::Lt3,      // ← 故意不同 due_bucket
        ResolutionStatus::Completed, // ← 故意不同 status
        Some("OA-002"),      // ← 故意不同 evidence_ref
    )
    .expect("二次 upsert（不应报错）");
    assert_eq!(out2, resolution_watch::store::UpsertOutcome::Existed);

    // 验证写入的内容仍是首次的（不覆盖）
    let events = get_by_week(&conn, "2026-W41").expect("get_by_week");
    assert_eq!(events.len(), 1, "应仅有 1 条决议（不覆盖）");
    assert_eq!(events[0].actor, "E10086", "actor 应保留首次值");
    assert_eq!(events[0].action, OaAction::Started, "action 应保留首次值");
    assert_eq!(events[0].owner_department, "医务科", "owner_department 应保留首次值");
    assert_eq!(
        events[0].evidence_ref.as_deref(),
        Some("OA-001"),
        "evidence_ref 应保留首次值"
    );
}

// ===== Test 3: 跨周不互命中 =====

#[test]
fn store_integration_cross_week_does_not_collide() {
    let tmp = TempSqlite::new("cross_week");
    let conn = open(Some(tmp.path())).expect("open 应成功");

    // 同 resolution_id 不同 week_id 各自 Inserted
    let out_w41 = upsert_idempotent(
        &conn,
        "2026-W41",
        "决议01",
        OaAction::Started,
        "E10086",
        "医务科",
        NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
        DueBucket::Lt30,
        ResolutionStatus::InProgress,
        None,
    )
    .expect("W41 upsert");
    let out_w42 = upsert_idempotent(
        &conn,
        "2026-W42",
        "决议01",
        OaAction::Started,
        "E10086",
        "医务科",
        NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(),
        DueBucket::Gt30,
        ResolutionStatus::InProgress,
        None,
    )
    .expect("W42 upsert");

    assert_eq!(out_w41, resolution_watch::store::UpsertOutcome::Inserted);
    assert_eq!(out_w42, resolution_watch::store::UpsertOutcome::Inserted);

    // get_by_week 各拉各的
    let w41_events = get_by_week(&conn, "2026-W41").expect("W41 get");
    let w42_events = get_by_week(&conn, "2026-W42").expect("W42 get");
    assert_eq!(w41_events.len(), 1);
    assert_eq!(w42_events.len(), 1);
    assert_eq!(w41_events[0].week_id, "2026-W41");
    assert_eq!(w42_events[0].week_id, "2026-W42");
    assert_eq!(w41_events[0].due_bucket, DueBucket::Lt30);
    assert_eq!(w42_events[0].due_bucket, DueBucket::Gt30);
}

// ===== Test 4: 6 桶 count 准确 =====

#[test]
fn store_integration_six_bucket_count_accurate_at_integration_level() {
    let tmp = TempSqlite::new("six_bucket_count");
    let conn = open(Some(tmp.path())).expect("open 应成功");

    // 6 桶各插 ≥1 条决议（Lt3 插 2 条以验证累加语义）
    let plan = [
        ("决议01", DueBucket::Lt3, 2026, 10, 31, "医务科"),
        ("决议02", DueBucket::Lt3, 2026, 10, 31, "医务科"),
        ("决议03", DueBucket::Lt7, 2026, 11, 5, "信息科"),
        ("决议04", DueBucket::Lt14, 2026, 11, 10, "设备科"),
        ("决议05", DueBucket::Lt30, 2026, 11, 25, "医保办"),
        ("决议06", DueBucket::Gt30, 2026, 12, 31, "财务科"),
        ("决议07", DueBucket::Overdue, 2026, 10, 1, "后勤"),
    ];
    for (rid, b, y, m, d, dept) in plan {
        insert_resolution(&conn, "2026-W41", rid, b, y, m, d, dept);
    }

    let counts = count_by_bucket(&conn, "2026-W41").expect("count_by_bucket 6 桶");

    // 6 桶 key 全数到齐
    let mut expected: HashMap<DueBucket, usize> = HashMap::new();
    expected.insert(DueBucket::Lt3, 2);
    expected.insert(DueBucket::Lt7, 1);
    expected.insert(DueBucket::Lt14, 1);
    expected.insert(DueBucket::Lt30, 1);
    expected.insert(DueBucket::Gt30, 1);
    expected.insert(DueBucket::Overdue, 1);

    assert_eq!(counts.len(), 6, "6 桶全景 HashMap");
    for (k, v) in expected {
        assert_eq!(counts.get(&k).copied(), Some(v), "桶 {k:?} 计数");
    }
    let total: usize = counts.values().sum();
    assert_eq!(total, 7, "6 桶总和 = 7 条决议");

    // 跨周次不互命中：W42 暂无决议 → 空 HashMap
    let w42_counts = count_by_bucket(&conn, "2026-W42").expect("W42 count_by_bucket");
    assert_eq!(w42_counts.len(), 0, "W42 应为空 HashMap");
}

// ===== Test 5: 周次幂等键读写 =====

#[test]
fn store_integration_week_idempotency_key_roundtrip_and_cross_week_isolation() {
    let tmp = TempSqlite::new("week_idempotency");
    let conn = open(Some(tmp.path())).expect("open 应成功");

    // (1) 初始 None
    assert!(
        get_idempotency_key(&conn, "2026-W41")
            .expect("get 初次")
            .is_none(),
        "未写入的周次应返回 None"
    );

    // (2) 写入 → 读取对得上
    put_idempotency_key(&conn, "2026-W41", "hash-W41-first-run").expect("put W41");
    let got = get_idempotency_key(&conn, "2026-W41").expect("get W41");
    assert_eq!(got.as_deref(), Some("hash-W41-first-run"));

    // (3) 二次写入覆盖
    put_idempotency_key(&conn, "2026-W41", "hash-W41-second-run").expect("put W41 二次");
    let got2 = get_idempotency_key(&conn, "2026-W41").expect("get W41 二次");
    assert_eq!(got2.as_deref(), Some("hash-W41-second-run"), "二次写入覆盖旧 hash");

    // (4) 跨周次互不命中
    put_idempotency_key(&conn, "2026-W42", "hash-W42").expect("put W42");
    let w41_after = get_idempotency_key(&conn, "2026-W41").expect("get W41 after W42 put");
    let w42_got = get_idempotency_key(&conn, "2026-W42").expect("get W42");
    assert_eq!(
        w41_after.as_deref(),
        Some("hash-W41-second-run"),
        "W41 不被 W42 污染"
    );
    assert_eq!(w42_got.as_deref(), Some("hash-W42"), "W42 独立 hash");
}

// ===== Test 6: 凭据缺失（不可写路径）返回错误 =====

#[test]
fn store_integration_invalid_path_returns_store_error() {
    // 显式传一个不可写路径 → open 应返 Err(StoreError::Open)
    let conn = open(Some("/this/path/does/not/exist/never_writable.sqlite"));
    match conn {
        Ok(_) => panic!("不可写路径应返回 Err，实际 Ok"),
        Err(e) => {
            use resolution_watch::store::error::StoreError;
            assert!(
                matches!(e, StoreError::Open(_)),
                "不可写路径应返 StoreError::Open，实际：{e:?}"
            );
            // Display 应包含路径信息，便于定位
            let s = e.to_string();
            assert!(
                s.contains("Sqlite 打开失败") || s.contains("/this/path"),
                "Display 应包含 Sqlite 打开失败或路径信息，实际：{s}"
            );
        }
    }
}

// ===== Test 7: 决议编号冲突处理（不静默、不覆盖、不 panic） =====

#[test]
fn store_integration_duplicate_resolution_id_handled_gracefully() {
    let tmp = TempSqlite::new("duplicate_rid");
    let conn = open(Some(tmp.path())).expect("open 应成功");

    // 首次写入决议条目
    let out1 = upsert_idempotent(
        &conn,
        "2026-W41",
        "决议01",
        OaAction::Started,
        "E10086",
        "医务科",
        NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
        DueBucket::Lt30,
        ResolutionStatus::InProgress,
        Some("OA-001"),
    )
    .expect("首次 upsert");
    assert_eq!(out1, resolution_watch::store::UpsertOutcome::Inserted);

    // 二次写入：模拟同周两条决议条目同号
    // （应被复合主键 (week_id, resolution_id) 守护，二次返回 Existed）
    let out2 = upsert_idempotent(
        &conn,
        "2026-W41",
        "决议01", // ← 同 resolution_id
        OaAction::Submitted, // ← 不同 action
        "E20002",  // ← 不同 actor
        "信息科",   // ← 不同科室
        NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
        DueBucket::Lt7,
        ResolutionStatus::DueSoon,
        Some("OA-002"),
    )
    .expect("二次 upsert 同 resolution_id 应不 panic 不报错");

    // 验证：复合主键命中 → 二次返回 Existed（业务层可据此判定「该决议已被记录」）
    assert_eq!(
        out2,
        resolution_watch::store::UpsertOutcome::Existed,
        "二次 upsert 同主键应返回 Existed（冲突被显式化）"
    );

    // 验证：决议库仍只有 1 条（不覆盖）
    let events = get_by_week(&conn, "2026-W41").expect("get_by_week");
    assert_eq!(events.len(), 1, "同主键二次 upsert 不应新增条目");
    assert_eq!(
        events[0].action,
        OaAction::Started,
        "action 应保留首次值（不覆盖）"
    );
    assert_eq!(events[0].actor, "E10086", "actor 应保留首次值");
    assert_eq!(
        events[0].owner_department, "医务科",
        "owner_department 应保留首次值"
    );

    // 验证：第三次 upsert（继续同主键）仍返回 Existed，行为一致
    let out3 = upsert_idempotent(
        &conn,
        "2026-W41",
        "决议01",
        OaAction::Completed,
        "E30003",
        "设备科",
        NaiveDate::from_ymd_opt(2026, 12, 1).unwrap(),
        DueBucket::Gt30,
        ResolutionStatus::Completed,
        Some("OA-003"),
    )
    .expect("三次 upsert 同主键应不 panic");
    assert_eq!(
        out3,
        resolution_watch::store::UpsertOutcome::Existed,
        "三次 upsert 同主键应继续返回 Existed"
    );

    // 验证：决议库仍然只有 1 条
    let events_after = get_by_week(&conn, "2026-W41").expect("get after");
    assert_eq!(events_after.len(), 1, "三次 upsert 后仍只 1 条");
}

// ===== Test 8: env RESO_DB_PATH 默认值生效 =====

#[test]
fn store_integration_env_default_path_takes_effect_when_env_missing() {
    let _guard = ENV_LOCK.lock().unwrap();

    // 先确保 env 已清空（其他测试可能留下残留）
    unsafe {
        std::env::remove_var(ENV_DB_PATH);
    }

    // open(None) + env 缺失 → 应回落 DEFAULT_DB_PATH，不报错
    let conn = open(None).expect("env 缺失回落默认路径应成功");

    // 表与索引应都已建好（open 内部调用 migrate）
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='resolution_event'",
            [],
            |row| row.get(0),
        )
        .expect("查表");
    assert_eq!(count, 1, "env 缺失回落默认路径后表应建好");

    // 验证：默认路径下的 Sqlite 文件已建好（但此时落点是项目工作目录）
    // （默认路径 ./reso-watch-cache.sqlite 实际是 ./reso-watch-cache.sqlite；
    //  本测试主要验证「不报错 + 表已建好」，不验证文件路径本身以避免污染）
    drop(conn);
}

// ===== 额外补强 1: 完整管线 =====

#[test]
fn store_integration_full_pipeline_open_migrate_upsert_get_count_idempotency() {
    let tmp = TempSqlite::new("full_pipeline");
    let conn = open(Some(tmp.path())).expect("open");

    // 决议条目
    insert_resolution(&conn, "2026-W41", "决议01", DueBucket::Lt3, 2026, 10, 31, "医务科");
    insert_resolution(&conn, "2026-W41", "决议02", DueBucket::Lt7, 2026, 11, 5, "信息科");
    insert_resolution(&conn, "2026-W41", "决议03", DueBucket::Overdue, 2026, 10, 1, "后勤");

    // get_by_week
    let events = get_by_week(&conn, "2026-W41").expect("get_by_week");
    assert_eq!(events.len(), 3);

    // count_by_bucket
    let counts = count_by_bucket(&conn, "2026-W41").expect("count_by_bucket");
    assert_eq!(counts.get(&DueBucket::Lt3).copied(), Some(1));
    assert_eq!(counts.get(&DueBucket::Lt7).copied(), Some(1));
    assert_eq!(counts.get(&DueBucket::Overdue).copied(), Some(1));

    // 周次幂等键
    put_idempotency_key(&conn, "2026-W41", "hash-W41-pipeline").expect("put");
    let got = get_idempotency_key(&conn, "2026-W41").expect("get");
    assert_eq!(got.as_deref(), Some("hash-W41-pipeline"));

    // 整体管线验证：决议 + 幂等键同时存在且不互覆盖
    let events_count = events.len();
    let total_count: usize = counts.values().sum();
    assert_eq!(events_count, total_count, "决议条目数 = 6 桶计数总和");
}

// ===== 额外补强 2: 跨 open 数据持久化 =====

#[test]
fn store_integration_db_state_persists_across_reopen() {
    let tmp = TempSqlite::new("persistence");

    // 连接 1：写入数据 + 幂等键
    {
        let conn = open(Some(tmp.path())).expect("open 1");
        insert_resolution(&conn, "2026-W41", "决议01", DueBucket::Lt30, 2026, 11, 25, "医务科");
        put_idempotency_key(&conn, "2026-W41", "hash-W41-persist").expect("put");
    } // drop 连接

    // 连接 2：同一路径重新 open → 数据可读
    {
        let conn = open(Some(tmp.path())).expect("open 2");
        let events = get_by_week(&conn, "2026-W41").expect("get_by_week reopen");
        assert_eq!(events.len(), 1, "连接断开后数据应仍存在");
        assert_eq!(events[0].resolution_id, "决议01");
        assert_eq!(events[0].owner_department, "医务科");

        let idem = get_idempotency_key(&conn, "2026-W41").expect("get_idempotency_key reopen");
        assert_eq!(idem.as_deref(), Some("hash-W41-persist"), "幂等键应持久化");
    }
}

// ===== 额外补强 3: idx_owner_bucket 二级索引实际可命中 =====

#[test]
fn store_integration_idx_owner_bucket_queryable_with_filter() {
    let tmp = TempSqlite::new("idx_query");
    let conn = open(Some(tmp.path())).expect("open");

    // 跨科室 + 跨桶维度插 4 条
    insert_resolution(&conn, "2026-W41", "决议01", DueBucket::Lt14, 2026, 11, 10, "医务科");
    insert_resolution(&conn, "2026-W41", "决议02", DueBucket::Lt30, 2026, 11, 25, "医务科");
    insert_resolution(&conn, "2026-W41", "决议03", DueBucket::Lt14, 2026, 11, 10, "信息科");
    insert_resolution(&conn, "2026-W41", "决议04", DueBucket::Lt14, 2026, 11, 12, "医务科");

    // 走 idx_owner_bucket：仅医务科 + lt14 桶 → 应命中决议01 + 决议04
    let mut stmt = conn
        .prepare(
            "SELECT resolution_id FROM resolution_event \
             WHERE owner_department = ?1 AND due_bucket = ?2 \
             ORDER BY resolution_id",
        )
        .expect("prepare");
    let rows: Vec<String> = stmt
        .query_map(
            rusqlite::params!["医务科", "lt14"],
            |row| row.get(0),
        )
        .expect("query_map")
        .filter_map(Result::ok)
        .collect();

    assert_eq!(rows, vec!["决议01".to_string(), "决议04".to_string()]);

    // 验证 count_by_bucket 与 idx_owner_bucket 输出一致：
    // count_by_bucket(2026-W41, Lt14) = 3（决议01/03/04），
    // 但 idx_owner_bucket 在 owner_department 维度只 2 条（医务科）
    let counts = count_by_bucket(&conn, "2026-W41").expect("count_by_bucket");
    assert_eq!(counts.get(&DueBucket::Lt14).copied(), Some(3));
    assert_eq!(rows.len(), 2, "idx_owner_bucket 在 owner_department 维度过滤后仅 2 条");
}
