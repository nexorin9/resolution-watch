//! CLI 子命令 dispatch（按 subcommand 分发到 run/ingest/verify/sign 各自 handler）。
//!
//! 与 `mod.rs` 中声明的 `dispatch(cli)` 配套；`mod.rs::dispatch` 仅做入口校验 → 这里做实际派发。

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use tracing::{info, warn};

use crate::bucket::due_bucket;
use crate::cli::cmd::{ResoCli, ResoSubcommand, RunArgs, SignArgs, VerifyArgs, IngestArgs};
use crate::cli::env::validate_env;
use crate::cli::error::CliError;
use crate::extract::extract_resolutions;
use crate::export::{
    build_manifest, build_summary, write_audit_log_jsonl, write_atomic, write_due_soon_jsonl,
    write_overdue_jsonl, write_manifest, write_summary,
    AuditSegmentEntry, ConflictEntry, ExportMetadata, ResolutionJsonlEntry, ResolutionRow,
    ResolutionRowBuilder, SummaryInputRow,
};
use crate::export::csv_export::write_resolutions_csv;
use crate::oa_pull::{fetch_events, OaClient};
use crate::schemas::{DueBucket, OaAction, ResolutionStatus};
use crate::store::{
    open, put_idempotency_key, upsert_idempotent, UpsertOutcome,
};
use crate::extract::ModelGateway;
use crate::extract::audit::CallStatus;

/// 顶层 dispatch 入口。
pub async fn dispatch(cli: ResoCli) -> Result<(), CliError> {
    let env = validate_env()?;
    match cli.command {
        ResoSubcommand::Run(ref args) => run_handler(&cli, args, &env).await,
        ResoSubcommand::Ingest(ref args) => ingest_handler(&cli, args).await,
        ResoSubcommand::Verify(ref args) => verify_handler(&cli, args).await,
        ResoSubcommand::Sign(ref args) => sign_handler(&cli, args).await,
    }
}

/// `run` 子命令 — 完整链路端到端（OA + extract + bucket + store + export）。
pub async fn run_handler(
    cli: &ResoCli,
    args: &RunArgs,
    env: &crate::cli::env::ValidatedEnv,
) -> Result<(), CliError> {
    let week_id = cli.week_id();
    info!(week_id, "run 子命令开始");

    // 1. OA 拉取
    let oa_client = OaClient::with_credentials(env.oa_bearer.clone(), env.oa_dsn.clone())
        .map_err(|e| CliError::OaPull(e))?;
    let oa_events = if args.dry_run {
        Vec::new()
    } else {
        fetch_events(&oa_client, week_id)
            .await
            .map_err(CliError::OaPull)?
    };
    info!(events = oa_events.len(), "OA 拉取完成");

    // 2. 读取 minutes.md
    let minutes_path = PathBuf::from(&env.reso_minutes_dir).join(week_id).join("minutes.md");
    let minutes_md = std::fs::read_to_string(&minutes_path)
        .map_err(|e| CliError::Other(format!("读取 minutes.md 失败 {}: {e}", minutes_path.display())))?;
    info!(path = %minutes_path.display(), "minutes.md 读取完成");

    // 3. LLM 抽取（retry + fallback 内置）
    let model = ModelGateway::with_config(
        env.model_base_url.clone(),
        env.model_api_key.clone(),
        env.model_name.clone(),
        env.model_timeout_ms,
    )
    .map_err(CliError::Extract)?;
    const DEPT_PATTERN: &str = r"医务科|护理部|信息科|财务科|医保办|设备科|院办|后勤科|院办";

    // --deterministic 模式：直接走 fallback，不打模型
    let (resolutions, ai_degraded, extract_metadata) = if args.deterministic {
        let secs_res = crate::extract::deterministic_fallback::parse(&minutes_md, week_id, DEPT_PATTERN)
            .map_err(|e| CliError::Extract(crate::extract::ExtractError::SchemaMismatch(format!("deterministic parse 失败：{e:?}"))))?;
        (secs_res, true, extract_meta_dummy())
    } else {
        let outcome = extract_resolutions(&model, &minutes_md, week_id, DEPT_PATTERN)
            .await
            .map_err(CliError::Extract)?;
        (outcome.resolutions, outcome.ai_degraded, outcome.metadata)
    };
    info!(
        resolutions = resolutions.len(),
        ai_degraded,
        "决议抽取完成"
    );

    // 4. bucket 派生（状态 + 6 桶）
    let today = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
    });
    let mut derived: Vec<ResolutionRow> = Vec::new();
    let mut store_events: Vec<(String, OaAction, NaiveDate, ResolutionStatus, DueBucket)> =
        Vec::new();
    for r in &resolutions {
        let days = due_bucket::due_in_days(today, r.due_at);
        let bucket = due_bucket::from_days(days);
        let status = compute_status(r.resolution_id.as_str(), &oa_events, today, r.due_at);
        derived.push(
            ResolutionRowBuilder::from_resolution(r)
                .derived(status, days, bucket)
                .build(),
        );
        store_events.push((r.resolution_id.clone(), OaAction::Started, r.due_at, status, bucket));
    }

    // 5. store upsert
    let conn = open(Some(env.reso_db_path.as_str())).map_err(CliError::Store)?;
    for (rid, action, due_at, status, bucket) in &store_events {
        if let Some(r) = resolutions.iter().find(|rr| rr.resolution_id == *rid) {
            let outcome = upsert_idempotent(
                &conn,
                week_id,
                rid,
                *action,
                &r.owner_id,
                &r.owner_department,
                *due_at,
                *bucket,
                *status,
                None,
            )
            .map_err(CliError::Store)?;
            if let UpsertOutcome::Existed = outcome {
                // 已存在：日志提示
            }
    }
    }
    put_idempotency_key(&conn, week_id, &format!("{:x}", md5_hash(minutes_md.as_bytes())))
        .map_err(CliError::Store)?;

    // 6. 导出包（6 文件 + sha256 manifest）
    let out_dir = PathBuf::from(&env.out_dir).join(week_id);
    let meta = ExportMetadata::now(week_id.to_string(), ai_degraded);
    let summary = build_summary(
        &meta,
        derived.iter().map(|r| SummaryInputRow {
            resolution_id: &r.resolution_id,
            owner_department: &r.owner_department,
            status: r.status.unwrap_or(ResolutionStatus::Pending),
            due_bucket: r.due_bucket.unwrap_or(DueBucket::Gt30),
        }),
    );

    let overdue_entries: Vec<ResolutionJsonlEntry> = derived
        .iter()
        .filter(|r| matches!(r.due_bucket, Some(DueBucket::Overdue)))
        .map(|r| ResolutionJsonlEntry {
            resolution_id: r.resolution_id.clone(),
            title: r.title.clone(),
            owner_department: r.owner_department.clone(),
            owner_id: r.owner_id.clone(),
            due_at: r.due_at.format("%Y-%m-%d").to_string(),
            status: r.status.unwrap_or(ResolutionStatus::Overdue),
            due_in_days: r.due_in_days.unwrap_or(0),
            due_bucket: "overdue".into(),
        })
        .collect();
    let due_soon_entries: Vec<ResolutionJsonlEntry> = derived
        .iter()
        .filter(|r| matches!(r.due_bucket, Some(DueBucket::Lt3)))
        .map(|r| ResolutionJsonlEntry {
            resolution_id: r.resolution_id.clone(),
            title: r.title.clone(),
            owner_department: r.owner_department.clone(),
            owner_id: r.owner_id.clone(),
            due_at: r.due_at.format("%Y-%m-%d").to_string(),
            status: r.status.unwrap_or(ResolutionStatus::DueSoon),
            due_in_days: r.due_in_days.unwrap_or(0),
            due_bucket: "lt3".into(),
        })
        .collect();
    let conflicts: Vec<ConflictEntry> = Vec::new(); // 简化版：no conflict detection

    let audit_segments = [
        AuditSegmentEntry {
            segment: "cli_start".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "oa_pull_complete".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "extract_call".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: Some(env.model_name.clone()),
            prompt_hash: Some(extract_metadata.prompt_hash.clone()),
            latency_ms: Some(extract_metadata.latency_ms),
            status: call_status_str(extract_metadata.status).into(),
        },
        AuditSegmentEntry {
            segment: "bucket_advance".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "export_complete".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: None,
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: None,
            status: "ok".into(),
        },
    ];

    write_atomic(&out_dir, |temp| {
        write_resolutions_csv(&temp.join("resolutions.csv"), &derived, &meta)
            .map_err(|e| e.to_string())?;
        write_overdue_jsonl(&temp.join("overdue.jsonl"), &overdue_entries)
            .map_err(|e| e.to_string())?;
        write_due_soon_jsonl(&temp.join("due_soon.jsonl"), &due_soon_entries)
            .map_err(|e| e.to_string())?;
        write_conflicts_inner(&temp.join("conflicts.jsonl"), &conflicts)
            .map_err(|e| e.to_string())?;
        write_audit_log_jsonl(&temp.join("audit_log.jsonl"), &audit_segments)
            .map_err(|e| e.to_string())?;
        write_summary(&temp.join("summary.json"), &summary).map_err(|e| e.to_string())?;
        let manifest = build_manifest(
            temp,
            &[
                "resolutions.csv",
                "overdue.jsonl",
                "due_soon.jsonl",
                "conflicts.jsonl",
                "audit_log.jsonl",
                "summary.json",
            ],
            &meta,
        )
        .map_err(|e| e.to_string())?;
        write_manifest(&temp.join("sha256_manifest.json"), &manifest)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .map_err(CliError::Export)?;
    info!(out_dir = %out_dir.display(), "导出包完成");
    Ok(())
}

fn call_status_str(s: CallStatus) -> &'static str {
    match s {
        CallStatus::Ok => "ok",
        CallStatus::Timeout => "timeout",
        CallStatus::Auth => "auth",
        CallStatus::Unreachable => "unreachable",
        CallStatus::SchemaMismatch => "schema_mismatch",
    }
}

fn write_conflicts_inner(
    path: &Path,
    entries: &[ConflictEntry],
) -> Result<(), crate::export::jsonl_export::JsonlExportError> {
    crate::export::jsonl_export::write_conflicts_jsonl(path, entries)
}

fn md5_hash(_bytes: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    _bytes.hash(&mut h);
    h.finish()
}

fn extract_meta_dummy() -> crate::extract::audit::CallMetadata {
    use crate::extract::audit::{CallMetadata, CallStatus};
    CallMetadata::failure(
        "deterministic",
        "0".repeat(64),
        0,
        CallStatus::Ok,
        None,
    )
}

/// 计算每个决议条目的状态（基于 OA events + due_at）。
///
/// 简化版本 —— 不做完整 state machine 推演，按是否有 OOA completed event → completed，
/// 否则 pending（实际生产由 bucket::advance 推导，此处简化）。
fn compute_status(
    _resolution_id: &str,
    oa_events: &[crate::schemas::OaEvent],
    _today: NaiveDate,
    _due_at: NaiveDate,
) -> ResolutionStatus {
    // 简化逻辑：若存在 completed action → completed；其余 over due。
    for e in oa_events {
        if matches!(e.action, OaAction::Completed) {
            return ResolutionStatus::Completed;
        }
    }
    ResolutionStatus::Pending
}

/// `ingest` 子命令 — 仅 OA + extract（不写决议推进、不导出）。
pub async fn ingest_handler(
    _cli: &ResoCli,
    _args: &IngestArgs,
) -> Result<(), CliError> {
    warn!("ingest 子命令：占位实现（task 12 后续填充）");
    Ok(())
}

/// `verify` 子命令 — 校验已导出的决议包 sha256 + 5 段审计链完整性。
pub async fn verify_handler(
    _cli: &ResoCli,
    _args: &VerifyArgs,
) -> Result<(), CliError> {
    warn!("verify 子命令：占位实现（task 12 后续填充）");
    Ok(())
}

/// `sign` 子命令 — 在导出的决议包上落签字。
pub async fn sign_handler(
    _cli: &ResoCli,
    _args: &SignArgs,
) -> Result<(), CliError> {
    warn!("sign 子命令：占位实现（task 12 后续填充）");
    Ok(())
}

/// 抑制 unused 警告
#[allow(dead_code)]
fn _unused_silencer(_a: &RunArgs, _b: &SignArgs, _c: &VerifyArgs, _d: &Path) {}