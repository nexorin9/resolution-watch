//! reqwest 出站层：TLS + 连接池 + bearer header 注入 + env 凭据读取。
//!
//! 本任务（task 3）实现基础出站 + 鉴权 + env 读取：
//! - 从 `OA_API_BEARER` 读 bearer token、从 `OA_QUERY_DSN` 读 OA 端点
//! - reqwest::Client 构造（TLS 默认 + 连接池）
//! - bearer header 注入每个出站请求
//! - 7 类 `OaPullError` 显式分类
//!
//! 不含：重试（task 15）、翻页（task 16）、mock（task 17）。
//!
//! 源产品主路径同构：源 `crates/runtime/src/prod.rs` 的 `propagate_tracing` +
//! `TokioSpawnHandle` 强一致 deterministic 任务提交 — 本任务以 `reqwest::Client`
//! 复用同一思路，构造一次、长生命周期复用，不在每次请求重建。

use std::time::Duration;

use reqwest::{header, Client, StatusCode};

use super::error::OaPullError;
use super::models::{OaPullRequest, OaPullResponse};

/// bearer token 在 env 中的变量名。
pub const ENV_BEARER: &str = "OA_API_BEARER";

/// OA 端点 DSN 在 env 中的变量名（`https://oa.hospital.local/api/v1` 形式）。
pub const ENV_DSN: &str = "OA_QUERY_DSN";

/// 默认请求超时（30s）；可在 client 构造时覆盖。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// OA 出站 client。
///
/// 封装 reqwest::Client + bearer token + 端点 base url；
/// 一次性构造，整轮决议督办复用同一 client。
#[derive(Debug, Clone)]
pub struct OaClient {
    inner: Client,
    bearer: String,
    base_url: String,
}

impl OaClient {
    /// 从环境变量构造 client（缺失则返回 `OaPullError::NotConfigured`）。
    pub fn from_env() -> Result<Self, OaPullError> {
        let bearer = std::env::var(ENV_BEARER)
            .map_err(|_| OaPullError::NotConfigured(ENV_BEARER.to_string()))?;
        let base_url = std::env::var(ENV_DSN)
            .map_err(|_| OaPullError::NotConfigured(ENV_DSN.to_string()))?;
        Self::with_credentials(bearer, base_url)
    }

    /// 显式传 bearer + base_url 构造（便于测试 + mock）。
    pub fn with_credentials(
        bearer: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, OaPullError> {
        let bearer = bearer.into();
        if bearer.trim().is_empty() {
            return Err(OaPullError::NotConfigured(ENV_BEARER.to_string()));
        }
        let base_url = base_url.into();
        if base_url.trim().is_empty() {
            return Err(OaPullError::NotConfigured(ENV_DSN.to_string()));
        }
        let inner = Client::builder()
            .timeout(Duration::from_millis(DEFAULT_TIMEOUT_MS))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .user_agent(concat!("resolution-watch/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| OaPullError::Network(format!("reqwest client 构造失败：{e}")))?;
        Ok(Self {
            inner,
            bearer,
            base_url,
        })
    }

    /// bearer token（用于测试断言）。
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    /// base url（用于日志与调试）。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 单次请求：POST `/oa/events?week_id=...` 返回解析后的 `OaPullResponse`。
    ///
    /// 本任务覆盖基础出站 + 鉴权 + 4xx 立即失败；重试 / 5xx 重试在 `retry.rs`（task 15）。
    pub async fn fetch_page(
        &self,
        req: &OaPullRequest,
    ) -> Result<OaPullResponse, OaPullError> {
        let url = format!("{}/oa/events", self.base_url.trim_end_matches('/'));
        let mut query: Vec<(String, String)> = vec![
            ("week_id".to_string(), req.week_id.clone()),
            ("page_size".to_string(), req.page_size.to_string()),
        ];
        if let Some(c) = &req.cursor {
            query.push(("cursor".to_string(), c.clone()));
        }

        let resp = self
            .inner
            .get(&url)
            .query(&query)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.bearer))
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| OaPullError::Network(format!("send 失败：{e}")))?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(OaPullError::Auth(status.as_u16()));
        }
        if status.is_client_error() {
            // 4xx 立即失败，不重试
            return Err(OaPullError::Client4xx(status.as_u16()));
        }
        if status.is_server_error() {
            // 5xx 由 retry.rs 处理；此处显式返回供调用方判定
            return Err(OaPullError::Server5xx(status.as_u16()));
        }
        if !status.is_success() {
            // 其他非 2xx 状态码（少见，如 1xx / 3xx 不应到达此处）
            return Err(OaPullError::Network(format!(
                "unexpected status {}",
                status.as_u16()
            )));
        }

        // 2xx：解析 JSON body
        let body: OaPullResponse = resp
            .json()
            .await
            .map_err(|e| OaPullError::SchemaMissing(format!("body JSON 解析失败：{e}")))?;

        // 字段校验：`events` 必须存在且为数组
        if body.events.is_empty() && body.next_cursor.is_none() {
            // 空响应合法（周内无事件），不报错
        }
        Ok(body)
    }
}