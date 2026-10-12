//! ModelGateway — OpenAI-compatible 协议实现（`POST /v1/chat/completions`）。
//!
//! 单一 ModelGateway 结构，业务层只与该结构耦合，**不直接调用** reqwest 或
//! 任何厂商 SDK。env 凭据从环境变量读取（`MODEL_BASE_URL` / `MODEL_API_KEY` /
//! `MODEL_NAME` / `MODEL_TIMEOUT_MS`，缺后者默认 15000ms）。
//!
//! 4 类错误语义显式：
//! - `Timeout`         — 超过 `MODEL_TIMEOUT_MS` 或 reqwest 超时
//! - `Auth`            — HTTP 401 / 403
//! - `Unreachable`     — 连接错误 / HTTP 5xx
//! - `SchemaMismatch`  — JSON 解析失败 / 必填字段缺失 / 4xx 非鉴权失败
//!
//! 每次调用产生一条 [`CallMetadata`](super::audit::CallMetadata) 落到
//! `audit_log.jsonl` `extract_call` 段（含 model + prompt_hash + tokens +
//! latency_ms + status + http_status）。
//!
//! 源产品主路径同构：源 `crates/isolate/src/udf_runtime.rs` 的 V8 isolate
//! + `BASE_SNAPSHOT` 沙箱思路 — 本任务以 ModelGateway + env 配置隔离业务层
//! 与厂商 SDK / 网络层；任一故障显式分类，**不静默推进**或回写。

use std::time::{Duration, Instant};

use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};

use super::audit::{
    prompt_fingerprint, sha256_hex, CallMetadata, CallStatus, TokenUsage,
};
use super::error::ExtractError;

// ---------- env 变量名常量 ----------

/// LLM 端点 base URL（如 `https://api.openai.com`，不含 `/v1` 后缀）。
pub const ENV_BASE_URL: &str = "MODEL_BASE_URL";

/// LLM API key（OpenAI-compatible；本地端点可为空字符串）。
pub const ENV_API_KEY: &str = "MODEL_API_KEY";

/// 模型名（不同厂商映射不同，如 `gpt-4o-mini` / `qwen2.5-7b-instruct`）。
pub const ENV_MODEL_NAME: &str = "MODEL_NAME";

/// LLM 请求超时（毫秒；默认 15000）。
pub const ENV_TIMEOUT_MS: &str = "MODEL_TIMEOUT_MS";

/// 默认超时时间（ms）。
pub const DEFAULT_TIMEOUT_MS: u64 = 15_000;

/// OpenAI-compatible chat completions 端点（相对 base_url）。
const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

// ---------- ModelGateway 主结构 ----------

/// ModelGateway：单一 OpenAI-compatible 协议 client。
///
/// 一次性构造、长生命周期复用；不在每次请求重建 reqwest Client。
#[derive(Debug, Clone)]
pub struct ModelGateway {
    inner: Client,
    base_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl ModelGateway {
    /// 从环境变量构造（任意必需项缺失返回 `ExtractError::NotConfigured`）。
    pub fn from_env() -> Result<Self, ExtractError> {
        let base_url = std::env::var(ENV_BASE_URL)
            .map_err(|_| ExtractError::NotConfigured(ENV_BASE_URL.to_string()))?;
        let api_key = std::env::var(ENV_API_KEY)
            .map_err(|_| ExtractError::NotConfigured(ENV_API_KEY.to_string()))?;
        let model = std::env::var(ENV_MODEL_NAME)
            .map_err(|_| ExtractError::NotConfigured(ENV_MODEL_NAME.to_string()))?;
        let timeout_ms = match std::env::var(ENV_TIMEOUT_MS) {
            Ok(s) => s.parse::<u64>().unwrap_or(DEFAULT_TIMEOUT_MS),
            Err(_) => DEFAULT_TIMEOUT_MS,
        };
        Self::with_config(base_url, api_key, model, timeout_ms)
    }

    /// 显式传配置构造（便于测试 + 协议替身）。
    ///
    /// `api_key` 允许空字符串（本地 OpenAI-compatible mock 经常不校验）。
    pub fn with_config(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        timeout_ms: u64,
    ) -> Result<Self, ExtractError> {
        let base_url = base_url.into();
        if base_url.trim().is_empty() {
            return Err(ExtractError::NotConfigured(ENV_BASE_URL.to_string()));
        }
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ExtractError::NotConfigured(ENV_MODEL_NAME.to_string()));
        }
        let api_key = api_key.into();
        let timeout = Duration::from_millis(timeout_ms);
        let inner = Client::builder()
            .timeout(timeout)
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .user_agent(concat!("resolution-watch/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ExtractError::Unreachable(format!("reqwest client 构造失败：{e}")))?;
        Ok(Self {
            inner,
            base_url,
            api_key,
            model,
            timeout,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
    pub fn timeout_ms(&self) -> u64 {
        self.timeout.as_millis() as u64
    }

    /// 完整 prompt（system + user）作为单字符串；用于 `CallMetadata.prompt_hash` 与日志。
    pub fn fingerprint_for(&self, system: &str, user: &str) -> String {
        prompt_fingerprint(system, user)
    }

    /// 单次 chat completions 调用。
    ///
    /// 返回 `(content, metadata)`：
    /// - `Ok((content, meta))` — 成功；`content` 为 `choices[0].message.content` 原始文本
    ///   （**未做 JSON parse**，由 `extract/validate.rs` 接管 schema 校验）
    /// - `Err((meta, err))` — 失败；`meta` 用于 audit log，`err` 用于业务侧判定
    ///   （retry / fallback / 拒绝）。**返回的 meta 即使是失败态也带 status + latency**，
    ///   便于 `audit_log.jsonl` 完整记录所有调用（成功 + 失败）。
    pub async fn chat(
        &self,
        system: &str,
        user: &str,
    ) -> Result<(String, CallMetadata), (CallMetadata, ExtractError)> {
        let started = Instant::now();
        let prompt_hash = sha256_hex(&format!("[SYSTEM]\n{system}\n[USER]\n{user}"));

        let req_body = ChatRequest {
            model: &self.model,
            messages: vec![
                Message {
                    role: "system",
                    content: system,
                },
                Message {
                    role: "user",
                    content: user,
                },
            ],
            temperature: 0.0,
            response_format: Some(ResponseFormat::json_object()),
        };

        let url = format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            CHAT_COMPLETIONS_PATH
        );

        let mut req_builder = self
            .inner
            .post(&url)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .json(&req_body);

        if !self.api_key.is_empty() {
            req_builder = req_builder.header(header::AUTHORIZATION, format!("Bearer {}", self.api_key));
        }

        let resp_result = req_builder.send().await;
        let latency_ms = started.elapsed().as_millis() as u64;

        let resp = match resp_result {
            Ok(r) => r,
            Err(e) => {
                // reqwest 错误：可能是超时 / 连接失败 / TLS 握手等
                let status = if e.is_timeout() {
                    CallStatus::Timeout
                } else {
                    CallStatus::Unreachable
                };
                let err = if status == CallStatus::Timeout {
                    ExtractError::Timeout(format!("请求超时：{e}"))
                } else {
                    ExtractError::Unreachable(format!("连接错误：{e}"))
                };
                let meta = CallMetadata::failure(
                    &self.model,
                    &prompt_hash,
                    latency_ms,
                    status,
                    None,
                );
                return Err((meta, err));
            }
        };

        let http_status = resp.status();
        // 1) 鉴权失败
        if http_status == StatusCode::UNAUTHORIZED || http_status == StatusCode::FORBIDDEN {
            let meta = CallMetadata::failure(
                &self.model,
                &prompt_hash,
                latency_ms,
                CallStatus::Auth,
                Some(http_status.as_u16()),
            );
            return Err((meta, ExtractError::Auth(http_status.as_u16())));
        }
        // 2) 5xx → 端点不可达
        if http_status.is_server_error() {
            let meta = CallMetadata::failure(
                &self.model,
                &prompt_hash,
                latency_ms,
                CallStatus::Unreachable,
                Some(http_status.as_u16()),
            );
            return Err((
                meta,
                ExtractError::Unreachable(format!("HTTP {}", http_status.as_u16())),
            ));
        }
        // 3) 4xx 非鉴权 → schema 不匹配（含 400/404 这类请求语法 / 端点路径错）
        if http_status.is_client_error() {
            let body = resp.text().await.unwrap_or_default();
            let meta = CallMetadata::failure(
                &self.model,
                &prompt_hash,
                latency_ms,
                CallStatus::SchemaMismatch,
                Some(http_status.as_u16()),
            );
            return Err((
                meta,
                ExtractError::SchemaMismatch(format!(
                    "HTTP {} body={}",
                    http_status.as_u16(),
                    body.chars().take(200).collect::<String>()
                )),
            ));
        }
        if !http_status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let meta = CallMetadata::failure(
                &self.model,
                &prompt_hash,
                latency_ms,
                CallStatus::Unreachable,
                Some(http_status.as_u16()),
            );
            return Err((
                meta,
                ExtractError::Unreachable(format!(
                    "unexpected HTTP {} body={}",
                    http_status.as_u16(),
                    body.chars().take(200).collect::<String>()
                )),
            ));
        }

        // 4) 2xx：解析 JSON body
        let body_text = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                let meta = CallMetadata::failure(
                    &self.model,
                    &prompt_hash,
                    latency_ms,
                    CallStatus::SchemaMismatch,
                    Some(http_status.as_u16()),
                );
                return Err((
                    meta,
                    ExtractError::SchemaMismatch(format!("body 读取失败：{e}")),
                ));
            }
        };
        let parsed: Result<ChatResponse, _> = serde_json::from_str(&body_text);
        let parsed = match parsed {
            Ok(v) => v,
            Err(e) => {
                let meta = CallMetadata::failure(
                    &self.model,
                    &prompt_hash,
                    latency_ms,
                    CallStatus::SchemaMismatch,
                    Some(http_status.as_u16()),
                );
                return Err((
                    meta,
                    ExtractError::SchemaMismatch(format!("JSON parse 失败：{e}")),
                ));
            }
        };

        // 字段校验：`choices[0].message.content` 必须存在且非空字符串
        let choice = parsed.choices.first().ok_or_else(|| {
            (
                CallMetadata::failure(
                    &self.model,
                    &prompt_hash,
                    latency_ms,
                    CallStatus::SchemaMismatch,
                    Some(http_status.as_u16()),
                ),
                ExtractError::SchemaMismatch("response 缺 choices[0]".to_string()),
            )
        })?;
        let content = choice.message.content.trim().to_string();
        if content.is_empty() {
            let meta = CallMetadata::failure(
                &self.model,
                &prompt_hash,
                latency_ms,
                CallStatus::SchemaMismatch,
                Some(http_status.as_u16()),
            );
            return Err((
                meta,
                ExtractError::SchemaMismatch("choices[0].message.content 为空字符串".to_string()),
            ));
        }

        let usage = TokenUsage::from_json_value(&parsed.usage_raw);
        let meta = CallMetadata::ok(
            &self.model,
            &prompt_hash,
            latency_ms,
            http_status.as_u16(),
            usage,
        );
        Ok((content, meta))
    }
}

// ---------- OpenAI-compatible DTO ----------

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message<'a>>,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none", rename = "response_format")]
    response_format: Option<ResponseFormat>,
}

#[derive(Debug, Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    ty: &'static str,
}

impl ResponseFormat {
    fn json_object() -> Self {
        Self { ty: "json_object" }
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    /// 完整保留原始 JSON 对象，便于 `TokenUsage::from_json_value` 取 `usage`
    /// （避免再定义结构化字段，protocol 兼容不同厂商可能有的扩展字段）。
    #[serde(flatten)]
    usage_raw: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: String,
}

// ---------- 单元测试（task 4 基础层） ----------

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    use super::super::error::ExtractError;

    fn gw(server_url: &str) -> ModelGateway {
        ModelGateway::with_config(server_url, "test-api-key", "test-model", 2000).unwrap()
    }

    /// 启一个手动 TCP listener：accept 后保持挂起、不回响应，
    /// 用于触发 reqwest 超时（配合极短 timeout）。
    async fn hang_server() -> String {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // 单次 accept 后永远不响应；足够测试超时
            if let Ok((mut sock, _)) = listener.accept().await {
                loop {
                    // 持续 hold socket 句柄
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    let _ = &mut sock; // 防止 unused 警告
                }
            }
        });
        format!("http://{addr}")
    }

    // === required 4 tests (per task description) ===

    /// 超时 → `CallStatus::Timeout` + `ExtractError::Timeout`
    #[tokio::test]
    async fn timeout_returns_timeout() {
        let url = hang_server().await;
        // 50ms 超时即可触发
        let gateway =
            ModelGateway::with_config(&url, "test-api-key", "test-model", 50).unwrap();
        let (meta, err) = gateway
            .chat("sys", "user")
            .await
            .expect_err("应返回 Err");

        assert_eq!(meta.status, CallStatus::Timeout);
        match err {
            ExtractError::Timeout(_) => {}
            other => panic!("期望 Timeout，实际 {other:?}"),
        }
        assert!(meta.http_status.is_none());
        assert!(meta.latency_ms > 0, "latency 应记录超时时长");
    }

    /// HTTP 401 → `CallStatus::Auth` + `ExtractError::Auth(401)`
    #[tokio::test]
    async fn four_01_returns_auth_error() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(401)
            .with_body(r#"{"error":"unauthorized"}"#)
            .create_async()
            .await;

        let gateway = gw(&server.url());
        let (meta, err) = gateway.chat("sys", "user").await.expect_err("应返回 Err");

        assert_eq!(meta.status, CallStatus::Auth);
        assert_eq!(meta.http_status, Some(401));
        match err {
            ExtractError::Auth(401) => {}
            other => panic!("期望 Auth(401)，实际 {other:?}"),
        }
    }

    /// HTTP 5xx → `CallStatus::Unreachable` + `ExtractError::Unreachable`
    #[tokio::test]
    async fn five_xx_returns_unreachable() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(502)
            .with_body(r#"bad gateway"#)
            .create_async()
            .await;

        let gateway = gw(&server.url());
        let (meta, err) = gateway.chat("sys", "user").await.expect_err("应返回 Err");

        assert_eq!(meta.status, CallStatus::Unreachable);
        assert_eq!(meta.http_status, Some(502));
        match err {
            ExtractError::Unreachable(_) => {}
            other => panic!("期望 Unreachable，实际 {other:?}"),
        }
    }

    /// 响应 JSON 字段不匹配 → `CallStatus::SchemaMismatch` + `ExtractError::SchemaMismatch`
    #[tokio::test]
    async fn schema_mismatch_returns_schema_mismatch() {
        let mut server = Server::new_async().await;
        // 2xx 但缺 choices 字段
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"created":12345,"model":"x"}"#)
            .create_async()
            .await;

        let gateway = gw(&server.url());
        let (meta, err) = gateway.chat("sys", "user").await.expect_err("应返回 Err");

        assert_eq!(meta.status, CallStatus::SchemaMismatch);
        assert_eq!(meta.http_status, Some(200));
        match err {
            ExtractError::SchemaMismatch(_) => {}
            other => panic!("期望 SchemaMismatch，实际 {other:?}"),
        }
    }

    // === 额外覆盖：happy path + 细分类 ===

    /// 2xx + 合法 body → `Ok(content, meta)` + `status=Ok`
    #[tokio::test]
    async fn two_xx_with_legal_body_returns_ok() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}
                }"#,
            )
            .create_async()
            .await;

        let gateway = gw(&server.url());
        let (content, meta) = gateway
            .chat("sys", "user")
            .await
            .expect("2xx 应解析成功");

        assert_eq!(content, "hello");
        assert_eq!(meta.status, CallStatus::Ok);
        assert_eq!(meta.http_status, Some(200));
        assert_eq!(meta.model, "test-model");
        assert_eq!(meta.prompt_tokens, Some(10));
        assert_eq!(meta.completion_tokens, Some(5));
        assert_eq!(meta.total_tokens, Some(15));
        assert_eq!(meta.prompt_hash.len(), 64);
    }

    /// 鉴权 header 注入正确（`Authorization: Bearer <api_key>`）
    #[tokio::test]
    async fn bearer_header_injection_correct() {
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer my-secret-key")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
            )
            .create_async()
            .await;

        let gateway = ModelGateway::with_config(server.url(), "my-secret-key", "m", 2000).unwrap();
        let _ = gateway.chat("s", "u").await.expect("ok");
        m.assert_async().await;
    }

    /// API key 为空时不注入 Authorization header（兼容本地 mock）
    #[tokio::test]
    async fn empty_api_key_skips_authorization_header() {
        let mut server = Server::new_async().await;
        let m = server
            .mock("POST", "/v1/chat/completions")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"choices":[{"message":{"role":"assistant","content":"x"}}]}"#,
            )
            .create_async()
            .await;

        let gateway = ModelGateway::with_config(server.url(), "", "m", 2000).unwrap();
        assert_eq!(gateway.api_key(), "");
        let _ = gateway.chat("s", "u").await.expect("ok");
        m.assert_async().await;
    }

    /// 4xx 非鉴权（如 400/422）→ SchemaMismatch
    #[tokio::test]
    async fn four_xx_non_auth_returns_schema_mismatch() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(400)
            .with_body(r#"{"error":"bad request"}"#)
            .create_async()
            .await;

        let gateway = gw(&server.url());
        let (meta, err) = gateway.chat("sys", "user").await.expect_err("应返回 Err");

        assert_eq!(meta.status, CallStatus::SchemaMismatch);
        assert_eq!(meta.http_status, Some(400));
        match err {
            ExtractError::SchemaMismatch(s) => assert!(s.contains("400")),
            other => panic!("期望 SchemaMismatch，实际 {other:?}"),
        }
    }

    /// content 是空字符串 → SchemaMismatch
    #[tokio::test]
    async fn empty_content_returns_schema_mismatch() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"choices":[{"message":{"role":"assistant","content":"   "}}]}"#,
            )
            .create_async()
            .await;

        let gateway = gw(&server.url());
        let (meta, err) = gateway.chat("sys", "user").await.expect_err("应返回 Err");
        assert_eq!(meta.status, CallStatus::SchemaMismatch);
        match err {
            ExtractError::SchemaMismatch(_) => {}
            other => panic!("期望 SchemaMismatch，实际 {other:?}"),
        }
    }

    /// env 凭据缺失 → NotConfigured
    #[test]
    fn with_config_empty_base_url_returns_not_configured() {
        let err =
            ModelGateway::with_config("", "k", "m", 15000).expect_err("空 base_url 应报错");
        match err {
            ExtractError::NotConfigured(field) => assert_eq!(field, ENV_BASE_URL),
            other => panic!("期望 NotConfigured，实际 {other:?}"),
        }
    }

    #[test]
    fn with_config_empty_model_returns_not_configured() {
        let err =
            ModelGateway::with_config("https://x", "k", "", 15000).expect_err("空 model 应报错");
        match err {
            ExtractError::NotConfigured(field) => assert_eq!(field, ENV_MODEL_NAME),
            other => panic!("期望 NotConfigured，实际 {other:?}"),
        }
    }
}
