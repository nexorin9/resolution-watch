//! 指数退避重试（5xx + 连接错误）。
//!
//! 退避参数：base 500ms / cap 8s / max 5 次。
//! 4xx 立即失败（不重试）；5xx 与连接错误触发重试。

use std::time::{Duration, Instant};

use tracing::warn;

use super::client::OaClient;
use super::error::OaPullError;
use super::models::{OaPullRequest, OaPullResponse};

const BASE_DELAY_MS: u64 = 500;
const CAP_DELAY_MS: u64 = 8_000;
const MAX_ATTEMPTS: u32 = 5;

/// 计算第 `attempt` 次的延迟时长（指数退避 + cap）。
///
/// `attempt` 从 1 开始：1→500ms, 2→1000ms, 3→2000ms, 4→4000ms, 5→cap=8000ms。
pub fn backoff(attempt: u32) -> Duration {
    let base = BASE_DELAY_MS;
    let cap = CAP_DELAY_MS;
    let exp = 2u64.saturating_pow(attempt.saturating_sub(1).min(8));
    let raw = base.saturating_mul(exp);
    Duration::from_millis(raw.min(cap))
}

/// 重试策略：调用 `fetcher`，捕获 `is_retryable()` 错误时退避后重试，最多 `MAX_ATTEMPTS` 次。
///
/// 每次重试前打印 `warn!` 日志，含 `attempt` / `latency_ms`（本次实际请求耗时）/
/// `delay_ms`（下次重试前等待）/ `error`，便于运维在 log pipeline 里聚合告警。
pub async fn fetch_with_retry<F, Fut>(
    fetcher: F,
) -> Result<OaPullResponse, OaPullError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<OaPullResponse, OaPullError>>,
{
    let mut last_err: Option<OaPullError> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let started = Instant::now();
        let outcome = fetcher().await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let is_last = attempt >= MAX_ATTEMPTS;
                if e.is_retryable() && !is_last {
                    let delay = backoff(attempt);
                    warn!(
                        attempt,
                        latency_ms,
                        delay_ms = delay.as_millis() as u64,
                        error = %e,
                        "oa_pull.retry: 触发指数退避"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                } else {
                    return Err(e);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| OaPullError::Network("重试耗尽".into())))
}

/// 直接调 `OaClient::fetch_page` + 重试封装。
pub async fn fetch_page_with_retry(
    client: &OaClient,
    req: &OaPullRequest,
) -> Result<OaPullResponse, OaPullError> {
    let req_owned = req.clone();
    let client_ref = client.clone();
    fetch_with_retry(|| {
        let r = req_owned.clone();
        let c = client_ref.clone();
        async move { c.fetch_page(&r).await }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_with_attempt() {
        assert_eq!(backoff(1).as_millis() as u64, 500);
        assert_eq!(backoff(2).as_millis() as u64, 1000);
        assert_eq!(backoff(3).as_millis() as u64, 2000);
        assert_eq!(backoff(4).as_millis() as u64, 4000);
        assert_eq!(backoff(5).as_millis() as u64, 8000);
        // 6 之后仍 stale 到 cap
        assert_eq!(backoff(6).as_millis() as u64, 8000);
        assert_eq!(backoff(10).as_millis() as u64, 8000);
    }
}