//! OA REST/SQL 拉取层（bearer 鉴权 + 重试 + 翻页 + mock）。
//!
//! 模块组成：
//! - `client.rs`    — reqwest Client + TLS + bearer header 注入 + env 读取
//! - `models.rs`    — OaPullRequest / OaPullResponse / OaEventDto + 与 schemas::OaEvent 互转
//! - `error.rs`     — OaPullError 7 类错误
//! - `retry.rs`     — 指数退避（base 500ms / cap 8s / max 5 次）
//! - `pagination.rs` — next_cursor 翻页（单页 100 / 最多 50 页）

pub mod client;
pub mod error;
pub mod mock;
pub mod models;
pub mod pagination;
pub mod retry;

#[cfg(test)]
mod tests;

pub use client::{OaClient, DEFAULT_TIMEOUT_MS, ENV_BEARER, ENV_DSN};
pub use error::OaPullError;
pub use models::{from_dto, from_dtos, OaEventDto, OaPullRequest, OaPullResponse};
pub use pagination::{
    fetch_events_paginated, fetch_one_page, take_page, DEFAULT_PAGE_SIZE, MAX_PAGES,
};
pub use retry::{backoff, fetch_page_with_retry, fetch_with_retry};

// ---------- 顶层入口 ----------

/// 拉取指定 ISO 周的全部 OA 流转事件（自动翻页 + 重试）。
///
/// 委托 `pagination::fetch_events_paginated`，便于上层一次调用拿到全量事件。
pub async fn fetch_events(
    client: &OaClient,
    week_id: &str,
) -> Result<Vec<crate::schemas::OaEvent>, OaPullError> {
    pagination::fetch_events_paginated(client, week_id).await
}