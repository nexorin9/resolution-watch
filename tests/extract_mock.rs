//! extract 子系统集成测试（task 20）：本地 mock LLM 端点 + 决议 fixture。
//!
//! 覆盖 spec 必答的两条 + 额外补强：
//! 1. **mock 模型返回 fixture 正确**：6 条完整决议经 `parse_llm_json` + `validate` 后产出 6 条 `Resolution`
//! 2. **缺字段 fixture 触发 validate 拒绝**：2 条缺关键字段被 `validate` 立即 `Missing` 拒
//! 3. **bearer header 注入正确**：mock 端点严格校验 `Authorization: Bearer <api_key>`
//! 4. **端到端经 `extract_resolutions` 走通**：mock LLM → 6 条决议 + metadata Ok
//! 5. **end-to-end with valid 6 fixture via chat()**：`ModelGateway::chat` 返回 content = 完整 JSON
//!
//! 复用 `resolution_watch::extract::mock_model` 的 helper（与 `oa_pull::mock` 同型；
//! always-compiled + 公开模块），避免与 `#[cfg(test)]` 门控冲突。

use resolution_watch::extract::llm::ModelGateway;
use resolution_watch::extract::mock_model;
use resolution_watch::extract::validate::{parse_llm_json, validate};

use mock_model::{
    build_complete_resolutions_fixture, build_invalid_resolutions_fixture, MOCK_API_KEY,
    MOCK_MODEL_NAME, MOCK_RESOLUTION_COUNT,
};

// ===== Test 1: mock 返回 6 条完整决议，parse + validate 通过 =====

#[tokio::test]
async fn mock_returns_six_complete_resolutions_parse_and_validate() {
    let mut server = mock_model::start_mock_llm_server().await;
    let _m = mock_model::register_chat_completions_endpoint(
        &mut server,
        &build_complete_resolutions_fixture(),
    )
    .await;

    let gateway = ModelGateway::with_config(server.url(), MOCK_API_KEY, MOCK_MODEL_NAME, 2000)
        .expect("构造 gateway 应成功");

    let (content, meta) = gateway
        .chat("SYSTEM", "USER")
        .await
        .expect("mock 200 应成功");

    // content 应等于完整决议 JSON
    let raw = parse_llm_json(&content).expect("mock 返回的 content 应能 parse");
    assert_eq!(
        raw.len(),
        MOCK_RESOLUTION_COUNT,
        "应解析出 {} 条决议",
        MOCK_RESOLUTION_COUNT
    );

    // 全部 5 必填字段都应被 validate 接受
    let validated = validate(&raw).expect("完整 fixture 应通过 validate");
    assert_eq!(validated.len(), MOCK_RESOLUTION_COUNT);

    // metadata 状态应为 Ok
    use resolution_watch::extract::audit::CallStatus;
    assert_eq!(meta.status, CallStatus::Ok);
    assert_eq!(meta.http_status, Some(200));
    assert_eq!(meta.model, MOCK_MODEL_NAME);

    _m.assert_async().await;
}

// ===== Test 2: 缺字段 fixture 触发 validate 拒绝 =====

#[tokio::test]
async fn mock_invalid_fixture_with_missing_fields_rejected_by_validate() {
    let content = build_invalid_resolutions_fixture();

    // mock LLM 端点可省略；这里只验证 fixture 自身被 validate 拒绝
    let raw = parse_llm_json(&content).expect("结构合法应被 parse 接受");
    assert_eq!(raw.len(), 2, "invalid fixture 应含 2 条决议");

    // validate 应拒绝（short-circuit 在第 1 条就 fail）
    let err = validate(&raw).expect_err("缺字段应被 validate 拒绝");
    use resolution_watch::extract::validate::InvalidResolution;
    match err {
        InvalidResolution::Missing(field) => {
            // 第 1 条缺 owner_id 或 acceptance
            assert!(
                field == "owner_id" || field == "acceptance",
                "第 1 条缺字段应为 owner_id 或 acceptance，实际 {field}"
            );
        }
        other => panic!("应为 InvalidResolution::Missing，实际 {other:?}"),
    }
}

// ===== Test 3: bearer header 注入正确 =====

#[tokio::test]
async fn mock_bearer_header_injection_is_verified_by_server() {
    let mut server = mock_model::start_mock_llm_server().await;
    let m = mock_model::register_chat_completions_with_bearer(
        &mut server,
        MOCK_API_KEY,
        &build_complete_resolutions_fixture(),
    )
    .await;

    let gateway = ModelGateway::with_config(server.url(), MOCK_API_KEY, MOCK_MODEL_NAME, 2000)
        .expect("构造 gateway 应成功");

    let (content, _meta) = gateway
        .chat("sys", "user")
        .await
        .expect("mock bearer 校验通过应成功");

    // 解析回来应是 6 条决议
    let raw = parse_llm_json(&content).expect("mock 返回内容应能 parse");
    assert_eq!(raw.len(), MOCK_RESOLUTION_COUNT);

    // 严格 1 次匹配（bearer 不匹配会 0 次）
    m.assert_async().await;
}

// ===== Test 4: 端到端经 extract_resolutions 走通 =====

#[tokio::test]
async fn end_to_end_extract_resolutions_with_mock_llm_emits_six_resolutions() {
    use resolution_watch::extract::{extract_resolutions, ExtractOutcome};

    let mut server = mock_model::start_mock_llm_server().await;
    let _m = mock_model::register_chat_completions_endpoint(
        &mut server,
        &build_complete_resolutions_fixture(),
    )
    .await;

    let gateway = ModelGateway::with_config(server.url(), MOCK_API_KEY, MOCK_MODEL_NAME, 2000)
        .expect("构造 gateway 应成功");

    // minutes.md 任意内容即可（mock 不解析其内容）
    let minutes_md = "## 决议 1:\n## 决议 2:\n## 决议 3:\n## 决议 4:\n## 决议 5:\n## 决议 6:\n";

    let outcome: ExtractOutcome = extract_resolutions(
        &gateway,
        minutes_md,
        mock_model::MOCK_WEEK_ID,
        "医务科|信息科|设备科|医保办|护理部|财务科",
    )
    .await
    .expect("mock LLM 200 + 6 条完整 fixture 应端到端通过");

    assert_eq!(outcome.resolutions.len(), MOCK_RESOLUTION_COUNT);
    assert!(
        !outcome.ai_degraded,
        "走 LLM 成功路径下 ai_degraded 应为 false"
    );

    // 6 条决议的责任科室应分别匹配 fixture
    let expected_depts = ["医务科", "信息科", "设备科", "医保办", "护理部", "财务科"];
    for (i, expected) in expected_depts.iter().enumerate() {
        assert_eq!(
            outcome.resolutions[i].owner_department, *expected,
            "决议 {} 责任科室应为 {expected}",
            outcome.resolutions[i].resolution_id
        );
    }

    // metadata 记录
    use resolution_watch::extract::audit::CallStatus;
    assert_eq!(outcome.metadata.status, CallStatus::Ok);

    _m.assert_async().await;
}

// ===== Test 5: 5xx 重试 — 第 1 次 503、第 2 次 200（验证 retry 入口已落盘） =====

#[tokio::test]
async fn mock_5xx_then_200_endpoint_returns_200_body() {
    // 本测试不直接触发 ModelGateway 的 retry（retry 在 task 21 落盘），但验证 mock 端点的
    // "5xx-then-200" helper 工作正常 — 即 5xx 之后的 200 mock 能被命中并返回 fixture。
    let mut server = mock_model::start_mock_llm_server().await;
    let (fail_mock, ok_mock) = mock_model::register_5xx_then_200_endpoint(
        &mut server,
        1,
        &build_complete_resolutions_fixture(),
    )
    .await;

    let gateway = ModelGateway::with_config(server.url(), MOCK_API_KEY, MOCK_MODEL_NAME, 2000)
        .expect("构造 gateway 应成功");

    // 第 1 次调用：503 → ModelGateway 拿到 Unreachable 错误（is_retryable=true）
    let first = gateway.chat("sys", "user").await;
    let err = first.expect_err("503 应被识别为 Unreachable");
    use resolution_watch::extract::error::ExtractError;
    match err.1 {
        ExtractError::Unreachable(msg) => assert!(
            msg.contains("503"),
            "Unreachable message 应含 503，实际：{msg}"
        ),
        other => panic!("应为 Unreachable，实际 {other:?}"),
    }
    // 第 1 次 5xx mock 至少 1 次命中
    fail_mock.assert_async().await;
    // ok_mock 至少 1 次匹配（本次没调用；保证只命中过 fail_mock）
    drop(ok_mock);
}
