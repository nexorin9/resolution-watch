//! LLM 决议抽取 retry policy。
//!
//! 触发条件（spec task 21）：
//! - 网络 / 5xx 错误（`ExtractError::is_retryable() == true`）
//! - Schema 校验失败（响应 200 但 `parse_llm_json` / `validate` 拒）
//!
//! 重试策略：
//! - 重试 1 次（间隔 `RETRY_DELAY` = 500ms）
//! - **每次重试独立计时**：每次 `gateway.chat` 调用自身带 `MODEL_TIMEOUT_MS`，
//!   不消耗累计 wall clock；总耗时上限 = `2 × timeout + retry_delay`
//! - **每次重试产出独立 `CallMetadata` → audit_log 记录**：
//!   - 首次失败：status=原 status, latency_ms=本次耗时
//!   - 重试成功：status=Ok, latency_ms=重试耗时
//!   - 重试失败：status=本次 status, latency_ms=本次耗时
//! - 仍失败 → 由调用方决定 fallback / 报错
//!
//! tracing 记录：
//! - 每次重试触发 `tracing::warn!` 含 `attempt / reason / next_delay_ms /
//!   status / latency_ms / prompt_hash`，便于聚合告警与回放
//!
//! 与源 `crates/runtime/src/prod.rs` 的 `propagate_tracing` 同构：
//! 每次重试都生成新 `CallMetadata`（不是累计），便于跨重试去重与失败聚合。

use std::time::Duration;

use tracing::warn;

use super::audit::CallMetadata;
use super::error::ExtractError;
use super::llm::ModelGateway;
use super::prompt::{render_user, SYSTEM_PROMPT};

/// spec 强制：重试间隔 500ms。
pub const RETRY_DELAY: Duration = Duration::from_millis(500);

/// spec 强制：最多重试 1 次（第二次失败即 fallback）。
pub const MAX_RETRIES: u32 = 1;

/// 单次 LLM 调用的统一返回：成功带 (content, meta)，失败带 (failure_meta, err)。
pub type ChatResult = Result<(String, CallMetadata), (CallMetadata, ExtractError)>;

/// 解析 + 校验 + 错误归一：把 validate 的 InvalidResolution 转成 SchemaMismatch。
fn parse_and_validate(content: &str) -> Result<(), String> {
    super::validate::parse_llm_json(content)
        .and_then(|raw| super::validate::validate(&raw))
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// 首次 LLM 调用 + 校验：chat 一次 → parse → validate。
///
/// 返回：
/// - `Ok((content, meta))` — 成功（meta.status=Ok）
/// - `Err((meta, err))` — 网络 / 5xx 错误（`is_retryable()` 由调用方判定）
///   或 schema 校验失败（被归一为 `ExtractError::SchemaMismatch`）
///
/// 调用方识别业务层重试触发的依据：`meta.status == Ok + err = SchemaMismatch`
/// （meta 来自 Ok 路径，但 validate 把内容拒了）。
pub async fn first_attempt(
    gateway: &ModelGateway,
    week_id: &str,
    minutes_md: &str,
) -> ChatResult {
    let user = render_user(week_id, minutes_md);
    match gateway.chat(SYSTEM_PROMPT, &user).await {
        Ok((content, meta)) => match parse_and_validate(&content) {
            Ok(_) => Ok((content, meta)),
            Err(reason) => {
                // Schema 校验失败：200 响应但内容不通过 → 业务层触发重试
                warn!(
                    attempt = 1,
                    status = "schema_invalid",
                    prompt_hash = %meta.prompt_hash,
                    latency_ms = meta.latency_ms,
                    next_delay_ms = RETRY_DELAY.as_millis() as u64,
                    reason = %reason,
                    "extract_resolutions: schema validation failed, will retry once"
                );
                Err((meta, ExtractError::SchemaMismatch(reason)))
            }
        },
        Err((meta, err)) => {
            if err.is_retryable() {
                warn!(
                    attempt = 1,
                    status = %err,
                    prompt_hash = %meta.prompt_hash,
                    latency_ms = meta.latency_ms,
                    next_delay_ms = RETRY_DELAY.as_millis() as u64,
                    "extract_resolutions: retryable error, will retry once"
                );
            }
            Err((meta, err))
        }
    }
}

/// 第二次（重试）调用：间隔 500ms + 调一次 chat + parse + validate。
///
/// 返回 `ChatResult`：成功即解析通过；失败即网络 / schema 错误。
pub async fn retry_attempt(
    gateway: &ModelGateway,
    week_id: &str,
    minutes_md: &str,
) -> ChatResult {
    tokio::time::sleep(RETRY_DELAY).await;
    let user = render_user(week_id, minutes_md);
    match gateway.chat(SYSTEM_PROMPT, &user).await {
        Ok((content, meta)) => match parse_and_validate(&content) {
            Ok(_) => {
                warn!(
                    attempt = 2,
                    status = "ok_after_retry",
                    prompt_hash = %meta.prompt_hash,
                    latency_ms = meta.latency_ms,
                    "extract_resolutions: retry succeeded"
                );
                Ok((content, meta))
            }
            Err(reason) => {
                warn!(
                    attempt = 2,
                    status = "schema_invalid_after_retry",
                    prompt_hash = %meta.prompt_hash,
                    latency_ms = meta.latency_ms,
                    reason = %reason,
                    "extract_resolutions: retry still schema-invalid, caller will fallback"
                );
                Err((meta, ExtractError::SchemaMismatch(reason)))
            }
        },
        Err((meta, err)) => {
            warn!(
                attempt = 2,
                status = %err,
                prompt_hash = %meta.prompt_hash,
                latency_ms = meta.latency_ms,
                "extract_resolutions: retry still failed, caller will fallback"
            );
            Err((meta, err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_500ms() {
        assert_eq!(RETRY_DELAY, Duration::from_millis(500));
    }

    #[test]
    fn max_retries_is_one() {
        assert_eq!(MAX_RETRIES, 1, "spec 强制：重试 1 次");
    }

    #[test]
    fn retry_delay_type_is_duration() {
        // 类型守护：编译期已保证，运行时验证 Duration 字面值
        let d: Duration = RETRY_DELAY;
        assert_eq!(d.as_millis(), 500);
    }
}
