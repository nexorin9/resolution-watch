//! OA 拉取层错误类型。
//!
//! 7 类错误显式分类，便于上游 (`cli::error`) 映射退出码 2（OA 拉取失败）。
//!
//! 与源 `crates/common/src/runtime/mod.rs` 的「确定性失败可见」思路同构：
//! 任一错误分类都不允许「静默重试 / 静默降级」，由 `client.rs` 显式 return。

use thiserror::Error;

/// OA 拉取层 7 类错误。
///
/// 退出码映射（见 `cli/error.rs`）：
/// - `NotConfigured` → 退出码 1（env 缺失）
/// - `Network` / `Auth` / `Server5xx` / `Client4xx` / `SchemaMissing` / `Pagination` → 退出码 2（OA 拉取失败）
#[derive(Debug, Error)]
pub enum OaPullError {
    /// 必需 env 变量未配置（`OA_API_BEARER` 或 `OA_QUERY_DSN` 任一缺失）。
    #[error("OA 配置缺失：{0}")]
    NotConfigured(String),

    /// 网络层错误（连接失败 / TLS 握手失败 / 超时 / reqwest::Error 中非 HTTP 状态码）。
    #[error("OA 网络错误：{0}")]
    Network(String),

    /// 鉴权失败：HTTP 401 / 403。
    #[error("OA 鉴权失败：HTTP {0}")]
    Auth(u16),

    /// OA 端 5xx 服务器错误（重试耗尽仍在 `retry.rs` 中处理）。
    #[error("OA 服务端 5xx：HTTP {0}")]
    Server5xx(u16),

    /// OA 端 4xx 客户端错误（立即失败，不重试）。
    #[error("OA 客户端 4xx：HTTP {0}")]
    Client4xx(u16),

    /// 响应 schema 缺失 / 字段不匹配（如 `events` 字段缺失 / `next_cursor` 类型错）。
    #[error("OA 响应 schema 不匹配：{0}")]
    SchemaMissing(String),

    /// 翻页超上限 / cursor 不一致（由 `pagination.rs` 抛出）。
    #[error("OA 翻页失败：{0}")]
    Pagination(String),
}

impl OaPullError {
    /// 是否需要重试：仅 5xx 与网络错误触发重试，4xx 立即失败。
    ///
    /// 在 `retry.rs`（task 15 落盘）中按此判定。
    pub fn is_retryable(&self) -> bool {
        matches!(self, OaPullError::Network(_) | OaPullError::Server5xx(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_retryable_distinguishes_5xx_from_4xx() {
        // 5xx / 网络错可重试
        assert!(OaPullError::Server5xx(503).is_retryable());
        assert!(OaPullError::Network("conn refused".into()).is_retryable());

        // 4xx / Auth / Schema / NotConfigured 不可重试
        assert!(!OaPullError::Client4xx(404).is_retryable());
        assert!(!OaPullError::Auth(401).is_retryable());
        assert!(!OaPullError::SchemaMissing("events".into()).is_retryable());
        assert!(!OaPullError::NotConfigured("OA_API_BEARER".into()).is_retryable());
        assert!(!OaPullError::Pagination("limit".into()).is_retryable());
    }

    #[test]
    fn error_display_contains_key_field() {
        // Display 必须包含关键字段（HTTP 状态码 / 错误字段名），便于日志与最终用户定位
        assert!(OaPullError::Auth(401).to_string().contains("401"));
        assert!(OaPullError::Server5xx(502).to_string().contains("502"));
        assert!(OaPullError::Client4xx(404).to_string().contains("404"));
        assert!(OaPullError::SchemaMissing("events".into())
            .to_string()
            .contains("events"));
        assert!(OaPullError::NotConfigured("OA_API_BEARER".into())
            .to_string()
            .contains("OA_API_BEARER"));
    }
}