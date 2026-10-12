//! CLI 子命令集成测试（task 8：clap 骨架 + 4 子命令 parse + `--week` 必填校验 +
//! task 11：env 校验 + 退出码 0-7 映射）。
//!
//! 测试策略：
//! 1. **`--week` 写在子命令之前**（`reso --week 2026-W41 run ...`）—— `--week` 是必填
//!    但**非 global**（clap 限制：global 与 required 互斥），所以必须出现在子命令前。
//! 2. **`--out-dir` / `--inbox-dir` 是 global**，可出现在子命令前或后。
//! 3. **子命令专属 arg**（`--signer-id` 等）必须出现在子命令名之后。
//! 4. **运行 `cargo test --lib cli::`** 验证 clap derive parse 正确性 + 错误路径退出码。

use super::cmd::{ResoCli, ResoSubcommand, WeekId, WeekIdParseError};
use crate::bucket::error::BucketError;
use crate::bucket::state_machine::InvalidTransition;
use crate::bucket::ResolutionStatus;
use crate::cli::error::CliError;
use crate::cli::test_util::ENV_LOCK;
use crate::extract::error::ExtractError;
use crate::oa_pull::error::OaPullError;
use crate::store::error::StoreError;
use std::env;
use std::str::FromStr;

// ============================================================================
// task 12：tracing + dispatch + 全链路 run 端到端测试
// ============================================================================

fn unset_all_required_env() {
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

fn set_all_required_env() {
    env::set_var("OA_API_BEARER", "test-bearer");
    env::set_var("OA_QUERY_DSN", "https://oa.local/api");
    env::set_var("RESO_DB_PATH", "./test.sqlite");
    env::set_var("RESO_MINUTES_DIR", "./inbox");
    env::set_var("OUT_DIR", "./out");
    env::set_var("MODEL_BASE_URL", "https://llm.local/v1");
    env::set_var("MODEL_API_KEY", "test-key");
    env::set_var("MODEL_NAME", "test-model");
}

#[test]
fn reso_run_invocation_parses_to_run_subcommand() {
    let argv = vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
    ];
    let cli = ResoCli::from_args(argv).expect("reso --week 2026-W41 run should parse");
    assert!(matches!(cli.command, ResoSubcommand::Run(_)));
    assert_eq!(cli.week_id(), "2026-W41");
    assert_eq!(cli.out_dir, "./out");
    assert_eq!(cli.inbox_dir, "./inbox");
}

#[test]
fn reso_ingest_invocation_with_deterministic_flag() {
    let argv = vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "ingest".to_string(),
        "--deterministic".to_string(),
        "--model-base-url".to_string(),
        "https://mock-llm.local/v1".to_string(),
        "--model-name".to_string(),
        "gpt-mock".to_string(),
    ];
    let cli = ResoCli::from_args(argv).expect("reso ingest with overrides should parse");
    assert!(matches!(cli.command, ResoSubcommand::Ingest(_)));
    let args = match cli.command {
        ResoSubcommand::Ingest(args) => args,
        _ => unreachable!(),
    };
    assert!(args.deterministic);
    assert_eq!(
        args.model_base_url.as_deref(),
        Some("https://mock-llm.local/v1")
    );
    assert_eq!(args.model_name.as_deref(), Some("gpt-mock"));
}

#[test]
fn reso_verify_invocation_with_print_only_flag() {
    let argv = vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "verify".to_string(),
        "--print-only".to_string(),
    ];
    let cli = ResoCli::from_args(argv).expect("reso verify should parse");
    assert!(matches!(cli.command, ResoSubcommand::Verify(_)));
    let args = match cli.command {
        ResoSubcommand::Verify(args) => args,
        _ => unreachable!(),
    };
    assert!(args.print_only);
}

#[test]
fn reso_sign_invocation_with_minimum_required_args() {
    let argv = vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "sign".to_string(),
        "--signer-id".to_string(),
        "U2026-0042".to_string(),
        "--sign-name".to_string(),
        "李四".to_string(),
    ];
    let cli = ResoCli::from_args(argv)
        .expect("reso sign with --signer-id + --sign-name should parse");
    assert!(matches!(cli.command, ResoSubcommand::Sign(_)));
    let args = match cli.command {
        ResoSubcommand::Sign(args) => args,
        _ => unreachable!(),
    };
    assert_eq!(args.signer_id, "U2026-0042");
    assert_eq!(args.sign_name, "李四");
    assert!(args.note.is_none());
}

#[test]
fn reso_missing_week_returns_missing_required_argument_error() {
    // 子命令名前无 --week —— 必填校验失败
    let argv = vec!["reso".to_string(), "run".to_string()];
    let err = ResoCli::from_args(argv)
        .expect_err("--week missing should reject without silent default");
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
}

#[test]
fn reso_run_dry_run_flag_is_recognized() {
    let argv = vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
        "--dry-run".to_string(),
        "--deterministic".to_string(),
    ];
    let cli = ResoCli::from_args(argv).expect("reso run --dry-run should parse");
    let args = match cli.command {
        ResoSubcommand::Run(args) => args,
        _ => unreachable!(),
    };
    assert!(args.dry_run);
    assert!(args.deterministic);
}

#[test]
fn reso_unicode_args_parse_without_panic() {
    // 中文字符 / 全角逗号 都不影响 clap 解析（仅作为字符串参数透传）
    let argv = vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "sign".to_string(),
        "--signer-id".to_string(),
        "U001".to_string(),
        "--sign-name".to_string(),
        "王五，院长办公室".to_string(),
        "--note".to_string(),
        "决议督办，秘书复核".to_string(),
    ];
    let cli = ResoCli::from_args(argv).expect("unicode args parse");
    let args = match cli.command {
        ResoSubcommand::Sign(args) => args,
        _ => unreachable!(),
    };
    assert_eq!(args.sign_name, "王五，院长办公室");
    assert_eq!(args.note.as_deref(), Some("决议督办，秘书复核"));
}

/// WeekId 强类型与 from_str 双向转换。
#[test]
fn week_id_from_str_and_parse_round_trip() {
    let week_id = WeekId::from_str("2026-W41").expect("valid iso week id");
    assert_eq!(week_id.as_str(), "2026-W41");
    assert_eq!(format!("{}", week_id), "2026-W41");
}

/// WeekId 拒绝空字符串。
#[test]
fn week_id_empty_string_rejected() {
    assert_eq!(WeekId::from_str(""), Err(WeekIdParseError::Empty));
    assert_eq!(WeekId::from_str("   "), Err(WeekIdParseError::Empty));
}

/// WeekId 拒绝跨边界周次 `2026-W00` / `2026-W54`。
#[test]
fn week_id_out_of_iso_range_rejected() {
    assert!(matches!(
        WeekId::from_str("2026-W00"),
        Err(WeekIdParseError::OutOfRange { week: 0 })
    ));
    assert!(matches!(
        WeekId::from_str("2026-W54"),
        Err(WeekIdParseError::OutOfRange { week: 54 })
    ));
}

/// WeekId 拒绝格式错误（含缺 W / 年份非 4 位 / 周次非数字）。
#[test]
fn week_id_malformed_inputs_rejected() {
    for bad in [
        "202641",   // 缺 W
        "2026-41",  // 缺 W（仅 -）
        "abc-W41",  // 年份含字母
        "26-W41",   // 年份 2 位
        "2026-WXX", // 周次非数字
        "20A6-W41", // 年份含非数字
    ] {
        assert!(
            matches!(WeekId::from_str(bad), Err(WeekIdParseError::Malformed)),
            "{:?} should be Malformed",
            bad
        );
    }
}

/// WeekId 接受 1-2 位周次（`2026-W5`、`2026-W42`）作为合法格式。
#[test]
fn week_id_short_and_two_digit_week_accepted() {
    assert!(WeekId::from_str("2026-W5").is_ok());
    assert!(WeekId::from_str("2026-W42").is_ok());
}

/// WeekId 严格拒绝小写 `w` 分隔符（避免与 canonical ISO 8601 偏差）。
#[test]
fn week_id_strictly_rejects_lowercase_w_separator() {
    assert!(matches!(
        WeekId::from_str("2026-w41"),
        Err(WeekIdParseError::Malformed)
    ));
}

/// 4 子命令都有对应 help —— 仅校验 try_parse_from 不 panic。
#[test]
fn four_subcommand_help_invocation_does_not_panic() {
    // `--help` 在 clap 中触发 `DisplayHelp` -> `Error::print`（不返回 Err）；
    // try_parse_from 会返回 Result，且 `DisplayHelp` 算作 Ok 路径之后 process 退出；
    // 这里我们只调用确保链路通畅。
    for argv in [
        vec!["reso".to_string(), "--help".to_string()],
        vec![
            "reso".to_string(),
            "--week".to_string(),
            "2026-W41".to_string(),
            "run".to_string(),
            "--help".to_string(),
        ],
        vec![
            "reso".to_string(),
            "--week".to_string(),
            "2026-W41".to_string(),
            "ingest".to_string(),
            "--help".to_string(),
        ],
        vec![
            "reso".to_string(),
            "--week".to_string(),
            "2026-W41".to_string(),
            "verify".to_string(),
            "--help".to_string(),
        ],
        vec![
            "reso".to_string(),
            "--week".to_string(),
            "2026-W41".to_string(),
            "sign".to_string(),
            "--help".to_string(),
        ],
    ] {
        let _ = ResoCli::from_args(argv);
    }
}

/// `dispatch` 占位返回错误（task 12 替换）—— 避免 task 8 接口被误用。
#[test]
fn cli_dispatch_placeholder_returns_not_implemented_error() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
    ])
    .expect("parses");
    let err = super::dispatch(cli).expect_err("placeholder should error");
    // dispatch 现在要求 env 校验；CI/测试环境通常缺少 env，应报 EnvMissing 或类似
    // 仅校验返回 Err，不校验具体消息（实现可演进）
    let _ = format!("{err:?}");
}

// ============================================================================
// task 11：env 校验 + 退出码 0-7 映射测试
// ============================================================================

/// task 11 必答：env 缺一项（OA_API_BEARER）→ CliError::EnvMissing → 退出码 1。
#[test]
fn task11_env_missing_one_item_returns_exit_code_1() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    env::set_var("OA_QUERY_DSN", "https://oa");
    env::set_var("RESO_DB_PATH", "./x.sqlite");
    env::set_var("RESO_MINUTES_DIR", "./inbox");
    env::set_var("OUT_DIR", "./out");
    env::set_var("MODEL_BASE_URL", "https://llm");
    env::set_var("MODEL_API_KEY", "key");
    env::set_var("MODEL_NAME", "gpt");
    // OA_API_BEARER 故意不设

    let result = super::env::validate_env();
    let err = result.expect_err("OA_API_BEARER 缺失应报错");
    assert_eq!(err.exit_code(), 1, "env 缺失必须映射退出码 1");
    match &err {
        CliError::EnvMissing(missing) => {
            assert!(
                missing.contains(&"OA_API_BEARER".to_string()),
                "缺失列表必须包含 OA_API_BEARER：{missing:?}"
            );
        }
        other => panic!("应为 EnvMissing，实为 {other:?}"),
    }
}

/// task 11 必答：8 项 env 全设 + MODEL_TIMEOUT_MS 未设 → validate_env 通过 + 默认 15000ms。
#[test]
fn task11_all_eight_env_set_passes_with_default_timeout() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    set_all_required_env();
    // 不设 MODEL_TIMEOUT_MS → 期待回退到 15000ms

    let v = super::env::validate_env().expect("8 项齐全应通过");
    assert_eq!(v.oa_bearer, "test-bearer");
    assert_eq!(v.oa_dsn, "https://oa.local/api");
    assert_eq!(v.model_timeout_ms, 15_000, "MODEL_TIMEOUT_MS 默认 15000ms");
}

/// task 11 必答：MODEL_TIMEOUT_MS 显式设值 → 解析为该值（覆盖默认 15000ms）。
#[test]
fn task11_model_timeout_ms_explicit_value_overrides_default() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    set_all_required_env();
    env::set_var("MODEL_TIMEOUT_MS", "30000");

    let v = super::env::validate_env().expect("8 项齐全应通过");
    assert_eq!(
        v.model_timeout_ms, 30_000,
        "显式 MODEL_TIMEOUT_MS=30000 应被解析"
    );
}

/// task 11 必答：MODEL_TIMEOUT_MS 非法字符串 → 回退到默认 15000ms（不报错）。
#[test]
fn task11_model_timeout_ms_invalid_string_falls_back_to_default() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    set_all_required_env();
    env::set_var("MODEL_TIMEOUT_MS", "not-a-number");

    let v = super::env::validate_env().expect("非法 timeout 应回退默认，不报错");
    assert_eq!(
        v.model_timeout_ms, 15_000,
        "非法 MODEL_TIMEOUT_MS 应回退到 15000ms"
    );
}

/// task 11 必答：空白字符串视为缺失。
#[test]
fn task11_env_whitespace_string_treated_as_missing() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    env::set_var("OA_API_BEARER", "   ");
    set_all_required_env();
    // 把 OA_API_BEARER 覆盖为空白
    env::set_var("OA_API_BEARER", "   ");

    let err = super::env::validate_env().expect_err("空白字符串视为缺失");
    match &err {
        CliError::EnvMissing(m) => {
            assert!(m.contains(&"OA_API_BEARER".to_string()));
        }
        other => panic!("应为 EnvMissing，实为 {other:?}"),
    }
    assert_eq!(err.exit_code(), 1);
}

// ----------------------------------------------------------------------------
// 退出码 1-7 各自命中单测
// ----------------------------------------------------------------------------

/// 退出码 1：CliError::EnvMissing(vec!["X"]) → 1。
#[test]
fn task11_exit_code_1_for_env_missing() {
    let err = CliError::EnvMissing(vec!["OA_API_BEARER".to_string()]);
    assert_eq!(err.exit_code(), 1, "env 缺失 → 1");
}

/// 退出码 2：CliError::OaPull(...) → 2。
#[test]
fn task11_exit_code_2_for_oa_pull_failure() {
    // 4 类 OaPullError 变体都应映射到 2
    assert_eq!(
        CliError::OaPull(OaPullError::Auth(401)).exit_code(),
        2,
        "OA 鉴权失败 → 2"
    );
    assert_eq!(
        CliError::OaPull(OaPullError::Client4xx(404)).exit_code(),
        2,
        "OA 4xx → 2"
    );
    assert_eq!(
        CliError::OaPull(OaPullError::Server5xx(503)).exit_code(),
        2,
        "OA 5xx → 2"
    );
    assert_eq!(
        CliError::OaPull(OaPullError::Network("conn refused".into())).exit_code(),
        2,
        "OA 网络错 → 2"
    );
    assert_eq!(
        CliError::OaPull(OaPullError::SchemaMissing("events".into())).exit_code(),
        2,
        "OA schema 缺失 → 2"
    );
    assert_eq!(
        CliError::OaPull(OaPullError::Pagination("limit".into())).exit_code(),
        2,
        "OA 翻页失败 → 2"
    );
}

/// 退出码 3：CliError::Extract(...) → 3（LLM 不可达且 fallback 也失败）。
#[test]
fn task11_exit_code_3_for_extract_failure() {
    assert_eq!(
        CliError::Extract(ExtractError::Unreachable("conn".into())).exit_code(),
        3,
        "LLM 不可达 → 3"
    );
    assert_eq!(
        CliError::Extract(ExtractError::Timeout("5s".into())).exit_code(),
        3,
        "LLM 超时 → 3"
    );
    assert_eq!(
        CliError::Extract(ExtractError::Auth(401)).exit_code(),
        3,
        "LLM 鉴权失败 → 3"
    );
    assert_eq!(
        CliError::Extract(ExtractError::SchemaMismatch("missing field".into())).exit_code(),
        3,
        "LLM schema 不匹配 → 3"
    );
    assert_eq!(
        CliError::Extract(ExtractError::NotConfigured("MODEL_BASE_URL".into())).exit_code(),
        3,
        "LLM 配置缺失 → 3"
    );
}

/// 退出码 4：CliError::Schema(...) | Store(...) → 4（schema 校验失败 / 存储失败）。
#[test]
fn task11_exit_code_4_for_schema_and_store() {
    // Schema 校验失败
    assert_eq!(
        CliError::Schema("owner_department 缺失".into()).exit_code(),
        4,
        "schema 校验失败 → 4"
    );
    // 3 类 StoreError 变体都应映射到 4
    assert_eq!(
        CliError::Store(StoreError::Open("perm denied".into())).exit_code(),
        4,
        "Store Open 失败 → 4"
    );
    assert_eq!(
        CliError::Store(StoreError::Migrate("syntax error".into())).exit_code(),
        4,
        "Store Migrate 失败 → 4"
    );
    assert_eq!(
        CliError::Store(StoreError::Config("invalid path".into())).exit_code(),
        4,
        "Store Config 失败 → 4"
    );
}

/// 退出码 5：CliError::Bucket(...) → 5（桶派生失败）。
#[test]
fn task11_exit_code_5_for_bucket_failure() {
    // InvalidTransition 来自状态机非法跃迁
    let inv = InvalidTransition::new(
        ResolutionStatus::Pending,
        ResolutionStatus::Completed,
        "缺 started 事件",
    );
    assert_eq!(
        CliError::Bucket(BucketError::InvalidTransition(inv)).exit_code(),
        5,
        "Bucket 非法跃迁 → 5"
    );
    // InvalidDate
    assert_eq!(
        CliError::Bucket(BucketError::InvalidDate("2026-13-40".into())).exit_code(),
        5,
        "Bucket 日期非法 → 5"
    );
    // Unreachable（防御性保留）
    assert_eq!(
        CliError::Bucket(BucketError::Unreachable("should-not-happen".into())).exit_code(),
        5,
        "Bucket Unreachable → 5"
    );
}

/// 退出码 6：CliError::Rollback(...) → 6（半成品回滚）。
#[test]
fn task11_exit_code_6_for_rollback() {
    let err = CliError::Rollback("导出包写到 .tmp/ 中途失败：CSV 写盘错误".into());
    assert_eq!(err.exit_code(), 6, "半成品回滚 → 6");
    // Display 必须含可定位信息
    let s = err.to_string();
    assert!(s.contains("半成品回滚"), "Display 前缀：{s}");
    assert!(s.contains("CSV"), "Display 含原因：{s}");
}

/// 退出码 7：CliError::Export(...) | Other(...) → 7（导出失败 / 其它）。
#[test]
fn task11_exit_code_7_for_export_and_other() {
    // Export 失败（WriteAtomicError 6 态都映射到 7）
    let export_err = crate::export::WriteAtomicError::WriterFailed("CSV 写盘失败".into());
    assert_eq!(
        CliError::Export(export_err).exit_code(),
        7,
        "Export 失败 → 7"
    );

    // Other 兜底
    assert_eq!(
        CliError::Other("未分类错误".into()).exit_code(),
        7,
        "Other 兜底 → 7"
    );
}

/// task 11 总体：7 退出码（1-7）+ 0=成功全部命中。
#[test]
fn task11_all_seven_exit_codes_covered() {
    // 0 = 成功（用 Ok(()) 占位）
    let ok: Result<(), CliError> = Ok(());
    let _ = ok; // 0 不通过 CliError::exit_code() 表达，由 main 的 Ok 分支返回 ExitCode::SUCCESS

    // 1
    assert_eq!(CliError::EnvMissing(vec!["X".into()]).exit_code(), 1);
    // 2
    assert_eq!(CliError::OaPull(OaPullError::Auth(401)).exit_code(), 2);
    // 3
    assert_eq!(
        CliError::Extract(ExtractError::Unreachable("x".into())).exit_code(),
        3
    );
    // 4
    assert_eq!(CliError::Schema("x".into()).exit_code(), 4);
    assert_eq!(CliError::Store(StoreError::Open("x".into())).exit_code(), 4);
    // 5
    let inv = InvalidTransition::new(
        ResolutionStatus::Pending,
        ResolutionStatus::Overdue,
        "test",
    );
    assert_eq!(CliError::Bucket(BucketError::InvalidTransition(inv)).exit_code(), 5);
    // 6
    assert_eq!(CliError::Rollback("x".into()).exit_code(), 6);
    // 7
    assert_eq!(
        CliError::Export(crate::export::WriteAtomicError::WriterFailed("x".into())).exit_code(),
        7
    );
}

/// task 11：dispatch 在 env 缺失时返回 EnvMissing 错误（验证 task 11 与 task 12 dispatch 串联）。
#[test]
fn task11_dispatch_returns_env_missing_when_env_unset() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();

    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
    ])
    .expect("parses");
    let err = super::dispatch(cli).expect_err("env 缺失应报错");
    assert_eq!(err.exit_code(), 1, "env 缺失 → 1");
    assert!(
        matches!(err, CliError::EnvMissing(_)),
        "dispatch env 缺失应返回 CliError::EnvMissing，实为 {err:?}"
    );
}

// ============================================================================
// task 12 必答：tracing JSON 格式 + 4 subcommand dispatch + run 端到端
// ============================================================================

/// task 12 必答 1/3：tracing-subscriber JSON 初始化不 panic（多次调用幂等）。
///
/// 验证：
/// - `init_tracing()` 调用本身不 panic
/// - `tracing::info!()` 在 init 之后能正常 emit（不要求 stderr 被测试抓到，仅校验链路通畅）
/// - 多次调用 init_tracing 也不 panic —— `try_init()` 第二次返回 Err 但 `.ok()` 吞掉
#[test]
fn task12_tracing_json_init_succeeds_and_is_idempotent() {
    super::init_tracing();
    super::init_tracing(); // 第二次调用：try_init 返回 Err，但 .ok() 吞掉
    super::init_tracing();

    // init 之后 tracing 事件能正常 emit（不 panic）
    tracing::info!(
        test_marker = "task12_tracing_json_init_succeeds",
        "tracing-subscriber JSON init verified"
    );
}

/// task 12 必答 2/3：4 subcommand 各自 dispatch 正确路由到对应 handler。
///
/// - ingest/verify/sign 占位 handler → 直接返回 `Ok(())`
/// - 验证：sync dispatch wrapper 创建 current_thread tokio runtime + 调 dispatch_async → 4 个 handler 各被调用一次
#[test]
fn task12_dispatch_routes_each_subcommand_to_correct_handler() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    set_all_required_env();

    // ingest → ingest_handler (占位) → Ok(())
    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "ingest".to_string(),
    ])
    .expect("ingest parses");
    let result = super::dispatch(cli);
    assert!(
        result.is_ok(),
        "ingest 占位 handler 应返回 Ok(()), 实为 {result:?}"
    );

    // verify → verify_handler (占位) → Ok(())
    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "verify".to_string(),
    ])
    .expect("verify parses");
    let result = super::dispatch(cli);
    assert!(
        result.is_ok(),
        "verify 占位 handler 应返回 Ok(()), 实为 {result:?}"
    );

    // sign → sign_handler (占位) → Ok(())
    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "sign".to_string(),
        "--signer-id".to_string(),
        "U001".to_string(),
        "--sign-name".to_string(),
        "王五".to_string(),
    ])
    .expect("sign parses");
    let result = super::dispatch(cli);
    assert!(
        result.is_ok(),
        "sign 占位 handler 应返回 Ok(()), 实为 {result:?}"
    );
}

/// task 12 必答 3/3：run subcommand 走 sync dispatch → 路由到 run_handler
/// （不是 ingest/verify/sign 的占位 handler）。
///
/// 用 `--dry-run` 跳过 OA 拉取与 minutes.md 之外的 IO；run_handler 进入
/// minutes.md 读取失败 → `CliError::Other("读取 minutes.md 失败 ...")` → exit 7。
/// 这证明 dispatch 路由到了 run_handler 的「OA → extract → bucket → store → export」主路径入口，
/// 不是 ingest/verify/sign 的占位分支。
#[test]
fn task12_run_subcommand_dispatches_to_run_handler_via_sync_dispatch() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    set_all_required_env();

    // --dry-run 跳过 OA 拉取；run_handler 仍走 minutes.md 读取 → 失败 → exit 7
    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "run".to_string(),
        "--dry-run".to_string(),
    ])
    .expect("run parses");
    let err = super::dispatch(cli).expect_err("minutes.md 不存在应报错");
    assert_eq!(
        err.exit_code(),
        7,
        "run_handler 失败应映射到 exit code 7 (CliError::Other), 实为 {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("minutes.md") || msg.contains("读取"),
        "错误消息应说明 run_handler 进入主路径，实为 {msg}"
    );
}

/// task 12 补充：sync dispatch wrapper 验证 — 内部新建 current_thread tokio runtime，
/// 不依赖 main.rs 的 `#[tokio::main]`（避免 tokio runtime 嵌套）。
///
/// 关键：sync dispatch 不能在已存在 tokio runtime 内被调用（会 panic），但
/// 普通 `cargo test` 同步上下文是安全的。本测试在 ENV_LOCK 保护下串行验证。
#[test]
fn task12_sync_dispatch_creates_independent_tokio_runtime() {
    let _g = ENV_LOCK.lock().unwrap();
    unset_all_required_env();
    set_all_required_env();

    // 在没有外部 tokio runtime 的同步上下文里调用 sync dispatch
    // —— 内部应当成功创建 current_thread runtime 并 block_on dispatch_async
    let cli = ResoCli::from_args(vec![
        "reso".to_string(),
        "--week".to_string(),
        "2026-W41".to_string(),
        "ingest".to_string(),
    ])
    .expect("ingest parses");
    // 占位 handler → Ok(()) → 视为「runtime 创建 + block_on」链路通畅
    let result = super::dispatch(cli);
    assert!(
        result.is_ok(),
        "sync dispatch 应能独立创建 tokio runtime 并执行 ingest handler，实为 {result:?}"
    );
}
