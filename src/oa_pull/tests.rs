//! OA 出站层单测。
//!
//! 覆盖 task 3 要求的 3 个核心行为：
//! 1. **bearer header 注入正确**：reqwest Authorization: Bearer <token> 出站
//! 2. **env 缺失返回 NotConfigured**：bearer / DSN 任一缺失立即报错
//! 3. **4xx 立即返回 Client4xx**：不重试、不降级、显式归类
//!
//! 额外覆盖：
//! - 401/403 → Auth 错误分类（区别于通用 4xx，便于上游退出码映射）
//! - 2xx 空 events 数组合法（周内无流转事件）
//! - task 15 重试策略：5xx 重试 3 次后成功 / 4xx 立即失败不重试 / 重试耗尽返回 Server5xx

use mockito::Server;

use super::client::{OaClient, ENV_BEARER, ENV_DSN};
use super::error::OaPullError;
use super::models::OaPullRequest;
use super::retry::fetch_with_retry;

#[tokio::test]
async fn bearer_header_injection_correct() {
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/oa/events")
        .match_header("authorization", "Bearer test-bearer-12345")
        .match_query(mockito::Matcher::AnyOf(vec![mockito::Matcher::UrlEncoded(
            "week_id".into(),
            "2026-W41".into(),
        )]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"events":[],"next_cursor":null}"#)
        .create_async()
        .await;

    let client = OaClient::with_credentials("test-bearer-12345", server.url()).unwrap();
    assert_eq!(client.bearer(), "test-bearer-12345");

    let req = OaPullRequest::first("2026-W41");
    let resp = client
        .fetch_page(&req)
        .await
        .expect("2xx 应解析成功");
    assert!(resp.is_last_page());
    assert_eq!(resp.events.len(), 0);

    m.assert_async().await;
}

#[test]
fn from_env_missing_bearer_returns_not_configured() {
    let err = OaClient::with_credentials("", "https://oa.example.com").unwrap_err();
    match err {
        OaPullError::NotConfigured(field) => {
            assert_eq!(field, ENV_BEARER);
        }
        other => panic!("期望 NotConfigured，实际 {other:?}"),
    }
}

#[test]
fn from_env_missing_dsn_returns_not_configured() {
    let err = OaClient::with_credentials("valid-bearer", "").unwrap_err();
    match err {
        OaPullError::NotConfigured(field) => {
            assert_eq!(field, ENV_DSN);
        }
        other => panic!("期望 NotConfigured，实际 {other:?}"),
    }
}

#[tokio::test]
async fn four_xx_returns_client_4xx_immediately() {
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", mockito::Matcher::Any)
        .match_query(mockito::Matcher::Any)
        .with_status(404)
        .with_body("not found")
        .create_async()
        .await;

    let client = OaClient::with_credentials("bearer-x", server.url()).unwrap();
    let req = OaPullRequest::first("2026-W41");
    let err = client
        .fetch_page(&req)
        .await
        .expect_err("4xx 应立即失败");
    match err {
        OaPullError::Client4xx(404) => {}
        other => panic!("期望 Client4xx(404)，实际 {other:?}"),
    }

    m.assert_async().await;
}

#[tokio::test]
async fn four_01_returns_auth_error() {
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", mockito::Matcher::Any)
        .match_query(mockito::Matcher::Any)
        .with_status(401)
        .with_body("unauthorized")
        .create_async()
        .await;

    let client = OaClient::with_credentials("bad-bearer", server.url()).unwrap();
    let req = OaPullRequest::first("2026-W41");
    let err = client.fetch_page(&req).await.expect_err("401 应返回 Auth");
    match err {
        OaPullError::Auth(401) => {}
        other => panic!("期望 Auth(401)，实际 {other:?}"),
    }

    m.assert_async().await;
}

#[tokio::test]
async fn two_xx_with_empty_events_is_legal() {
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"events":[],"next_cursor":null}"#)
        .create_async()
        .await;

    let client = OaClient::with_credentials("bearer-y", server.url()).unwrap();
    let req = OaPullRequest::first("2026-W41");
    let resp = client.fetch_page(&req).await.expect("空 events 应合法");
    assert_eq!(resp.events.len(), 0);
    assert!(resp.is_last_page());

    m.assert_async().await;
}

// ----- task 16: next_cursor 翻页 -----

#[tokio::test]
async fn pagination_cursors_stitch_three_pages() {
    // 3 页拼接：第 1 页无 cursor + next_cursor="cur1"；第 2 页 cursor="cur1" + next_cursor="cur2"；第 3 页 cursor="cur2" + next_cursor=null
    let mut server = Server::new_async().await;

    // 第 1 页（无 cursor）：query 严格等于 `week_id=2026-W41&page_size=100`（不出现 cursor=）
    let m1 = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Regex(
            r"^week_id=2026-W41&page_size=100$".to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"events":[{"oa_event_id":"OA-001","resolution_id":"R1","action":"started","actor_id":"E1","event_time":"2026-10-12T09:00:00+08:00"},{"oa_event_id":"OA-002","resolution_id":"R1","action":"in_progress","actor_id":"E1","event_time":"2026-10-12T10:00:00+08:00"}],"next_cursor":"cur1"}"#,
        )
        .expect(1)
        .create_async()
        .await;

    // 第 2 页（cursor=cur1）
    let m2 = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("week_id".into(), "2026-W41".into()),
            mockito::Matcher::UrlEncoded("page_size".into(), "100".into()),
            mockito::Matcher::UrlEncoded("cursor".into(), "cur1".into()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"events":[{"oa_event_id":"OA-003","resolution_id":"R2","action":"submitted","actor_id":"E2","event_time":"2026-10-13T09:00:00+08:00"}],"next_cursor":"cur2"}"#,
        )
        .expect(1)
        .create_async()
        .await;

    // 第 3 页（cursor=cur2，next_cursor=null 终止）
    let m3 = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::AllOf(vec![
            mockito::Matcher::UrlEncoded("week_id".into(), "2026-W41".into()),
            mockito::Matcher::UrlEncoded("page_size".into(), "100".into()),
            mockito::Matcher::UrlEncoded("cursor".into(), "cur2".into()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"events":[{"oa_event_id":"OA-004","resolution_id":"R3","action":"completed","actor_id":"E3","event_time":"2026-10-14T17:00:00+08:00"}],"next_cursor":null}"#,
        )
        .expect(1)
        .create_async()
        .await;

    let url = server.url();
    let client = OaClient::with_credentials("bearer-pag", url).unwrap();
    let events = super::pagination::fetch_events_paginated(&client, "2026-W41")
        .await
        .expect("3 页翻页应成功");

    // 4 条事件按时间序拼接（2 + 1 + 1）
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].oa_event_id, "OA-001");
    assert_eq!(events[1].oa_event_id, "OA-002");
    assert_eq!(events[2].oa_event_id, "OA-003");
    assert_eq!(events[3].oa_event_id, "OA-004");

    m1.assert_async().await;
    m2.assert_async().await;
    m3.assert_async().await;
}

#[tokio::test]
async fn pagination_caps_at_max_pages_preventing_infinite_loop() {
    // 故意让 mockito 永远返回非空 next_cursor → 验证 50 页后立即触发 Pagination 错误，不无限循环
    let mut server = Server::new_async().await;

    // 注意：mockito 不便按 cursor 区分（cursor 内容每次随机），用 match_query(Matcher::Any) 匹配任意 query
    let m = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"events":[{"oa_event_id":"OA-X","resolution_id":"RX","action":"started","actor_id":"EX","event_time":"2026-10-12T09:00:00+08:00"}],"next_cursor":"never-null"}"#,
        )
        .expect_at_least(super::pagination::MAX_PAGES as usize)
        .create_async()
        .await;

    let url = server.url();
    let client = OaClient::with_credentials("bearer-loop", url).unwrap();
    let started = std::time::Instant::now();
    let err = super::pagination::fetch_events_paginated(&client, "2026-W41")
        .await
        .expect_err("50 页后应触发翻页上限错误");
    let elapsed = started.elapsed();

    match err {
        OaPullError::Pagination(msg) => {
            assert!(
                msg.contains("50") || msg.contains(&super::pagination::MAX_PAGES.to_string()),
                "错误信息应含 50 / MAX_PAGES：{msg}"
            );
        }
        other => panic!("期望 OaPullError::Pagination，实际 {other:?}"),
    }

    // 必须立刻报错（不允许 50 页以上继续）；不验证 mockito 实际调用次数（受 retry 等影响），
    // 但 elapsed 必须远小于 5 分钟（任意 50 页以上都会触发）
    assert!(
        elapsed < std::time::Duration::from_secs(120),
        "翻页应在 120s 内终止，实测 {elapsed:?}"
    );

    m.assert_async().await;
}

#[tokio::test]
async fn pagination_terminates_when_first_page_has_no_cursor() {
    // 边界守护：首页即 next_cursor=null → 1 页终止
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"events":[{"oa_event_id":"OA-001","resolution_id":"R1","action":"started","actor_id":"E1","event_time":"2026-10-12T09:00:00+08:00"}],"next_cursor":null}"#,
        )
        .expect(1) // 严格 1 次
        .create_async()
        .await;

    let url = server.url();
    let client = OaClient::with_credentials("bearer-once", url).unwrap();
    let events = super::pagination::fetch_events_paginated(&client, "2026-W41")
        .await
        .expect("1 页直接终止应成功");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].oa_event_id, "OA-001");
    m.assert_async().await; // 严格 1 次，2 次以上会失败
}

// ----- task 15: 重试策略集成测试 -----

#[tokio::test]
async fn retry_five_xx_succeeds_on_third_attempt() {
    // 第一次 + 第二次 503，第三次 200 → 应在第 3 次成功后返回
    let mut server = Server::new_async().await;

    // 5xx 计数 1 次；用 .expect_at_least 允许 mockito 匹配多个 mock
    let m5 = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(503)
        .with_body("service unavailable")
        .expect_at_least(2)
        .create_async()
        .await;
    let m_ok = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"events":[],"next_cursor":null}"#)
        .expect(1)
        .create_async()
        .await;

    let url = server.url();
    let result = fetch_with_retry(|| {
        let url = url.clone();
        async move {
            let c = OaClient::with_credentials("bearer-r", url).unwrap();
            let req = OaPullRequest::first("2026-W41");
            c.fetch_page(&req).await
        }
    })
    .await;

    assert!(result.is_ok(), "第 3 次 200 应成功: {:?}", result.err());
    let resp = result.unwrap();
    assert!(resp.is_last_page());
    assert_eq!(resp.events.len(), 0);

    m5.assert_async().await;
    m_ok.assert_async().await;
}

#[tokio::test]
async fn retry_four_xx_returns_immediately_without_retry() {
    // 404 应立即返回，不重试；只调用 1 次
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(404)
        .with_body("not found")
        .expect(1) // 只许 1 次调用
        .create_async()
        .await;

    let url = server.url();
    let result = fetch_with_retry(|| {
        let url = url.clone();
        async move {
            let c = OaClient::with_credentials("bearer-4xx", url).unwrap();
            let req = OaPullRequest::first("2026-W41");
            c.fetch_page(&req).await
        }
    })
    .await;

    assert!(result.is_err(), "404 应返回 Err");
    match result.unwrap_err() {
        OaPullError::Client4xx(404) => {}
        other => panic!("期望 Client4xx(404)，实际 {other:?}"),
    }
    m.assert_async().await; // 严格 1 次调用，2 次以上会失败
}

#[tokio::test]
async fn retry_exhausted_returns_server_5xx_after_max_attempts() {
    // 一直 503，5 次后耗尽 → 返回 Server5xx
    let mut server = Server::new_async().await;
    let m = server
        .mock("GET", "/oa/events")
        .match_query(mockito::Matcher::Any)
        .with_status(503)
        .with_body("permanently down")
        .expect_at_least(5) // 至少 5 次
        .create_async()
        .await;

    let url = server.url();
    let result = fetch_with_retry(|| {
        let url = url.clone();
        async move {
            let c = OaClient::with_credentials("bearer-503", url).unwrap();
            let req = OaPullRequest::first("2026-W41");
            c.fetch_page(&req).await
        }
    })
    .await;

    assert!(result.is_err(), "重试耗尽应返回 Err");
    match result.unwrap_err() {
        OaPullError::Server5xx(503) => {}
        other => panic!("期望 Server5xx(503)，实际 {other:?}"),
    }
    m.assert_async().await;
}

#[tokio::test]
async fn retry_network_error_is_retryable() {
    // 不可达 URL → Network → 5 次后耗尽（5xx/Network 都是 retryable）
    let result = fetch_with_retry(|| async {
        // 故意连一个不会监听的高位端口，立即 ECONNREFUSED
        let c = OaClient::with_credentials("bearer-net", "http://127.0.0.1:1").unwrap();
        let req = OaPullRequest::first("2026-W41");
        c.fetch_page(&req).await
    })
    .await;

    assert!(result.is_err(), "网络错误应返回 Err");
    match result.unwrap_err() {
        OaPullError::Network(_) => {}
        other => panic!("期望 Network，实际 {other:?}"),
    }
}