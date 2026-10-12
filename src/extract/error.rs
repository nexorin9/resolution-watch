//! 决议抽取层错误类型。
//!
//! 4 类错误显式分类，对应 task 4 的 ModelGateway 4 类语义
//! （Timeout / Auth / Unreachable / SchemaMismatch）+ 1 类本地
//! 配置错误（NotConfigured），便于 `cli/error.rs` 映射退出码 3
//! （LLM 不可达且 deterministic_fallback 也失败 → 退出码 3）。
//!
//! 与源 `crates/runtime/src/prod.rs` 的「确定性失败可见」思路同构：
//! 任一错误分类都不允许「静默回退 / 静默推进」，由 `llm.rs` 显式 return。

use thiserror::Error;

/// 决议抽取层错误。
#[derive(Debug, Error)]
pub enum ExtractError {
    /// 必需 env 变量未配置（`MODEL_BASE_URL` / `MODEL_API_KEY` / `MODEL_NAME` 任一缺失）。
    #[error("LLM 配置缺失：{0}")]
    NotConfigured(String),

    /// 请求超时（超过 `MODEL_TIMEOUT_MS`，默认 15000ms）。
    #[error("LLM 请求超时：{0}")]
    Timeout(String),

    /// 鉴权失败：HTTP 401 / 403。
    #[error("LLM 鉴权失败：HTTP {0}")]
    Auth(u16),

    /// 端点不可达：连接错误 / HTTP 5xx / 非预期状态码。
    #[error("LLM 端点不可达：{0}")]
    Unreachable(String),

    /// 响应 schema 不匹配：JSON 解析失败 / 必填字段缺失 / 4xx 非鉴权。
    #[error("LLM 响应 schema 不匹配：{0}")]
    SchemaMismatch(String),
}

impl ExtractError {
    /// 是否需要重试：仅 Unreachable 与 Timeout 触发重试，
    /// Auth / SchemaMismatch / NotConfigured 立即失败（确定性失败可见）。
    pub fn is_retryable(&self) -> bool {
        matches!(self, ExtractError::Unreachable(_) | ExtractError::Timeout(_))
    }

    /// 是否需要降级到 deterministic_fallback：
    /// Unreachable / Timeout / SchemaMismatch / Auth 都允许走 fallback；
    /// NotConfigured 直接拒绝（无法 fallback）。
    pub fn should_fallback(&self) -> bool {
        !matches!(self, ExtractError::NotConfigured(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_retryable_distinguishes_retryable_from_immediate() {
        // 可重试
        assert!(ExtractError::Unreachable("conn".into()).is_retryable());
        assert!(ExtractError::Timeout("5s".into()).is_retryable());
        // 不可重试
        assert!(!ExtractError::Auth(401).is_retryable());
        assert!(!ExtractError::SchemaMismatch("missing".into()).is_retryable());
        assert!(!ExtractError::NotConfigured("ENV".into()).is_retryable());
    }

    #[test]
    fn should_fallback_excludes_only_not_configured() {
        assert!(!ExtractError::NotConfigured("MODEL_BASE_URL".into()).should_fallback());
        assert!(ExtractError::Unreachable("x".into()).should_fallback());
        assert!(ExtractError::Timeout("x".into()).should_fallback());
        assert!(ExtractError::Auth(401).should_fallback());
        assert!(ExtractError::SchemaMismatch("x".into()).should_fallback());
    }

    #[test]
    fn error_display_contains_key_field() {
        assert!(ExtractError::Auth(401).to_string().contains("401"));
        assert!(ExtractError::NotConfigured("MODEL_BASE_URL".into())
            .to_string()
            .contains("MODEL_BASE_URL"));
    }
}
