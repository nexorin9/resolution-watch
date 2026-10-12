//! 可审计调用元数据结构。
//!
//! 每次 ModelGateway 调用的元数据落到 `audit_log.jsonl` 第 3 段（`extract_call`），
//! 含模型名 + prompt hash + token 用量 + 延迟 + 状态 + HTTP 状态码，
//! 用于跨周复盘 LLM 调用行为、定位接口异常与回算成本。
//!
//! 源产品主路径同构：源 `crates/common/src/runtime/mod.rs` 的「强一致
//! deterministic 任务失败可见」 — 本结构以单条元数据记录把
//! 每次 LLM 调用的输入、输出、状态、延迟都显式结构化，便于审计与回溯。

use serde::{Deserialize, Serialize};

/// LLM 调用最终状态。
///
/// 对应 `ExtractError` 的 4 类分类 + 成功态：
/// - `Ok`              — 200 + body 解析成功 + `choices[0].message.content` 非空
/// - `Timeout`         — 超过 `MODEL_TIMEOUT_MS` 或 reqwest 超时
/// - `Auth`            — HTTP 401 / 403
/// - `Unreachable`     — 连接错误 / HTTP 5xx
/// - `SchemaMismatch`  — JSON parse 失败 / 必填字段缺失 / 4xx 非鉴权失败
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Ok,
    Timeout,
    Auth,
    Unreachable,
    SchemaMismatch,
}

impl CallStatus {
    /// 字符串形式（snake_case）便于 JSON 序列化与日志 grep。
    pub fn as_str(&self) -> &'static str {
        match self {
            CallStatus::Ok => "ok",
            CallStatus::Timeout => "timeout",
            CallStatus::Auth => "auth",
            CallStatus::Unreachable => "unreachable",
            CallStatus::SchemaMismatch => "schema_mismatch",
        }
    }
}

impl std::fmt::Display for CallStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单次 LLM 调用的可审计元数据。
///
/// 关键约束：
/// - `prompt_hash` 是 SHA-256 hex（64 字符），**不存原始 prompt 内容**（避免
///   决议条目 + 系统 prompt 重复落 audit 日志导致膨胀；hash 即可回溯同一
///   prompt 的多次调用行为）
/// - `latency_ms` 是 wall clock 时长（含网络 + 推理），用于异常回放
/// - `prompt_tokens` / `completion_tokens` / `total_tokens` 来自响应
///   `usage` 字段（缺失时为 `None`，允许服务端不返回 usage）
/// - `status` + `http_status` 双标：HTTP 状态码用于日志聚合，`status` 用于
///   业务侧判定（重试 / fallback / 拒绝）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallMetadata {
    /// 调用的模型名（如 `gpt-4o-mini` / `qwen2.5-7b-instruct`）。
    pub model: String,
    /// SHA-256 hex of `system + user` 拼成的完整 prompt（用于跨调用去重与回溯）。
    pub prompt_hash: String,
    /// 输入 token 数（来自响应 `usage.prompt_tokens`，服务端不返回时为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prompt_tokens: Option<u64>,
    /// 输出 token 数（来自响应 `usage.completion_tokens`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub completion_tokens: Option<u64>,
    /// 总 token 数（来自响应 `usage.total_tokens`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub total_tokens: Option<u64>,
    /// 端到端耗时（毫秒，含网络 + 推理 + JSON 解析）。
    pub latency_ms: u64,
    /// 业务侧终态（用于重试 / fallback / 拒绝判定）。
    pub status: CallStatus,
    /// HTTP 状态码（连接失败时为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub http_status: Option<u16>,
}

impl CallMetadata {
    /// 构造一条 `Ok` 状态的元数据（成功调用）。
    pub fn ok(
        model: impl Into<String>,
        prompt_hash: impl Into<String>,
        latency_ms: u64,
        http_status: u16,
        usage: Option<TokenUsage>,
    ) -> Self {
        let (prompt_tokens, completion_tokens, total_tokens) = match usage {
            Some(u) => (Some(u.prompt_tokens), Some(u.completion_tokens), Some(u.total_tokens)),
            None => (None, None, None),
        };
        Self {
            model: model.into(),
            prompt_hash: prompt_hash.into(),
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latency_ms,
            status: CallStatus::Ok,
            http_status: Some(http_status),
        }
    }

    /// 构造一条失败态元数据。
    pub fn failure(
        model: impl Into<String>,
        prompt_hash: impl Into<String>,
        latency_ms: u64,
        status: CallStatus,
        http_status: Option<u16>,
    ) -> Self {
        Self {
            model: model.into(),
            prompt_hash: prompt_hash.into(),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            latency_ms,
            status,
            http_status,
        }
    }
}

/// LLM 响应中的 `usage` 字段（OpenAI-compatible 标准字段）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// 从响应 JSON 的 `usage` 对象解析（缺失字段返回 `None`）。
    pub fn from_json_value(v: &serde_json::Value) -> Option<Self> {
        let usage = v.get("usage")?;
        let pt = usage.get("prompt_tokens").and_then(|x| x.as_u64())?;
        let ct = usage.get("completion_tokens").and_then(|x| x.as_u64())?;
        let tt = usage.get("total_tokens").and_then(|x| x.as_u64())?;
        Some(Self {
            prompt_tokens: pt,
            completion_tokens: ct,
            total_tokens: tt,
        })
    }
}

/// 计算 SHA-256 hex（用于 `prompt_hash`）。
///
/// 64 字符小写 hex；空字符串 SHA-256 = `e3b0…`（与 OpenSSL / `sha256sum` 一致）。
pub fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// 把 prompt（system + user）拼成单字符串，便于 hash 与跨调用去重。
///
/// 不同轮次的 system prompt 可不同（如医院上传示例变动），但周内单次
/// 跑批内 hash 应稳定（同一份 minutes.md + 同一份 system prompt）。
pub fn prompt_fingerprint(system: &str, user: &str) -> String {
    sha256_hex(&format!("[SYSTEM]\n{system}\n[USER]\n{user}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_status_snake_case_roundtrip() {
        for s in [
            CallStatus::Ok,
            CallStatus::Timeout,
            CallStatus::Auth,
            CallStatus::Unreachable,
            CallStatus::SchemaMismatch,
        ] {
            let j = serde_json::to_string(&s).unwrap();
            let back: CallStatus = serde_json::from_str(&j).unwrap();
            assert_eq!(s, back);
            // snake_case 形式（无 Quoted 重命名）
            assert!(j.contains(s.as_str()));
        }
    }

    #[test]
    fn call_status_display_matches_as_str() {
        assert_eq!(CallStatus::Ok.to_string(), "ok");
        assert_eq!(CallStatus::Timeout.to_string(), "timeout");
        assert_eq!(CallStatus::Auth.to_string(), "auth");
        assert_eq!(CallStatus::Unreachable.to_string(), "unreachable");
        assert_eq!(CallStatus::SchemaMismatch.to_string(), "schema_mismatch");
    }

    #[test]
    fn sha256_hex_is_64_chars_and_stable() {
        let h1 = sha256_hex("hello world");
        let h2 = sha256_hex("hello world");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        // SHA-256("hello world") = b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        assert_eq!(
            h1,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn prompt_fingerprint_distinguishes_system_and_user() {
        // 相同 user + 不同 system → 不同 hash
        let h1 = prompt_fingerprint("sys-A", "user-1");
        let h2 = prompt_fingerprint("sys-B", "user-1");
        assert_ne!(h1, h2);
        // 同 system + 同 user → 稳定
        let h3 = prompt_fingerprint("sys-A", "user-1");
        assert_eq!(h1, h3);
    }

    #[test]
    fn token_usage_from_json_value_parses_standard_fields() {
        let v = serde_json::json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 250,
                "total_tokens": 350
            }
        });
        let u = TokenUsage::from_json_value(&v).unwrap();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 250);
        assert_eq!(u.total_tokens, 350);
    }

    #[test]
    fn token_usage_from_json_value_returns_none_when_missing() {
        let v = serde_json::json!({"choices": []});
        assert!(TokenUsage::from_json_value(&v).is_none());
        let v = serde_json::json!({"usage": {"prompt_tokens": 1}});
        // completion_tokens 缺失
        assert!(TokenUsage::from_json_value(&v).is_none());
    }

    #[test]
    fn ok_metadata_carries_usage_and_status_ok() {
        let m = CallMetadata::ok(
            "gpt-4o-mini",
            "abc123",
            1234,
            200,
            Some(TokenUsage {
                prompt_tokens: 50,
                completion_tokens: 200,
                total_tokens: 250,
            }),
        );
        assert_eq!(m.status, CallStatus::Ok);
        assert_eq!(m.http_status, Some(200));
        assert_eq!(m.prompt_tokens, Some(50));
        assert_eq!(m.completion_tokens, Some(200));
        assert_eq!(m.total_tokens, Some(250));
    }

    #[test]
    fn failure_metadata_clears_token_fields_and_captures_status() {
        let m = CallMetadata::failure(
            "gpt-4o-mini",
            "abc123",
            5000,
            CallStatus::Timeout,
            None,
        );
        assert_eq!(m.status, CallStatus::Timeout);
        assert_eq!(m.http_status, None);
        assert_eq!(m.prompt_tokens, None);
        assert_eq!(m.completion_tokens, None);
        assert_eq!(m.total_tokens, None);
        assert_eq!(m.latency_ms, 5000);
    }
}
