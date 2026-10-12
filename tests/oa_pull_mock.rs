//! oa_pull 集成测试（task 17）：5 个单测覆盖 mock 服务端的关键路径。
//!
//! 本测试文件覆盖 task 17 要求的 5 个核心行为：
//! 1. **bearer 注入正确**：Authorization: Bearer <token> 出站被 mock 端点验证
//! 2. **5xx 重试 3 次后成功**：mock 前 2 次 503、第 3 次 200，验证 retry 链路
//! 3. **4xx 立即失败**：mock 404 严格 1 次调用，验证不重试
//! 4. **翻页 cursor 拼接**：mock 三页拼接，验证 next_cursor 链路
//! 5. **端到端 200 条**：mock 200 条 OA 事件翻页拉取，验证全量返回 + 5 action 覆盖
//!
//! 复用 `resolution_watch::oa_pull::mock` 的 helper（统一在 `src/oa_pull/mock.rs` 维护），
//! 避免与 `#[cfg(test)]` 门控冲突。
//!
//! 源产品主路径同构：源 `crates/db_connection/src/lib.rs` 的多驱动 persistence seed
//! 抽象 — 本任务以「mock 端点可替身」思路复刻同一机制。

use mockito::Matcher;

use resolution_watch::oa_pull::client::OaClient;
use resolution_watch::oa_pull::error::OaPullError;
use resolution_watch::oa_pull::mock;
use resolution_watch::oa_pull::models::OaPullRequest;
use resolution_watch::oa_pull::pagination;
use resolution_watch::oa_pull::retry::fetch_with_retry;

use mock::{ALL_ACTIONS, MOCK_WEEK_ID};

// ===== Test 1: bearer 注入正确 =====

#[tokio::test]
async fn mock_bearer_header_injection_is_verified_by_server() {
    let mut server = mock::start_mock_oa_server().await;
    // mock 端点严格校验 Authorization: Bearer <token>
    let m = server
        .mock("GET", "/oa/events")
        .match_query(Matcher::UrlEncoded("week_id".into(), MOCK_WEEK_ID.into()))
        .match_header("authorization", "Bearer test-mock-bearer-99999")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"events":[],"next_cursor":null}"#)
        .create_async()
        .await;

    let client = OaClient::with_credentials("test-mock-bearer-99999", server.url()).unwrap();
    let req = OaPullRequest::first(MOCK_WEEK_ID);
    let resp = client.fetch_page(&req).await.expect("2xx 应成功");
    assert!(resp.is_last_page());
    assert_eq!(resp.events.len(), 0);

    m.assert_async().await; // 严格 1 次匹配（bearer 不匹配会 0 次）
}

// ===== Test 2: 5xx 重试 3 次后成功 =====

#[tokio::test]
async fn mock_retry_5xx_succeeds_on_third_attempt() {
    // mock 前 2 次 503、第 3 次 200 → 验证 retry 链路在第 3 次成功
    let mut server = mock::start_mock_oa_server().await;
    let (fail_mock, ok_mock) = mock::register_5xx_then_200_endpoint(
        &mut server,
        2,
        vec![mock::build_events_fixture(1)[0].clone()],
        MOCK_WEEK_ID,
    )
    .await;

    let url = server.url();
    let result = fetch_with_retry(|| {
        let url = url.clone();
        async move {
            let c = OaClient::with_credentials("bearer-mock-retry", url).unwrap();
            let req = OaPullRequest::first(MOCK_WEEK_ID);
            c.fetch_page(&req).await
        }
    })
    .await;

    let resp = result.expect("第 3 次 200 应成功");
    assert_eq!(resp.events.len(), 1);
    assert_eq!(resp.events[0].oa_event_id, "OA-MOCK-0001");
    assert!(resp.is_last_page());

    fail_mock.assert_async().await; // 至少匹配 2 次 5xx
    ok_mock.assert_async().await; // 至少匹配 1 次 200
}

// ===== Test 3: 4xx 立即失败不重试 =====

#[tokio::test]
async fn mock_4xx_returns_immediately_without_retry() {
    // mock 404 严格 1 次调用 → 验证不重试
    let mut server = mock::start_mock_oa_server().await;
    let m = mock::register_4xx_endpoint(&mut server, 404, MOCK_WEEK_ID).await;

    let url = server.url();
    let result = fetch_with_retry(|| {
        let url = url.clone();
        async move {
            let c = OaClient::with_credentials("bearer-mock-4xx", url).unwrap();
            let req = OaPullRequest::first(MOCK_WEEK_ID);
            c.fetch_page(&req).await
        }
    })
    .await;

    assert!(result.is_err(), "404 应返回 Err");
    match result.unwrap_err() {
        OaPullError::Client4xx(404) => {}
        other => panic!("期望 Client4xx(404)，实际 {other:?}"),
    }
    m.assert_async().await; // 严格 1 次（重试 1 次以上会失败）
}

// ===== Test 4: 翻页 cursor 拼接 =====

#[tokio::test]
async fn mock_pagination_cursor_stitches_three_pages() {
    // 3 页：第 1 页 5 条 + next_cursor=cur-1；第 2 页 3 条 + next_cursor=cur-2；第 3 页 2 条 + next_cursor=null
    // 直接调 helper（生成 10 条事件后切 5/3/2）
    let mut server = mock::start_mock_oa_server().await;
    let all = mock::build_events_fixture(10);
    let page1: Vec<_> = all[0..5].to_vec();
    let page2: Vec<_> = all[5..8].to_vec();
    let page3: Vec<_> = all[8..10].to_vec();

    // 手工构造 3 页（用 helper 但要 hack next_cursor，所以直接调 client 层）
    use resolution_watch::oa_pull::models::OaPullResponse;
    let m1 = server
        .mock("GET", "/oa/events")
        .match_query(Matcher::Regex(format!(r"^week_id={MOCK_WEEK_ID}(&|$)")))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::to_string(&OaPullResponse {
                events: page1.clone(),
                next_cursor: Some("cur-1".to_string()),
            })
            .unwrap(),
        )
        .expect(1)
        .create_async()
        .await;
    let m2 = server
        .mock("GET", "/oa/events")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("week_id".into(), MOCK_WEEK_ID.into()),
            Matcher::UrlEncoded("cursor".into(), "cur-1".to_string()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::to_string(&OaPullResponse {
                events: page2.clone(),
                next_cursor: Some("cur-2".to_string()),
            })
            .unwrap(),
        )
        .expect(1)
        .create_async()
        .await;
    let m3 = server
        .mock("GET", "/oa/events")
        .match_query(Matcher::AllOf(vec![
            Matcher::UrlEncoded("week_id".into(), MOCK_WEEK_ID.into()),
            Matcher::UrlEncoded("cursor".into(), "cur-2".to_string()),
        ]))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::to_string(&OaPullResponse {
                events: page3.clone(),
                next_cursor: None,
            })
            .unwrap(),
        )
        .expect(1)
        .create_async()
        .await;

    let url = server.url();
    let client = OaClient::with_credentials("bearer-mock-pag", url).unwrap();
    let events = pagination::fetch_events_paginated(&client, MOCK_WEEK_ID)
        .await
        .expect("3 页翻页应成功");

    // 验证：5 + 3 + 2 = 10 条
    assert_eq!(events.len(), 10);
    assert_eq!(events[0].oa_event_id, page1[0].oa_event_id);
    assert_eq!(events[4].oa_event_id, page1[4].oa_event_id);
    assert_eq!(events[5].oa_event_id, page2[0].oa_event_id);
    assert_eq!(events[7].oa_event_id, page2[2].oa_event_id);
    assert_eq!(events[8].oa_event_id, page3[0].oa_event_id);
    assert_eq!(events[9].oa_event_id, page3[1].oa_event_id);

    m1.assert_async().await;
    m2.assert_async().await;
    m3.assert_async().await;
}

// ===== Test 5: 端到端 200 条 OA 事件全量拉取 =====

#[tokio::test]
async fn mock_oa_end_to_end_returns_full_200_events_across_5_actions() {
    // 200 条事件 / page_size=50 → 4 页翻页
    let mut server = mock::start_mock_oa_server().await;
    let events = mock::build_events_fixture(200);
    let mocks = mock::register_paginated_endpoint(&mut server, events, 50, MOCK_WEEK_ID).await;
    assert_eq!(mocks.len(), 4, "200 / 50 = 4 页");

    let url = server.url();
    let client = OaClient::with_credentials("bearer-mock-e2e", url).unwrap();
    let pulled = pagination::fetch_events_paginated(&client, MOCK_WEEK_ID)
        .await
        .expect("端到端 200 条拉取应成功");

    // 1. 全量：200 条
    assert_eq!(pulled.len(), 200, "应拉取 200 条事件");

    // 2. 唯一 oa_event_id：200 个
    let unique_ids: std::collections::HashSet<_> =
        pulled.iter().map(|e| e.oa_event_id.clone()).collect();
    assert_eq!(unique_ids.len(), 200, "应 200 个唯一 oa_event_id");

    // 3. 5 种 action 各 40 条
    let mut action_counts = std::collections::HashMap::new();
    for e in &pulled {
        *action_counts.entry(format!("{:?}", e.action)).or_insert(0usize) += 1;
    }
    for action in ALL_ACTIONS.iter() {
        // OaAction debug 输出（snake_case 等价）
        let debug_name = match *action {
            "started" => "Started",
            "in_progress" => "InProgress",
            "blocked" => "Blocked",
            "submitted" => "Submitted",
            "completed" => "Completed",
            _ => unreachable!(),
        };
        assert_eq!(
            action_counts.get(debug_name).copied().unwrap_or(0),
            40,
            "action {action} 应 40 条"
        );
    }

    // 4. 决议 ID 覆盖 10 个唯一
    let unique_resolutions: std::collections::HashSet<_> =
        pulled.iter().map(|e| e.resolution_id.clone()).collect();
    assert_eq!(unique_resolutions.len(), 10, "10 个决议 ID 循环");

    // 5. 4 个翻页 mock 全部命中
    for m in mocks {
        m.assert_async().await;
    }
}

// ===== Bonus: 单页端点 helper 验证（辅助用） =====

#[tokio::test]
async fn mock_single_page_endpoint_returns_all_events() {
    // 辅助测试：验证 register_single_page_endpoint helper 工作正常
    let mut server = mock::start_mock_oa_server().await;
    let events = mock::build_events_fixture(15);
    let m = mock::register_single_page_endpoint(&mut server, events, MOCK_WEEK_ID).await;

    let url = server.url();
    let client = OaClient::with_credentials("bearer-single", url).unwrap();
    let resp = client
        .fetch_page(&OaPullRequest::first(MOCK_WEEK_ID))
        .await
        .expect("单页应成功");
    assert_eq!(resp.events.len(), 15);
    assert!(resp.is_last_page());

    m.assert_async().await;
}
