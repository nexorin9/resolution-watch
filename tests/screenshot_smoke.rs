//! 截图采集 smoke — 让端到端真实跑一次，把 CLI 输出与导出包落到固定目录，
//! 供 docs/screenshots/ 下 PNG 渲染使用。
//!
//! 这是 task 30 的真实运行数据采集脚手架：
//! 1. 起两个 mockito server（OA + LLM）
//! 2. 落 inbox/<week>/minutes.md
//! 3. 跑 run_handler（happy path：OA 200 + LLM 200 全量解析 + 桶派生 + 导出）
//! 4. 把 CLI 输出流 + out/<week>/ 6 文件按 PNG-friendly 形式落到一个持久目录
//!
//! 注意：临时目录使用 /tmp/reso_screenshot_ 而不是 TempDir（避免 drop 清理）。

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use mockito::Server;
use resolution_watch::cli::dispatch::run_handler;

// 进程级互斥，避免与其他集成测试并行踩 env
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// 把所有命令行/stdout/stderr 模拟收集到 cli-session.txt。
#[tokio::test]
async fn smoke_run_produces_outputs_for_screenshot() {
    let _g = ENV_LOCK.lock().unwrap();
    let _ = tracing_subscriber::fmt::try_init();

    // 持久目录（不清理）
    let workdir = PathBuf::from("/tmp/reso_screenshot_2026-W41");
    if workdir.exists() {
        fs::remove_dir_all(&workdir).ok();
    }
    fs::create_dir_all(&workdir).unwrap();

    // 起 mock servers
    let mut oa_server = Server::new_async().await;
    let mut llm_server = Server::new_async().await;
    let oa_url = oa_server.url();
    let llm_url = llm_server.url();

    // 准备 inbox + minutes.md
    let inbox_week = workdir.join("inbox").join("2026-W41");
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

    // mock OA 端点
    let _oa_mock = oa_server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&oa_events_body)
        .create_async()
        .await;

    // mock LLM 端点
    let _llm_mock = llm_server
        .mock("POST", "/v1/chat/completions")
        .match_body(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(format!(
            r#"{{"choices":[{{"index":0,"message":{{"role":"assistant","content":{}}}}}],"usage":{{"prompt_tokens":312,"completion_tokens":480,"total_tokens":792}}}}"#,
            serde_json::to_string(&llm_response_body).unwrap()
        ))
        .create_async()
        .await;

    // env
    env::set_var("OA_API_BEARER", "test-bearer-token");
    env::set_var("OA_QUERY_DSN", &oa_url);
    env::set_var("RESO_DB_PATH", workdir.join("cache.sqlite").to_str().unwrap());
    env::set_var("RESO_MINUTES_DIR", workdir.join("inbox").to_str().unwrap());
    env::set_var("OUT_DIR", workdir.join("out").to_str().unwrap());
    env::set_var("MODEL_BASE_URL", &llm_url);
    env::set_var("MODEL_API_KEY", "test-api-key");
    env::set_var("MODEL_NAME", "test-model");
    env::set_var("MODEL_TIMEOUT_MS", "15000");

    let cli = resolution_watch::cli::ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
    ])
    .expect("CLI parse");

    let env_cfg = resolution_watch::cli::env::validate_env().expect("env validate");
    let result = run_handler(
        &cli,
        &resolution_watch::cli::cmd::RunArgs {
            deterministic: false,
            dry_run: false,
        },
        &env_cfg,
    )
    .await;

    // 清理 env（不清理 workdir，让 PNG 渲染脚本能拿到输出）
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

    result.expect("happy path run_handler 应成功");

    let out_dir = workdir.join("out").join("2026-W41");
    assert!(out_dir.exists(), "out/2026-W41 应存在");
    assert!(out_dir.join("resolutions.csv").exists(), "resolutions.csv 缺失");
    assert!(out_dir.join("summary.json").exists(), "summary.json 缺失");
    assert!(out_dir.join("audit_log.jsonl").exists(), "audit_log.jsonl 缺失");
    assert!(out_dir.join("overdue.jsonl").exists(), "overdue.jsonl 缺失");
    assert!(out_dir.join("due_soon.jsonl").exists(), "due_soon.jsonl 缺失");
    assert!(out_dir.join("conflicts.jsonl").exists(), "conflicts.jsonl 缺失");
    assert!(
        out_dir.join("sha256_manifest.json").exists(),
        "sha256_manifest.json 缺失"
    );
}
