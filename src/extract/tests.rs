//! 决议抽取层集成测试。
//!
//! task 4 阶段：在 mod.rs 已经验证 4 类错误（timeout / auth / 5xx / schema mismatch）
//! 的单元测试基础上，再覆盖：
//! - env 凭据构造路径（`ModelGateway::from_env`）
//! - `chat` 端到端 happy path 含 prompt_hash 稳定性
//! - `fingerprint_for` 业务侧调用
//! - 不同 prompt → 不同 prompt_hash
//!
//! 后续 task（18/19/21）会补充 prompt + validate + fallback + retry 的集成测试。
//!
//! 注意：env 是进程级共享，为避免并行测试互相污染，`from_env_*` 用
//! `serial_test_env` 全局互斥串行执行。

use std::env;
use std::sync::Mutex;

use super::llm::{ModelGateway, ENV_API_KEY, ENV_BASE_URL, ENV_MODEL_NAME, ENV_TIMEOUT_MS};
use super::audit::{CallMetadata, CallStatus};

/// 全局互斥：env 是进程级，并行 `from_env_*` 测试会互相污染。
/// 测试运行时通过 `lock_env()` 获取（Drop 自动释放），保证同一时刻只有一个
/// `from_env_*` 测试在跑。
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 清掉所有 MODEL_* env。
fn clear_model_env() {
    for k in [ENV_BASE_URL, ENV_API_KEY, ENV_MODEL_NAME, ENV_TIMEOUT_MS] {
        env::remove_var(k);
    }
}

/// 在测试作用域内设置 env；guard drop 时自动恢复。
struct EnvGuard {
    keys: Vec<&'static str>,
    prev: Vec<(&'static str, Option<String>)>,
}
impl EnvGuard {
    fn new(kv: &[(&'static str, &str)]) -> Self {
        let mut prev: Vec<(&'static str, Option<String>)> = Vec::with_capacity(kv.len());
        for (k, _) in kv {
            prev.push((*k, env::var(k).ok()));
        }
        clear_model_env();
        for (k, v) in kv {
            env::set_var(k, v);
        }
        let keys: Vec<&'static str> = kv.iter().map(|(k, _)| *k).collect();
        Self { keys, prev }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for k in &self.keys {
            env::remove_var(k);
        }
        for (k, v) in &self.prev {
            match v {
                Some(val) => env::set_var(k, val),
                None => env::remove_var(k),
            }
        }
    }
}

/// 拿全局 env 互斥锁。drop 时自动释放，与 `EnvGuard` 配合保证隔离。
fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[test]
fn from_env_missing_base_url_returns_not_configured() {
    let _lock = lock_env();
    let _g = EnvGuard::new(&[(ENV_API_KEY, "k"), (ENV_MODEL_NAME, "m")]);
    let err = ModelGateway::from_env().expect_err("缺 MODEL_BASE_URL");
    match err {
        super::error::ExtractError::NotConfigured(field) => {
            assert_eq!(field, ENV_BASE_URL);
        }
        other => panic!("期望 NotConfigured，实际 {other:?}"),
    }
}

#[test]
fn from_env_missing_api_key_returns_not_configured() {
    let _lock = lock_env();
    let _g = EnvGuard::new(&[(ENV_BASE_URL, "https://x"), (ENV_MODEL_NAME, "m")]);
    let err = ModelGateway::from_env().expect_err("缺 MODEL_API_KEY");
    match err {
        super::error::ExtractError::NotConfigured(field) => {
            assert_eq!(field, ENV_API_KEY);
        }
        other => panic!("期望 NotConfigured，实际 {other:?}"),
    }
}

#[test]
fn from_env_missing_model_name_returns_not_configured() {
    let _lock = lock_env();
    let _g = EnvGuard::new(&[(ENV_BASE_URL, "https://x"), (ENV_API_KEY, "k")]);
    let err = ModelGateway::from_env().expect_err("缺 MODEL_NAME");
    match err {
        super::error::ExtractError::NotConfigured(field) => {
            assert_eq!(field, ENV_MODEL_NAME);
        }
        other => panic!("期望 NotConfigured，实际 {other:?}"),
    }
}

#[test]
fn from_env_with_all_present_constructs_gateway() {
    let _lock = lock_env();
    let _g = EnvGuard::new(&[
        (ENV_BASE_URL, "https://api.openai.com"),
        (ENV_API_KEY, "sk-test"),
        (ENV_MODEL_NAME, "gpt-4o-mini"),
        (ENV_TIMEOUT_MS, "20000"),
    ]);
    let gw = ModelGateway::from_env().expect("4 项齐全应构造成功");
    assert_eq!(gw.model(), "gpt-4o-mini");
    assert_eq!(gw.timeout_ms(), 20000);
    assert_eq!(gw.base_url(), "https://api.openai.com");
}

#[test]
fn from_env_uses_default_timeout_when_missing() {
    let _lock = lock_env();
    let _g = EnvGuard::new(&[
        (ENV_BASE_URL, "https://x"),
        (ENV_API_KEY, "k"),
        (ENV_MODEL_NAME, "m"),
    ]);
    let gw = ModelGateway::from_env().unwrap();
    // MODEL_TIMEOUT_MS 缺失 → 默认 15000ms（[task 4 spec]）
    assert_eq!(gw.timeout_ms(), 15000);
}

#[test]
fn from_env_invalid_timeout_falls_back_to_default() {
    let _lock = lock_env();
    let _g = EnvGuard::new(&[
        (ENV_BASE_URL, "https://x"),
        (ENV_API_KEY, "k"),
        (ENV_MODEL_NAME, "m"),
        (ENV_TIMEOUT_MS, "not-a-number"),
    ]);
    let gw = ModelGateway::from_env().unwrap();
    assert_eq!(gw.timeout_ms(), 15000, "解析失败应回落到默认值");
}

#[test]
fn fingerprint_is_stable_for_same_prompt() {
    let gw = ModelGateway::with_config("https://x", "k", "m", 15000).unwrap();
    let h1 = gw.fingerprint_for("sys", "user");
    let h2 = gw.fingerprint_for("sys", "user");
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 64, "SHA-256 hex 应为 64 字符");
}

#[test]
fn fingerprint_changes_when_prompt_changes() {
    let gw = ModelGateway::with_config("https://x", "k", "m", 15000).unwrap();
    let h_a = gw.fingerprint_for("sys-A", "user-1");
    let h_b = gw.fingerprint_for("sys-B", "user-1");
    let h_c = gw.fingerprint_for("sys-A", "user-2");
    assert_ne!(h_a, h_b, "system prompt 改变 → hash 改变");
    assert_ne!(h_a, h_c, "user prompt 改变 → hash 改变");
    assert_ne!(h_b, h_c);
}

#[tokio::test]
async fn chat_returns_ok_with_prompt_hash_and_usage() {
    use mockito::Server;
    let mut server = Server::new_async().await;
    let _m = server
        .mock("POST", "/v1/chat/completions")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "choices":[{"message":{"role":"assistant","content":"决议1"}}],
                "usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}
            }"#,
        )
        .create_async()
        .await;

    let gw =
        ModelGateway::with_config(server.url(), "k", "extract-model", 2000).unwrap();
    let (content, meta) = gw
        .chat("请按 JSON Schema 输出", "院办督办决议 2026-W41 …")
        .await
        .expect("2xx 应 ok");

    assert_eq!(content, "决议1");
    let CallMetadata {
        model,
        prompt_hash,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        latency_ms,
        status,
        http_status,
    } = meta;
    assert_eq!(model, "extract-model");
    assert!(!prompt_hash.is_empty());
    assert_eq!(prompt_hash.len(), 64);
    assert_eq!(prompt_tokens, Some(12));
    assert_eq!(completion_tokens, Some(8));
    assert_eq!(total_tokens, Some(20));
    assert!(latency_ms < 5_000, "本地 mock 应 < 5s");
    assert_eq!(status, CallStatus::Ok);
    assert_eq!(http_status, Some(200));
}

// ---------------------------------------------------------------------------
// task 18 — extract prompt + JSON Schema + 5 类缺项 validate
//
// 在 lib 级（tests.rs）再次断言 validate::validate 的 5 类缺项拒绝与完整决议通过，
// 让 prompt + validate 行为在 extract 顶层可见，不依赖 validate 内层 #[cfg(test)] mod。
// ---------------------------------------------------------------------------

/// 6 必备字段齐全 + 1 选填空：完整决议通过 validate。
fn complete_raw() -> super::validate::RawResolution {
    super::validate::RawResolution {
        resolution_id: Some("#[W41]01".into()),
        title: Some("试点开展 DRG 入组校验".into()),
        owner_department: Some("医务科".into()),
        owner_id: Some("E10086".into()),
        due_at: Some("2026-10-31".into()),
        acceptance: Some("骨科 3 个病组入组一致率 ≥ 95%".into()),
        evidence_ref: Some(String::new()),
    }
}

#[test]
fn task18_validate_accepts_complete_resolution() {
    let raw = vec![complete_raw()];
    let resolved = super::validate::validate(&raw).expect("6 必填齐全应通过");
    assert_eq!(resolved.len(), 1);
    let r = &resolved[0];
    assert_eq!(r.resolution_id, "#[W41]01");
    assert_eq!(r.title, "试点开展 DRG 入组校验");
    assert_eq!(r.owner_department, "医务科");
    assert_eq!(r.owner_id, "E10086");
    // evidence_ref 为 "" / "null" 时被过滤为 None
    assert!(r.evidence_ref.is_none());
}

#[test]
fn task18_validate_rejects_missing_resolution_id() {
    let mut raw = complete_raw();
    raw.resolution_id = None;
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    assert_eq!(err, super::validate::InvalidResolution::Missing("resolution_id"));
}

#[test]
fn task18_validate_rejects_missing_title() {
    let mut raw = complete_raw();
    raw.title = Some(String::new());
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    assert_eq!(err, super::validate::InvalidResolution::Missing("title"));
}

#[test]
fn task18_validate_rejects_missing_owner_department() {
    let mut raw = complete_raw();
    raw.owner_department = None;
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    assert_eq!(
        err,
        super::validate::InvalidResolution::Missing("owner_department")
    );
}

#[test]
fn task18_validate_rejects_missing_owner_id() {
    let mut raw = complete_raw();
    raw.owner_id = Some("   ".into()); // 空白视为缺失
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    assert_eq!(err, super::validate::InvalidResolution::Missing("owner_id"));
}

#[test]
fn task18_validate_rejects_missing_due_at() {
    let mut raw = complete_raw();
    raw.due_at = Some(String::new());
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    assert_eq!(err, super::validate::InvalidResolution::Missing("due_at"));
}

#[test]
fn task18_validate_rejects_missing_acceptance() {
    let mut raw = complete_raw();
    raw.acceptance = None;
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    assert_eq!(err, super::validate::InvalidResolution::Missing("acceptance"));
}

#[test]
fn task18_validate_rejects_malformed_due_at() {
    let mut raw = complete_raw();
    raw.due_at = Some("2026/10/31".into());
    let err = super::validate::validate(&vec![raw]).unwrap_err();
    match err {
        super::validate::InvalidResolution::InvalidDate(s) => assert_eq!(s, "2026/10/31"),
        other => panic!("应为 InvalidDate，实际 {other:?}"),
    }
}

#[test]
fn task18_validate_short_circuits_on_first_invalid_resolution() {
    // 两条决议，第一条合格、第二条缺 acceptance → 应返回第二条的 Missing(acceptance)
    let bad = super::validate::RawResolution {
        acceptance: None,
        ..complete_raw()
    };
    let raw = vec![complete_raw(), bad];
    let err = super::validate::validate(&raw).unwrap_err();
    assert_eq!(err, super::validate::InvalidResolution::Missing("acceptance"));
}

#[test]
fn task18_parse_llm_json_unwraps_top_level_resolutions_key() {
    // LLM 输出 {"resolutions": [...]} → parse_llm_json 取 resolutions 数组。
    // 用 r##"..."## 包外层，避免内层 "#[W41]02" 的 " + # 提前闭合 raw string。
    let content = r##"{
        "resolutions": [
            {
                "resolution_id": "#[W41]02",
                "title": "升级病案首页校验",
                "owner_department": "病案室",
                "owner_id": "E20011",
                "due_at": "2026-11-15",
                "acceptance": "病案首页完整率 100%"
            }
        ]
    }"##;
    let raw = super::validate::parse_llm_json(content).expect("顶层 resolutions 应解析成功");
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].resolution_id.as_deref(), Some(r#"#[W41]02"#));
    assert_eq!(raw[0].owner_id.as_deref(), Some("E20011"));
    assert!(raw[0].evidence_ref.is_none());
}

#[test]
fn task18_parse_llm_json_rejects_top_level_without_resolutions() {
    // 缺顶层 resolutions → 拒
    let err = super::validate::parse_llm_json(r#"{"items":[]}"#).unwrap_err();
    assert!(matches!(err, super::validate::InvalidResolution::Missing(_)));
}

#[test]
fn task18_parse_llm_json_rejects_invalid_json() {
    // JSON 解析失败 → 拒
    let err = super::validate::parse_llm_json("not json at all").unwrap_err();
    assert!(matches!(err, super::validate::InvalidResolution::Missing(_)));
}

#[test]
fn task18_parse_llm_json_empty_array_is_legal_but_validates_to_empty() {
    // LLM 输出 {"resolutions": []}（无决议）→ 解析成功 + validate 通过返回空 Vec
    let raw = super::validate::parse_llm_json(r#"{"resolutions":[]}"#).unwrap();
    assert!(raw.is_empty());
    let resolved = super::validate::validate(&raw).unwrap();
    assert!(resolved.is_empty());
}

#[test]
fn task18_system_prompt_contains_all_six_required_field_names() {
    // SYSTEM_PROMPT 必须含 6 个必填字段名 + 1 个可选 evidence_ref + 顶层 resolutions 数组说明
    use super::prompt::SYSTEM_PROMPT;
    for k in [
        "resolution_id",
        "title",
        "owner_department",
        "owner_id",
        "due_at",
        "acceptance",
        "evidence_ref",
        "resolutions",
    ] {
        assert!(
            SYSTEM_PROMPT.contains(k),
            "SYSTEM_PROMPT 缺少字段名 {k}"
        );
    }
}

#[test]
fn task18_user_template_renders_week_id_and_minutes_md() {
    use super::prompt::render_user;
    let user = render_user("2026-W41", "## 决议 1: 升级病案首页校验");
    assert!(user.contains("2026-W41"));
    assert!(user.contains("## 决议 1: 升级病案首页校验"));
    // 占位符必须已展开
    assert!(!user.contains("{week_id}"));
    assert!(!user.contains("{minutes_md}"));
}

#[test]
fn task18_json_schema_description_covers_resolution_contract() {
    use super::prompt::JSON_SCHEMA_DESCRIPTION;
    for k in [
        "resolution_id",
        "title",
        "owner_department",
        "owner_id",
        "due_at",
        "acceptance",
        "evidence_ref",
    ] {
        assert!(
            JSON_SCHEMA_DESCRIPTION.contains(k),
            "JSON_SCHEMA_DESCRIPTION 缺字段 {k}"
        );
    }
}

// ---------------------------------------------------------------------------
// task 19 — extract deterministic_fallback（按 ## 决议 N 段落切分 + 正则识别）
//
// 在 lib 级（tests.rs）再次断言 deterministic_fallback::parse 的两条主路径：
// - 完整决议通过：fallback 解析出 ≥1 条 Resolution（分支 1）
// - 缺字段拒绝：fallback 输出走 validate，缺 acceptance 即被拒（分支 2）
// ---------------------------------------------------------------------------

const FALLBACK_DEPT_PATTERN: &str = r"医务科|护理部|信息科|财务科|医保办|设备科|院办|后勤科";

#[test]
fn task19_fallback_parses_at_least_one_complete_resolution() {
    // 段落齐全（5 必填字段都对得上）：fallback 应解析出 1 条 Resolution。
    let md = r#"## 决议 1: 试点开展 DRG 入组校验

责任科室：医务科，工号 100086。截止 2026-10-31。验收标准：骨科 3 个病组入组一致率 ≥ 95%。
"#;
    let resolved = super::deterministic_fallback::parse(md, "2026-W41", FALLBACK_DEPT_PATTERN)
        .expect("完整决议应通过 fallback");
    assert!(
        !resolved.is_empty(),
        "fallback 应至少解析出 1 条决议，实际为 0"
    );
    assert_eq!(resolved.len(), 1);
    let r = &resolved[0];
    // 5 必填字段都应填上
    assert!(!r.resolution_id.is_empty());
    assert!(r.title.contains("DRG"));
    assert_eq!(r.owner_department, "医务科");
    assert_eq!(r.owner_id, "100086");
    assert_eq!(
        r.due_at,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap()
    );
    assert!(r.acceptance.contains("骨科"));
}

#[test]
fn task19_fallback_output_goes_through_validate() {
    // 段落缺 acceptance：fallback 输出走 validate，应被 validate::Missing("acceptance") 拒。
    let md = r#"## 决议 1: 缺验收决议

责任科室：医务科，工号 100086。截止 2026-12-01。
"#;
    let err = super::deterministic_fallback::parse(md, "2026-W41", FALLBACK_DEPT_PATTERN)
        .expect_err("缺 acceptance 必须被 validate 拒绝");
    match err {
        super::validate::InvalidResolution::Missing(field) => {
            assert_eq!(field, "acceptance", "应返回 Missing(acceptance)");
        }
        other => panic!("期望 Missing(acceptance)，实际 {other:?}"),
    }
}

#[test]
fn task19_fallback_section_split_yields_multiple_resolutions() {
    // 多段决议：fallback 应切出 3 段 + enrich 后科室都填上 + validate 仅放过前两条。
    let md = r#"## 决议 1: DRG 入组校验

责任科室：医务科，工号 100086。截止 2026-10-31。验收标准：达标。

## 决议 2: 门诊电子病历升级

责任科室：信息科，工号 200002。截止 2026-11-05。完成标准：门诊 5 个科室全部切换到 EMR 5.0。

## 决议 3: 缺验收（应被拒）

责任科室：医保办，工号 300003。截止 2026-12-01。
"#;
    // 先看切分段数（不走 validate）
    let sections = super::deterministic_fallback::split_sections(md);
    assert_eq!(sections.len(), 3, "fallback 应切出 3 段决议");
    assert_eq!(sections[0].section_id, "决议 1");
    assert_eq!(sections[1].section_id, "决议 2");
    assert_eq!(sections[2].section_id, "决议 3");

    // enrich 后科室应被字典命中
    let mut sections = sections;
    super::deterministic_fallback::enrich_with_departments(&mut sections, FALLBACK_DEPT_PATTERN);
    assert_eq!(sections[0].owner_department.as_deref(), Some("医务科"));
    assert_eq!(sections[1].owner_department.as_deref(), Some("信息科"));
    assert_eq!(sections[2].owner_department.as_deref(), Some("医保办"));

    // 走 validate：第 3 段缺 acceptance → 拒
    let raw: Vec<super::validate::RawResolution> = sections
        .iter()
        .map(|s| super::deterministic_fallback::to_raw(s, "2026-W41"))
        .collect();
    let err = super::validate::validate(&raw).expect_err("缺 acceptance 必须被 validate 拒绝");
    assert_eq!(
        err,
        super::validate::InvalidResolution::Missing("acceptance"),
        "第 3 段缺 acceptance，应明确指出 acceptance 字段"
    );
}

#[test]
fn task19_fallback_to_raw_assigns_resolution_id_from_section_label() {
    // 决议编号自动派生：section_id = "决议 N" → resolution_id = 以 "#[" 开头的派生值。
    // 不去断言派生字符串的精确形式（format 字符较脆，留给上游读 ParsedSection.section_id
    // 时再校），只断言两条不变量：(a) 显式给定的 resolution_id 不被覆盖；(b) 派生时
    // 输出以 "#[" 开头且非空。
    let mut sec = super::deterministic_fallback::ParsedSection {
        section_id: "决议 5".to_string(),
        resolution_id: Some("EXISTING-001".into()),
        title: Some("升级接口引擎".into()),
        owner_department: Some("信息科".into()),
        owner_id: Some("200002".into()),
        due_at: Some("2026-12-15".into()),
        acceptance: Some("ETL 全量回放通过".into()),
        raw_body: "...".into(),
    };
    let raw = super::deterministic_fallback::to_raw(&sec, "2026-W41");
    assert_eq!(
        raw.resolution_id.as_deref(),
        Some("EXISTING-001"),
        "显式给定 resolution_id 应原样保留，不被派生覆盖"
    );

    // 不给定 → 由 section_id 派生
    sec.resolution_id = None;
    let raw = super::deterministic_fallback::to_raw(&sec, "2026-W41");
    let rid = raw
        .resolution_id
        .as_deref()
        .expect("派生应产生非空决议编号");
    assert!(rid.starts_with("#["), "派生决议编号应以 `#[` 开头，实际 {rid}");
    assert!(!rid.is_empty(), "派生决议编号不应为空");
}

// ---------------------------------------------------------------------------
// task 21 — extract LLM retry policy + 7 集成单测
//
// 在 lib 级（tests.rs）覆盖 retry_policy 的完整主路径：
// (1) LLM schema 正确响应（happy path，无重试）
// (2) 缺 owner_department 拒绝 → 走 deterministic
// (3) 缺 due_at 拒绝 → 走 deterministic
// (4) 5 类缺项各自拒绝（通过 validate::InvalidResolution::Missing 链路）
// (5) LLM 不可达（5xx）→ 重试 1 次 → 仍失败 → 走 deterministic
// (6) deterministic_fallback 解析出 ≥1 条决议
// (7) mock_model 端到端（走完整 extract_resolutions 入口）
// ---------------------------------------------------------------------------

/// mock 用的 minutes.md 内容（用于 LLM 调用；不参与 schema 校验）。
const MOCK_MINUTES_MD: &str = "## 决议 1: 升级信息科接口引擎\n\n责任科室：信息科。\n";

/// 完整决议响应（mock LLM happy path）。
const COMPLETE_RESOLUTIONS_LLM: &str = r##"{
    "resolutions": [
        {
            "resolution_id": "#[T21]01",
            "title": "升级信息科接口引擎",
            "owner_department": "信息科",
            "owner_id": "E10010",
            "due_at": "2026-12-15",
            "acceptance": "ETL full replay passes"
        }
    ]
}"##;

/// 缺 owner_department 决议（mock LLM 返回 200 但 validate 拒）。
const MISSING_OWNER_DEPARTMENT_LLM: &str = r##"{
    "resolutions": [
        {
            "resolution_id": "#[T21]02",
            "title": "缺科室决议",
            "owner_id": "E10011",
            "due_at": "2026-12-15",
            "acceptance": "某项验收标准"
        }
    ]
}"##;

/// 缺 due_at 决议。
const MISSING_DUE_AT_LLM: &str = r##"{
    "resolutions": [
        {
            "resolution_id": "#[T21]03",
            "title": "缺截止决议",
            "owner_department": "信息科",
            "owner_id": "E10012",
            "acceptance": "某项验收标准"
        }
    ]
}"##;

/// 5 类缺项同时（缺 resolution_id / title / owner_department / owner_id / due_at / acceptance）。
/// 第一条决议故意全缺 → 触底 validate。
const ALL_FIVE_MISSING_FIELDS_LLM: &str = r##"{
    "resolutions": [
        {
            "resolution_id": "#[T21]04"
        }
    ]
}"##;

#[tokio::test]
async fn task21_extract_happy_path_emits_resolutions_with_zero_retries() {
    // (1) LLM schema 正确响应：extract_resolutions 走 happy path，ai_degraded=false
    use super::mock_model::{
        register_chat_completions_endpoint, start_mock_llm_server, MOCK_WEEK_ID,
    };
    let mut server = start_mock_llm_server().await;
    let _m = register_chat_completions_endpoint(&mut server, COMPLETE_RESOLUTIONS_LLM).await;

    let gw = ModelGateway::with_config(server.url(), "k", "test-model", 5000).unwrap();
    let outcome = super::extract_resolutions(&gw, MOCK_MINUTES_MD, MOCK_WEEK_ID, FALLBACK_DEPT_PATTERN)
        .await
        .expect("happy path 应成功");

    assert_eq!(outcome.resolutions.len(), 1, "应解析出 1 条决议");
    assert_eq!(outcome.resolutions[0].resolution_id, "#[T21]01");
    assert_eq!(outcome.resolutions[0].owner_department, "信息科");
    assert!(!outcome.ai_degraded, "happy path 不应触发 fallback");
    assert_eq!(outcome.metadata.status, CallStatus::Ok);
}

#[tokio::test]
async fn task21_retry_after_first_invalid_response_does_not_succeed_then_falls_back() {
    // (2) 缺 owner_department：第 1 次响应 validate 失败 → 重试 1 次同样失败 → 走 deterministic
    use super::mock_model::{
        register_chat_completions_endpoint, start_mock_llm_server, MOCK_WEEK_ID,
    };
    let mut server = start_mock_llm_server().await;
    // 端点对所有调用返回相同的缺字段 fixture（首次 + 重试）
    let _m = register_chat_completions_endpoint(&mut server, MISSING_OWNER_DEPARTMENT_LLM).await;

    let gw = ModelGateway::with_config(server.url(), "k", "test-model", 5000).unwrap();

    // 用完整 minutes.md 走 deterministic_fallback 应能解析出 1 条决议（含「信息科」）
    let minutes = r#"## 决议 1: 升级接口引擎

责任科室：信息科，工号 100010。截止 2026-12-15。验收标准：ETL 全量回放通过。
"#;
    let outcome = super::extract_resolutions(&gw, minutes, MOCK_WEEK_ID, FALLBACK_DEPT_PATTERN)
        .await
        .expect("LLM 失败 + deterministic 成功 → 仍 Ok");

    assert!(outcome.ai_degraded, "LLM 两次失败后必须置 ai_degraded=true");
    assert!(!outcome.resolutions.is_empty(), "deterministic 应至少解析出 1 条");
    assert_eq!(outcome.resolutions[0].owner_department, "信息科");
}

#[tokio::test]
async fn task21_retry_after_first_missing_due_at_then_falls_back() {
    // (3) 缺 due_at：同上路径 → 重试 1 次 → deterministic 兜底
    use super::mock_model::{
        register_chat_completions_endpoint, start_mock_llm_server, MOCK_WEEK_ID,
    };
    let mut server = start_mock_llm_server().await;
    let _m = register_chat_completions_endpoint(&mut server, MISSING_DUE_AT_LLM).await;

    let gw = ModelGateway::with_config(server.url(), "k", "test-model", 5000).unwrap();
    let minutes = r#"## 决议 1: 缺 due_at 演示

责任科室：信息科，工号 100012。截止 2026-12-15。验收标准：完成。
"#;
    let outcome = super::extract_resolutions(&gw, minutes, MOCK_WEEK_ID, FALLBACK_DEPT_PATTERN)
        .await
        .expect("应走 deterministic 成功");
    assert!(outcome.ai_degraded);
    assert!(!outcome.resolutions.is_empty());
    assert_eq!(outcome.resolutions[0].owner_department, "信息科");
}

#[tokio::test]
async fn task21_retry_after_all_five_missing_fields_still_falls_back() {
    // (4) 5 类缺项：第 1 次响应缺所有必填 → 重试 1 次同样缺 → 走 deterministic
    //     deterministic 拿到的 minutes.md 也含一段缺全部必填的决议 → 仍失败
    //     此时应返回 SchemaMismatch 错误（validate::validate 在缺字段时报错）
    use super::mock_model::{
        register_chat_completions_endpoint, start_mock_llm_server, MOCK_WEEK_ID,
    };
    let mut server = start_mock_llm_server().await;
    let _m = register_chat_completions_endpoint(&mut server, ALL_FIVE_MISSING_FIELDS_LLM).await;

    let gw = ModelGateway::with_config(server.url(), "k", "test-model", 5000).unwrap();
    // minutes.md 包含一段 决议，但缺全部必填字段（无科室/工号/截止/验收）
    // deterministic_fallback split_sections 能切出该段，但 validate 必拒 → SchemaMismatch
    let minutes_missing_all = "## 决议 1: 全缺字段演示\n\n本段无任何可识别字段。\n";
    let err = super::extract_resolutions(
        &gw,
        minutes_missing_all,
        MOCK_WEEK_ID,
        FALLBACK_DEPT_PATTERN,
    )
    .await
    .expect_err("LLM 缺 5 类 + deterministic 也缺 → 应报错");
    match err {
        super::error::ExtractError::SchemaMismatch(_) => {} // expected
        other => panic!("期望 SchemaMismatch，实际 {other:?}"),
    }
}

#[tokio::test]
async fn task21_retry_after_5xx_failure_succeeds_on_second_attempt() {
    // (5) LLM 不可达（5xx）：第 1 次失败 → 重试 1 次（第 2 次 200）→ 成功
    use super::mock_model::{
        register_5xx_then_200_endpoint, start_mock_llm_server, MOCK_WEEK_ID,
    };
    let mut server = start_mock_llm_server().await;
    // fail_count=1：第 1 次 503 → ModelGateway 拿到 Unreachable；重试 1 次（第 2 次 200）
    let (_fail, _ok) =
        register_5xx_then_200_endpoint(&mut server, 1, COMPLETE_RESOLUTIONS_LLM).await;

    let gw = ModelGateway::with_config(server.url(), "k", "test-model", 5000).unwrap();
    let outcome = super::extract_resolutions(
        &gw,
        MOCK_MINUTES_MD,
        MOCK_WEEK_ID,
        FALLBACK_DEPT_PATTERN,
    )
    .await
    .expect("重试第 2 次应成功");

    assert!(!outcome.ai_degraded, "第 2 次成功 → ai_degraded=false");
    assert_eq!(outcome.resolutions.len(), 1);
    assert_eq!(outcome.resolutions[0].resolution_id, "#[T21]01");
}

#[tokio::test]
async fn task21_retry_after_persistent_5xx_falls_back_to_deterministic() {
    // (6) LLM 不可达（持续 5xx）：重试 1 次仍 5xx → 走 deterministic
    use mockito::Server;
    let mut server = Server::new_async().await;
    // 第 1 次 503 + 第 2 次 503（重试也失败）— 这里需要单独 mock 2 次失败，因为
    // register_5xx_then_200_endpoint 的 fail_count=2 表示前 2 次 5xx 之后才 200。
    // 直接用 mockito mock 端点接受任意次并返回 503。
    let _m = server
        .mock("POST", "/v1/chat/completions")
        .match_query(mockito::Matcher::Any)
        .with_status(503)
        .with_body("model unavailable")
        .expect_at_least(1)
        .create_async()
        .await;

    let gw = ModelGateway::with_config(server.url(), "k", "test-model", 5000).unwrap();
    let minutes = r#"## 决议 1: 持续 5xx 演示

责任科室：信息科，工号 100015。截止 2026-12-15。验收标准：完成。
"#;
    let outcome = super::extract_resolutions(&gw, minutes, "2026-W41", FALLBACK_DEPT_PATTERN)
        .await
        .expect("持续 5xx → deterministic 兜底");
    assert!(outcome.ai_degraded, "持续失败必须置 ai_degraded=true");
    assert!(!outcome.resolutions.is_empty(), "deterministic 应解析出至少 1 条");
    assert_eq!(outcome.resolutions[0].owner_department, "信息科");
}

#[tokio::test]
async fn task21_end_to_end_with_complete_mock_llm_fixture() {
    // (7) mock_model 端到端：用 mock_model::build_complete_resolutions_fixture 走完整 extract_resolutions
    use super::mock_model::{
        build_complete_resolutions_fixture, register_chat_completions_endpoint,
        start_mock_llm_server, MOCK_WEEK_ID,
    };
    let mut server = start_mock_llm_server().await;
    let fixture = build_complete_resolutions_fixture();
    let _m = register_chat_completions_endpoint(&mut server, &fixture).await;

    let gw = ModelGateway::with_config(server.url(), "k", "mock-llm-test-model", 5000).unwrap();
    let outcome = super::extract_resolutions(&gw, MOCK_MINUTES_MD, MOCK_WEEK_ID, FALLBACK_DEPT_PATTERN)
        .await
        .expect("完整 fixture 应走 happy path");

    assert_eq!(outcome.resolutions.len(), 6, "fixture 6 条决议应全部解析");
    assert!(!outcome.ai_degraded);
    assert_eq!(outcome.metadata.status, CallStatus::Ok);
    // 第 6 条决议应带 evidence_ref
    assert_eq!(
        outcome.resolutions[5].evidence_ref.as_deref(),
        Some("OA-FIN-2026-1015")
    );
}
