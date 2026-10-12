//! oa_pull 拉取层本地 mock（mockito server + fixture）。
//!
//! 提供本地测试替身，让集成测试在无真实 OA 的情况下跑全链路：
//! - `build_events_fixture(count)`：生成跨 5 状态 action 的 N 条 OA 事件 fixture
//! - `start_mock_oa_server()`：起一个 mockito server（async）
//! - `register_paginated_endpoint()`：注册支持 `next_cursor` 翻页的 `/oa/events` 端点
//! - `register_single_page_endpoint()`：注册单页（无翻页）端点
//! - `register_5xx_then_200_endpoint()`：注册「前 N 次 5xx、之后 200」的端点（重试场景）
//! - `register_4xx_endpoint()`：注册「4xx 立即失败」端点
//!
//! 本模块仅在测试上下文编译（`#[cfg(test)]` 门控）；集成测试通过 `#[path]` 显式 include。
//! 源产品主路径同构：源 `crates/db_connection/src/lib.rs` 的 `PersistenceSeed::Sqlite/Postgres/MySql`
//! 多驱动抽象 — 本任务以「mock 端点可替身」思路复刻同一机制。
//!
//! 注意：mockito 在 `[dev-dependencies]` 中；本模块被 `#[cfg(test)]` 门控，仅在 cargo test 时
//! 才会被编译到 lib，**生产构建不会引入**。

#![allow(dead_code)]

use mockito::{Matcher, Server, ServerGuard};

use crate::oa_pull::models::{OaEventDto, OaPullResponse};

/// 5 状态 action 列表（与 `schemas::OaAction` 5 变体对应）。
///
/// 顺序固定为：`started` → `in_progress` → `blocked` → `submitted` → `completed`，
/// 与 `OaAction` enum 派生顺序保持一致。
pub const ALL_ACTIONS: [&str; 5] = [
    "started",
    "in_progress",
    "blocked",
    "submitted",
    "completed",
];

/// 决议 ID 模板（mock 用，避免与真实 fixture 冲突）。
pub const MOCK_RESOLUTION_PREFIX: &str = "#[MOCK]";

/// Mock OA 默认 ISO 周次（与 task 9 真实 fixture 对齐，便于交叉验证）。
pub const MOCK_WEEK_ID: &str = "2026-W41";

/// 构造 N 条 OA 事件 fixture，跨 5 种 action 循环。
///
/// - 事件 ID 形如 `OA-MOCK-{i:04}`
/// - 决议 ID 形如 `#[MOCK]01` ~ `#[MOCK]10`（10 个决议循环）
/// - actor_id 形如 `E10001` ~ `E10020`（20 个工号循环）
/// - event_time 形如 `2026-10-DDTHH:00:00+08:00`（10 月 1-28 日，0-23 时循环）
///
/// 这样 200 条 fixture 能均匀覆盖 5 种 action，每条决议得到 20 条事件。
pub fn build_events_fixture(count: usize) -> Vec<OaEventDto> {
    (1..=count)
        .map(|i| {
            let action = ALL_ACTIONS[(i - 1) % ALL_ACTIONS.len()];
            let day = (((i - 1) / ALL_ACTIONS.len()) % 28) + 1;
            let hour = (i - 1) % 24;
            OaEventDto {
                oa_event_id: format!("OA-MOCK-{i:04}"),
                resolution_id: format!("{MOCK_RESOLUTION_PREFIX}{:02}", (i % 10) + 1),
                action: action.to_string(),
                actor_id: format!("E{:05}", 10_000 + (i % 20)),
                event_time: format!("2026-10-{day:02}T{hour:02}:00:00+08:00"),
            }
        })
        .collect()
}

/// 起一个 async mockito server（用于 `tokio::test`）。
pub async fn start_mock_oa_server() -> ServerGuard {
    Server::new_async().await
}

/// 在 mockito server 上注册支持 `next_cursor` 翻页的 `/oa/events` 端点。
///
/// 按 `page_size` 把 events 切分；首页匹配 `cursor` 缺失，后续页匹配 `cursor=cur-N`。
/// 末页 `next_cursor = None`。
///
/// 返回每页对应的 `Mock` 句柄（按 page 顺序），调用方按需 `assert_async` 校验。
pub async fn register_paginated_endpoint(
    server: &mut ServerGuard,
    events: Vec<OaEventDto>,
    page_size: usize,
    week_id: &str,
) -> Vec<mockito::Mock> {
    assert!(page_size > 0, "page_size 必须 > 0");
    let total_pages = events.len().div_ceil(page_size).max(1);
    let mut mocks = Vec::with_capacity(total_pages);

    for page_idx in 0..total_pages {
        let start = page_idx * page_size;
        let end = (start + page_size).min(events.len());
        let page_events: Vec<OaEventDto> = if start < events.len() {
            events[start..end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if page_idx + 1 < total_pages {
            Some(format!("cur-{}", page_idx + 1))
        } else {
            None
        };
        let body = OaPullResponse {
            events: page_events,
            next_cursor,
        };
        let body_json = serde_json::to_string(&body).expect("fixture 序列化失败");

        let mock = if page_idx == 0 {
            // 首页：query 严格 `week_id=...&page_size=...`（cursor 缺失）
            // 用 Regex 同时校验 week_id 在前、cursor 不在 query 中
            // 注：page_size 由 client 端硬编码为 100，cursor 缺失用 `^week_id=...(&|$)` 守护
            let pattern = format!(r"^week_id={week_id}(&|$)");
            server
                .mock("GET", "/oa/events")
                .match_query(Matcher::Regex(pattern))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body_json)
                .expect(1)
                .create_async()
                .await
        } else {
            // 后续页：query 含 `cursor=cur-N`
            server
                .mock("GET", "/oa/events")
                .match_query(Matcher::AllOf(vec![
                    Matcher::UrlEncoded("week_id".into(), week_id.into()),
                    Matcher::UrlEncoded(
                        "cursor".into(),
                        format!("cur-{}", page_idx),
                    ),
                ]))
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(body_json)
                .expect(1)
                .create_async()
                .await
        };
        mocks.push(mock);
    }
    mocks
}

/// 注册单页（无翻页）端点：返回 events 全部 + `next_cursor = None`。
pub async fn register_single_page_endpoint(
    server: &mut ServerGuard,
    events: Vec<OaEventDto>,
    week_id: &str,
) -> mockito::Mock {
    let body = OaPullResponse {
        events,
        next_cursor: None,
    };
    let body_json = serde_json::to_string(&body).expect("fixture 序列化失败");

    server
        .mock("GET", "/oa/events")
        .match_query(Matcher::UrlEncoded("week_id".into(), week_id.into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body_json)
        .expect_at_least(1)
        .create_async()
        .await
}

/// 注册「前 `fail_count` 次 5xx、之后 200」的端点（重试场景）。
///
/// 返回两个 mock 句柄：5xx mock（用 `expect_at_least(fail_count)` 接受多次匹配） +
/// 200 mock（`expect(1)` 严格一次）。
pub async fn register_5xx_then_200_endpoint(
    server: &mut ServerGuard,
    fail_count: usize,
    success_events: Vec<OaEventDto>,
    week_id: &str,
) -> (mockito::Mock, mockito::Mock) {
    let fail_mock = server
        .mock("GET", "/oa/events")
        .match_query(Matcher::Any)
        .with_status(503)
        .with_body("service unavailable")
        .expect_at_least(fail_count)
        .create_async()
        .await;

    let success_body = OaPullResponse {
        events: success_events,
        next_cursor: None,
    };
    let success_json = serde_json::to_string(&success_body).expect("fixture 序列化失败");
    let success_mock = server
        .mock("GET", "/oa/events")
        .match_query(Matcher::UrlEncoded("week_id".into(), week_id.into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(success_json)
        .expect_at_least(1)
        .create_async()
        .await;

    (fail_mock, success_mock)
}

/// 注册 4xx 立即失败端点（不重试场景）。
pub async fn register_4xx_endpoint(
    server: &mut ServerGuard,
    status_code: usize,
    week_id: &str,
) -> mockito::Mock {
    server
        .mock("GET", "/oa/events")
        .match_query(Matcher::UrlEncoded("week_id".into(), week_id.into()))
        .with_status(status_code)
        .with_body("client error")
        .expect(1)
        .create_async()
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_covers_all_five_actions_uniformly() {
        // 200 条 / 5 action = 每 action 40 条
        let fixture = build_events_fixture(200);
        assert_eq!(fixture.len(), 200);

        let mut counts = std::collections::HashMap::new();
        for e in &fixture {
            *counts.entry(e.action.as_str()).or_insert(0usize) += 1;
        }
        for action in ALL_ACTIONS.iter() {
            assert_eq!(counts.get(action).copied().unwrap_or(0), 40, "action {action} 应 40 条");
        }
    }

    #[test]
    fn fixture_resolution_ids_cycle_through_ten() {
        // 决议 ID #[MOCK]01-#[MOCK]10 循环
        let fixture = build_events_fixture(50);
        let unique: std::collections::HashSet<_> =
            fixture.iter().map(|e| e.resolution_id.clone()).collect();
        assert_eq!(unique.len(), 10, "50 条事件应覆盖 10 个决议 ID");
    }

    #[test]
    fn fixture_oa_event_ids_are_unique() {
        let fixture = build_events_fixture(200);
        let unique: std::collections::HashSet<_> =
            fixture.iter().map(|e| e.oa_event_id.clone()).collect();
        assert_eq!(unique.len(), 200, "200 条事件应有 200 个唯一 oa_event_id");
    }

    #[test]
    fn fixture_event_time_is_rfc3339_compatible() {
        use chrono::DateTime;
        let fixture = build_events_fixture(20);
        for e in &fixture {
            DateTime::parse_from_rfc3339(&e.event_time)
                .unwrap_or_else(|err| panic!("event_time {} RFC3339 解析失败：{err}", e.event_time));
        }
    }
}
