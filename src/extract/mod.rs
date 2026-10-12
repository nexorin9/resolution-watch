//! 决议抽取层（OpenAI-compatible ModelGateway + deterministic fallback + 5 类缺项校验）。
//!
//! 模块组成：
//! - `llm.rs`                       — ModelGateway 结构 + chat/completions POST + 4 类错误分类（task 4）
//! - `audit.rs`                     — 可审计调用元数据 CallMetadata + CallStatus + sha256 hex（task 4）
//! - `error.rs`                     — ExtractError 4 类错误 + NotConfigured（task 4）
//! - `prompt.rs`                    — SYSTEM_PROMPT + USER_TEMPLATE + JSON Schema 描述
//! - `validate.rs`                   — validate + InvalidResolution::Missing(field)
//! - `deterministic_fallback.rs`    — 段落切分 + 4 类正则识别
//! - `retry_policy.rs`              — retry 1 次（500ms 间隔）+ 仍失败走 fallback + tracing（task 21）
//! - `mock_model.rs`                — mockito server + /v1/chat/completions 端点（测试替身，task 20）
//! - `tests.rs`                     — 集成测试：retry + 端到端

pub mod audit;
pub mod deterministic_fallback;
pub mod error;
pub mod llm;
pub mod mock_model;
pub mod prompt;
pub mod retry_policy;
pub mod validate;

#[cfg(test)]
mod tests;

pub use audit::{CallMetadata, CallStatus, TokenUsage};
pub use error::ExtractError;
pub use llm::{
    ModelGateway, DEFAULT_TIMEOUT_MS, ENV_API_KEY, ENV_BASE_URL, ENV_MODEL_NAME, ENV_TIMEOUT_MS,
};
pub use prompt::{render_user, USER_TEMPLATE, SYSTEM_PROMPT};
pub use validate::{validate as validate_resolutions, InvalidResolution, RawResolution};

use crate::schemas::Resolution;

/// `extract_resolutions` 完整产出。
#[derive(Debug)]
pub struct ExtractOutcome {
    pub resolutions: Vec<Resolution>,
    pub raw_json: String,
    pub metadata: CallMetadata,
    /// 是否走了 deterministic_fallback（ai_degraded 标记）
    pub ai_degraded: bool,
}

/// 顶层入口：从 minutes.md 抽取决议条目（LLM → 失败重试 1 次 → 仍失败走 deterministic）。
///
/// retry 策略（task 21 落盘）：
/// - LLM 第一次失败若 `is_retryable()`（5xx / Timeout / 连接错误） → 重试 1 次（间隔 500ms）
/// - LLM 第一次返回 200 但 `parse_llm_json` / `validate` 失败 → 重试 1 次（业务层触发）
/// - LLM 第一次 `should_fallback()` 但 `!is_retryable()`（Auth / SchemaMismatch） → 立即 fallback
/// - LLM 不可达 + deterministic 也解析失败 → 返回错误（SchemaMismatch）
/// - **每次重试独立计时**：每次 `gateway.chat` 自带 `MODEL_TIMEOUT_MS`，
///   不消耗累计 wall clock；总耗时上限 = `2 × timeout + 500ms`
/// - **每次重试产出独立 `CallMetadata` → audit_log 记录**
pub async fn extract_resolutions(
    gateway: &ModelGateway,
    minutes_md: &str,
    week_id: &str,
    dept_pattern: &str,
) -> Result<ExtractOutcome, ExtractError> {
    use retry_policy::{first_attempt, retry_attempt};

    // 1) 首次 LLM 调用 + parse + validate（合并在 first_attempt）
    match first_attempt(gateway, week_id, minutes_md).await {
        Ok((content, meta)) => Ok(ExtractOutcome {
            resolutions: parse_to_resolutions(&content)?,
            raw_json: content,
            metadata: meta,
            ai_degraded: false,
        }),
        Err((meta, err)) => {
            // 判定是否可重试：
            // - 网络 / 5xx 错误（is_retryable）→ 重试
            // - Schema 校验失败（status=Ok + err=SchemaMismatch）→ 重试
            // - 其他（Auth / NotConfigured）→ 立即 fallback / 报错
            let should_retry = err.is_retryable()
                || (meta.status == CallStatus::Ok
                    && matches!(err, ExtractError::SchemaMismatch(_)));

            if should_retry {
                // 重试 1 次（间隔 500ms，独立计时）
                match retry_attempt(gateway, week_id, minutes_md).await {
                    Ok((content2, meta2)) => Ok(ExtractOutcome {
                        resolutions: parse_to_resolutions(&content2)?,
                        raw_json: content2,
                        metadata: meta2,
                        ai_degraded: false,
                    }),
                    Err((meta2, _err2)) => {
                        // 重试仍失败 → 走 deterministic_fallback
                        fallback_outcome(minutes_md, week_id, dept_pattern, meta2, true).await
                    }
                }
            } else if err.should_fallback() {
                // 不可重试但可降级（Auth / SchemaMismatch 等）→ 立即 fallback
                fallback_outcome(minutes_md, week_id, dept_pattern, meta, true).await
            } else {
                // NotConfigured 等不应 fallback → 报错
                Err(err)
            }
        }
    }
}

/// 把 LLM 返回 content 解析为 `Vec<Resolution>`。失败 → SchemaMismatch。
fn parse_to_resolutions(content: &str) -> Result<Vec<crate::schemas::Resolution>, ExtractError> {
    validate::parse_llm_json(content)
        .and_then(|raw| validate::validate(&raw))
        .map_err(|e| ExtractError::SchemaMismatch(format!("parse+validate failed: {e:?}")))
}

/// fallback 助手：调 deterministic_fallback 并包装 ExtractOutcome。
async fn fallback_outcome(
    minutes_md: &str,
    week_id: &str,
    dept_pattern: &str,
    metadata: CallMetadata,
    ai_degraded: bool,
) -> Result<ExtractOutcome, ExtractError> {
    deterministic_fallback(minutes_md, week_id, dept_pattern)
        .await
        .map(|r| ExtractOutcome {
            resolutions: r.0,
            raw_json: r.1,
            metadata,
            ai_degraded,
        })
}

/// `deterministic_fallback` 内部 helper：返回 (resolutions, raw_json_for_audit)。
async fn deterministic_fallback(
    minutes_md: &str,
    week_id: &str,
    dept_pattern: &str,
) -> Result<(Vec<Resolution>, String), ExtractError> {
    use crate::extract::deterministic_fallback as fb;
    let raw = fb::split_sections(minutes_md);
    let mut sections = raw;
    fb::enrich_with_departments(&mut sections, dept_pattern);
    let raw_resolutions: Vec<RawResolution> = sections
        .iter()
        .map(|s| fb::to_raw(s, week_id))
        .collect();
    let validated = validate::validate(&raw_resolutions).map_err(|_| {
        ExtractError::SchemaMismatch(
            "deterministic_fallback 解析失败，决议条目缺关键字段".to_string(),
        )
    })?;
    let raw_json = serde_json::json!({"resolutions": &raw_resolutions}).to_string();
    Ok((validated, raw_json))
}