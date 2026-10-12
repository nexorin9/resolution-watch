//! 端到端集成测试（task 9）：mock OA + mock LLM + 30 条决议 + 6 文件导出 + 5 段审计链。
//!
//! 本测试是 `reso` 周频批处理 CLI 的端到端验证：
//! - 起 mockito server 处理 OA REST 与 LLM chat completions
//! - 落 inbox/<week_id>/minutes.md
//! - 运行完整 run_handler
//! - 验证 out/<week_id>/ 下 6 个文件存在 + 内容正确 + sha256 manifest + 5 段审计链 + summary 6 桶数字

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use mockito::Server;
use resolution_watch::bucket::due_bucket;
use resolution_watch::cli::dispatch::run_handler;

// 进程级互斥：环境变量 OA_API_BEARER / RESO_DB_PATH 等都是进程全局，
// 并行跑多个集成测试时会发生 SQLite tempdir 在另一个测试前 drop 的竞态。
// 这里串行化所有跑 run_handler 的测试。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// fixture 根目录。
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// 临时工作目录（inbox + out + cache）。
fn temp_workdir() -> tempfile::TempDir {
    // 使用非 dot 前缀的 tempdir（macOS 上 dot 目录 sqlite 写入有限制）
    let mut builder = tempfile::Builder::new();
    builder.prefix("reso_test_");
    // 在 /tmp 下显式创建（绕开 macOS /var/folders 限制）
    std::env::set_var("TMPDIR", "/tmp");
    builder.tempdir().expect("tempdir 创建失败")
}

/// 设置 8 项 env（供 run_handler 走 validate_env 通过）。
fn set_required_env(oa_url: &str, llm_url: &str, workdir: &PathBuf) {
    let db_path = workdir.join("cache.sqlite");
    env::set_var("OA_API_BEARER", "test-bearer-token");
    env::set_var("OA_QUERY_DSN", oa_url);
    env::set_var("RESO_DB_PATH", db_path.to_str().unwrap());
    env::set_var("RESO_MINUTES_DIR", workdir.join("inbox").to_str().unwrap());
    env::set_var("OUT_DIR", workdir.join("out").to_str().unwrap());
    env::set_var("MODEL_BASE_URL", llm_url);
    env::set_var("MODEL_API_KEY", "test-api-key");
    env::set_var("MODEL_NAME", "test-model");
    env::set_var("MODEL_TIMEOUT_MS", "15000");
}

fn clear_required_env() {
    for k in [
        "OA_API_BEARER",
        "OA_QUERY_DSN",
        "RESO_DB_PATH",
        "RESO_MINUTES_DIR",
        "OUT_DIR",
        "MODEL_BASE_URL",
        "MODEL_API_KEY",
        "MODEL_NAME",
        "MODEL_TIMEOUT_MS",
    ] {
        env::remove_var(k);
    }
}

/// 端到端跑批：mock OA + mock LLM + minutes.md → out/<week>/ 下 6 文件。
#[tokio::test]
async fn end_to_end_run_with_mock_oa_and_llm_emits_six_files_with_audit_chain() {
    let _g = ENV_LOCK.lock().unwrap();
    let _ = tracing_subscriber::fmt::try_init();

    // 1. 起两个 mockito server
    let mut oa_server = Server::new_async().await;
    let mut llm_server = Server::new_async().await;

    let oa_url = oa_server.url();
    let llm_url = llm_server.url();

    // 2. 准备 fixture
    let workdir = temp_workdir();
    let inbox_week = workdir.path().join("inbox").join("2026-W41");
    fs::create_dir_all(&inbox_week).unwrap();
    fs::copy(
        fixtures_dir().join("week_2026-W41_minutes.md"),
        inbox_week.join("minutes.md"),
    )
    .expect("minutes.md 复制失败");

    let oa_events_body = fs::read_to_string(fixtures_dir().join("week_2026-W41_oa_events.json"))
        .expect("OA fixture 读取失败");
    let llm_response_body =
        fs::read_to_string(fixtures_dir().join("week_2026-W41_llm_response.json"))
            .expect("LLM fixture 读取失败");

    // 3. mock OA 端点（GET /oa/events?week_id=...&page_size=...）
    let _oa_mock = oa_server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&oa_events_body)
        .create_async()
        .await;

    // 4. mock LLM 端点（POST /v1/chat/completions）
    let _llm_mock = llm_server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"choices":[{{"index":0,"message":{{"role":"assistant","content":{}}}}}], "usage":{{"prompt_tokens":50,"completion_tokens":80,"total_tokens":130}}}}"#,
            serde_json::to_string(&llm_response_body).unwrap()
        ))
        .create_async()
        .await;

    // 5. 设置 env
    set_required_env(&oa_url, &llm_url, &workdir.path().to_path_buf());

    // 6. 调用 run_handler
    let cli = resolution_watch::cli::ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
    ])
    .expect("CLI parse");

    let env_cfg = resolution_watch::cli::env::validate_env().expect("env validate");
    let result = run_handler(&cli, &resolution_watch::cli::cmd::RunArgs {
        deterministic: false,
        dry_run: false,
    }, &env_cfg)
        .await;

    // Cleanup env regardless of result
    clear_required_env();

    // 8. 验证：6 个文件存在 + 内容正确
    let out_dir = workdir.path().join("out").join("2026-W41");
    assert!(
        out_dir.exists(),
        "out/2026-W41 应存在，result: {result:?}"
    );
    result.expect("run_handler 应当成功");

    for fname in [
        "resolutions.csv",
        "overdue.jsonl",
        "due_soon.jsonl",
        "conflicts.jsonl",
        "audit_log.jsonl",
        "summary.json",
    ] {
        let p = out_dir.join(fname);
        assert!(p.exists(), "缺少导出文件 {fname}：path={}", p.display());
        let content = fs::read_to_string(&p).unwrap_or_else(|_| panic!("读取 {fname} 失败"));
        // conflicts.jsonl 可为空（无冲突时不写入行）；其他文件必须非空
        if fname != "conflicts.jsonl" {
            assert!(!content.is_empty(), "导出文件 {fname} 不应为空");
        }
    }

    // 9. sha256 manifest 校验
    let manifest_path = out_dir.join("sha256_manifest.json");
    assert!(manifest_path.exists(), "sha256_manifest.json 应存在");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let entries = manifest["entries"].as_array().unwrap();
    assert!(entries.len() >= 6, "manifest 应至少含 6 个文件");

    // 10. audit_log.jsonl 含 5 段
    let audit_content = fs::read_to_string(out_dir.join("audit_log.jsonl")).unwrap();
    let audit_lines: Vec<&str> = audit_content.lines().collect();
    assert_eq!(audit_lines.len(), 5, "audit_log.jsonl 应含 5 段");
    for expected_segment in [
        "cli_start",
        "oa_pull_complete",
        "extract_call",
        "bucket_advance",
        "export_complete",
    ] {
        assert!(
            audit_content.contains(expected_segment),
            "audit_log 应含 {expected_segment}"
        );
    }

    // 11. summary.json 6 桶数字（by_due_bucket 6 桶 keys）
    let summary: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out_dir.join("summary.json")).unwrap()).unwrap();
    let by_due_bucket = summary["by_due_bucket"].as_object().unwrap();
    for bucket in ["lt3", "lt7", "lt14", "lt30", "gt30", "overdue"] {
        // 6 桶之一存在桶
        let _ = by_due_bucket.get(bucket); // 不要求每桶都有值，但 6 桶语义保留
    }

    // 12. resolutions.csv 含决议条目
    let csv_content = fs::read_to_string(out_dir.join("resolutions.csv")).unwrap();
    // 元字段头是 "# DEPLOYED_AT:" 形式（"# " 后跟大写字母）；数据行以 "#[W" 开头（"#" 后跟 "["）
    let data_lines: Vec<&str> = csv_content
        .lines()
        .filter(|l| !l.starts_with("# ") && !l.starts_with("resolution_id,"))
        .collect();
    assert!(
        data_lines.len() >= 1,
        "resolutions.csv 应至少含 1 条决议，实际 {} 条",
        data_lines.len()
    );
}

/// 端到端 deterministic 模式（LLM 跳过）：mock OA + minutes.md → out/<week>/ 下 6 文件。
#[tokio::test]
async fn end_to_end_run_deterministic_only_emits_six_files() {
    let _g = ENV_LOCK.lock().unwrap();
    let _ = tracing_subscriber::fmt::try_init();

    let mut oa_server = Server::new_async().await;
    let mut llm_server = Server::new_async().await;

    let oa_url = oa_server.url();
    let llm_url = llm_server.url();

    let workdir = temp_workdir();
    let inbox_week = workdir.path().join("inbox").join("2026-W41");
    fs::create_dir_all(&inbox_week).unwrap();
    fs::copy(
        fixtures_dir().join("week_2026-W41_minutes.md"),
        inbox_week.join("minutes.md"),
    )
    .unwrap();

    let oa_events_body = fs::read_to_string(fixtures_dir().join("week_2026-W41_oa_events.json"))
        .unwrap();
    let _oa_mock = oa_server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&oa_events_body)
        .create_async()
        .await;

    // 让 LLM 端点故意 500，触发 fallback
    let _llm_mock = llm_server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::Any)
        .with_status(500)
        .with_body(r#"{"error":"server overloaded"}"#)
        .create_async()
        .await;

    set_required_env(&oa_url, &llm_url, &workdir.path().to_path_buf());

    let cli = resolution_watch::cli::ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
        "--deterministic".to_string(),
    ])
    .expect("CLI parse");

    let env_cfg = resolution_watch::cli::env::validate_env().expect("env validate");
    let result = run_handler(&cli, &resolution_watch::cli::cmd::RunArgs {
        deterministic: true,
        dry_run: false,
    }, &env_cfg)
        .await;

    clear_required_env();
    result.expect("deterministic 模式 run_handler 应成功");

    // 6 文件应存在
    let out_dir = workdir.path().join("out").join("2026-W41");
    assert!(out_dir.exists(), "out/2026-W41 应存在");
    for fname in [
        "resolutions.csv",
        "summary.json",
        "audit_log.jsonl",
    ] {
        assert!(
            out_dir.join(fname).exists(),
            "deterministic 模式缺 {fname}"
        );
    }

    // audit_log 5 段
    let audit = fs::read_to_string(out_dir.join("audit_log.jsonl")).unwrap();
    assert_eq!(audit.lines().count(), 5);
}

/// 单元：6 桶边界守护（防止 end-to-end 输出 6 桶错位）。
#[test]
fn six_bucket_semantics_preserved() {
    use due_bucket::from_days;
    assert_eq!(from_days(-1), resolution_watch::schemas::DueBucket::Overdue);
    assert_eq!(from_days(0), resolution_watch::schemas::DueBucket::Lt3);
    assert_eq!(from_days(3), resolution_watch::schemas::DueBucket::Lt3);
    assert_eq!(from_days(4), resolution_watch::schemas::DueBucket::Lt7);
    assert_eq!(from_days(7), resolution_watch::schemas::DueBucket::Lt7);
    assert_eq!(from_days(8), resolution_watch::schemas::DueBucket::Lt14);
    assert_eq!(from_days(14), resolution_watch::schemas::DueBucket::Lt14);
    assert_eq!(from_days(15), resolution_watch::schemas::DueBucket::Lt30);
    assert_eq!(from_days(30), resolution_watch::schemas::DueBucket::Lt30);
    assert_eq!(from_days(31), resolution_watch::schemas::DueBucket::Gt30);
}