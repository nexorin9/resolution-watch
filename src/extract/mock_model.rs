//! extract 子系统本地 mock LLM（mockito server + fixture）。
//!
//! 提供本地测试替身，让集成测试在无真实 LLM 端点的情况下验证
//! `extract::llm::ModelGateway` 的全部主路径：
//! - `build_complete_resolutions_fixture()`：6 条决议 + 1 条带 `evidence_ref`，覆盖 5 必填字段
//! - `build_invalid_resolutions_fixture()`：2 条决议，缺关键字段，触发 `validate` 拒绝
//! - `wrap_as_chat_completion(content)`：把内层 JSON 包装为 OpenAI-compatible 响应
//! - `start_mock_llm_server()`：起一个 async mockito server
//! - `register_chat_completions_endpoint(server, content)`：注册 `POST /v1/chat/completions`
//! - `register_chat_completions_with_bearer(server, expected, content)`：严格 bearer 校验版
//!
//! 与 `oa_pull::mock` 同型：always-compiled 公开模块，集成测试通过
//! `use resolution_watch::extract::mock_model;` 直接使用。
//!
//! 源产品主路径同构：源 `crates/db_connection/src/lib.rs` 的多驱动 persistence
//! seed 抽象 — 本任务以「mock 端点可替身」思路复刻同一机制。
//!
//! 注意：本模块依赖 mockito；mockito 在 `[dependencies]` 中（与 `oa_pull::mock` 一致），
//! 单元测试 `mod tests` 与集成测试共享同一组 helper。

#![allow(dead_code)]

use mockito::{Matcher, Server, ServerGuard};

/// Mock 决议条目 ID 模板前缀（与 `oa_pull::mock::MOCK_RESOLUTION_PREFIX` 风格一致）。
pub const MOCK_RESOLUTION_PREFIX: &str = "#[MOCK-LLM]";

/// Mock 默认 ISO 周次。
pub const MOCK_WEEK_ID: &str = "2026-W41";

/// Mock 决议数量（≥6 覆盖 spec「至少 6 条」要求；含 1 条带 `evidence_ref`）。
pub const MOCK_RESOLUTION_COUNT: usize = 6;

/// Mock 模型名（注入到 chat completion 响应的 `model` 字段）。
pub const MOCK_MODEL_NAME: &str = "mock-llm-test-model";

/// Mock API key（用于 bearer 校验单测）。
pub const MOCK_API_KEY: &str = "mock-llm-test-api-key-7777";

/// 构造 6 条完整决议条目的 LLM 输出 JSON 字符串。
///
/// 字段说明：
/// - 6 条决议覆盖 6 个不同科室（医务/信息/设备/医保/护理/财务）
/// - 第 6 条决议带 `evidence_ref`（验证可选字段透传）
/// - `due_at` 全部合法 `YYYY-MM-DD`
///
/// 返回值是 `parse_llm_json` 能直接解析的 JSON 字符串（`{"resolutions": [...]}`）。
pub fn build_complete_resolutions_fixture() -> String {
    let resolutions = serde_json::json!({
        "resolutions": [
            {
                "resolution_id": "#[MOCK-LLM]01",
                "title": "骨科病区扩建工程立项",
                "owner_department": "医务科",
                "owner_id": "E10001",
                "due_at": "2026-10-31",
                "acceptance": "立项批复通过院务会签发"
            },
            {
                "resolution_id": "#[MOCK-LLM]02",
                "title": "信息科 HIS 接口升级",
                "owner_department": "信息科",
                "owner_id": "E10002",
                "due_at": "2026-11-15",
                "acceptance": "全院科室切换完成 7 天无故障"
            },
            {
                "resolution_id": "#[MOCK-LLM]03",
                "title": "CT 设备采购",
                "owner_department": "设备科",
                "owner_id": "E10003",
                "due_at": "2026-12-01",
                "acceptance": "招标完成并签订合同"
            },
            {
                "resolution_id": "#[MOCK-LLM]04",
                "title": "医保对账差异复核",
                "owner_department": "医保办",
                "owner_id": "E10004",
                "due_at": "2026-10-20",
                "acceptance": "差异清单签收并启动申诉"
            },
            {
                "resolution_id": "#[MOCK-LLM]05",
                "title": "护理部排班规则调整",
                "owner_department": "护理部",
                "owner_id": "E10005",
                "due_at": "2026-11-30",
                "acceptance": "新排班表全院落地"
            },
            {
                "resolution_id": "#[MOCK-LLM]06",
                "title": "财务科预算执行率分析",
                "owner_department": "财务科",
                "owner_id": "E10006",
                "due_at": "2026-12-15",
                "acceptance": "分析报告院长办公会汇报",
                "evidence_ref": "OA-FIN-2026-1015"
            }
        ]
    });
    resolutions.to_string()
}

/// 构造 2 条缺字段决议条目的 LLM 输出 JSON 字符串（用于触发 `validate` 拒绝）。
///
/// 字段说明：
/// - 第 1 条缺 `owner_id` + `acceptance`
/// - 第 2 条缺 `acceptance`
///
/// 返回值同样可被 `parse_llm_json` 直接解析（结构合法但字段不全）。
pub fn build_invalid_resolutions_fixture() -> String {
    let resolutions = serde_json::json!({
        "resolutions": [
            {
                "resolution_id": "#[MOCK-LLM]11",
                "title": "缺 owner_id 决议",
                "owner_department": "医务科",
                "due_at": "2026-10-31"
            },
            {
                "resolution_id": "#[MOCK-LLM]12",
                "title": "缺 acceptance 决议",
                "owner_department": "信息科",
                "owner_id": "E10010",
                "due_at": "2026-11-30"
            }
        ]
    });
    resolutions.to_string()
}

/// 把内层 JSON（`{"resolutions": [...]}`）包装为 OpenAI-compatible chat completions 响应。
///
/// 响应结构：
/// ```json
/// {
///   "id": "chatcmpl-mock-...",
///   "choices": [{"index": 0, "message": {"role": "assistant", "content": "<input>"}}],
///   "usage": {"prompt_tokens": ..., "completion_tokens": ..., "total_tokens": ...}
/// }
/// ```
///
/// `content` 字段必须是 `String`（OpenAI 协议要求 content 是 string，不是嵌套对象）。
pub fn wrap_as_chat_completion(content: &str) -> String {
    serde_json::json!({
        "id": format!("chatcmpl-mock-{}", chrono_like_id()),
        "object": "chat.completion",
        "created": 1_700_000_000_i64,
        "model": MOCK_MODEL_NAME,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 120,
            "completion_tokens": 60,
            "total_tokens": 180
        }
    })
    .to_string()
}

/// 简易 ID 派生（避免引入额外 dep；mock 端点 ID 唯一性无强约束）。
fn chrono_like_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

/// 起一个 async mockito server（用于 `tokio::test`）。
pub async fn start_mock_llm_server() -> ServerGuard {
    Server::new_async().await
}

/// 注册 `POST /v1/chat/completions` 端点（不校验 bearer / 任意 query）。
///
/// 返回的 `Mock` 句柄可由 caller `assert_async` 校验「至少 1 次被命中」。
pub async fn register_chat_completions_endpoint(
    server: &mut ServerGuard,
    content: &str,
) -> mockito::Mock {
    let body = wrap_as_chat_completion(content);
    server
        .mock("POST", "/v1/chat/completions")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .expect_at_least(1)
        .create_async()
        .await
}

/// 注册 `POST /v1/chat/completions` 端点（严格校验 bearer header）。
///
/// 用于验证 ModelGateway 出站时 `Authorization: Bearer <api_key>` 注入正确。
/// `expected_bearer` 是「api_key 的字面值」（不含 `Bearer ` 前缀）。
pub async fn register_chat_completions_with_bearer(
    server: &mut ServerGuard,
    expected_bearer: &str,
    content: &str,
) -> mockito::Mock {
    let body = wrap_as_chat_completion(content);
    let expected_header = format!("Bearer {expected_bearer}");
    server
        .mock("POST", "/v1/chat/completions")
        .match_query(Matcher::Any)
        .match_header("authorization", expected_header.as_str())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .expect_at_least(1)
        .create_async()
        .await
}

/// 注册「前 `fail_count` 次 5xx、之后 200」的端点（重试场景）。
///
/// 返回两个 mock 句柄：5xx mock（`expect_at_least(fail_count)`）+ 200 mock（`expect(1)`）。
/// 失败次数耗尽时 ModelGateway retry 上限 5 次由 `oa_pull::retry::fetch_with_retry` 同型
/// 实现接管；本任务为 task 21 retry policy 集成测试保留入口。
pub async fn register_5xx_then_200_endpoint(
    server: &mut ServerGuard,
    fail_count: usize,
    content: &str,
) -> (mockito::Mock, mockito::Mock) {
    let fail_mock = server
        .mock("POST", "/v1/chat/completions")
        .match_query(Matcher::Any)
        .with_status(503)
        .with_body("model unavailable")
        .expect_at_least(fail_count)
        .create_async()
        .await;

    let success_body = wrap_as_chat_completion(content);
    let success_mock = server
        .mock("POST", "/v1/chat/completions")
        .match_query(Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(success_body)
        .expect_at_least(1)
        .create_async()
        .await;

    (fail_mock, success_mock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::validate::{parse_llm_json, validate};

    #[test]
    fn complete_fixture_has_six_resolutions_with_one_evidence_ref() {
        let content = build_complete_resolutions_fixture();
        let raw = parse_llm_json(&content).expect("完整 fixture 应被 parse 接受");
        assert_eq!(raw.len(), 6);
        // 验证其中恰好 1 条带 evidence_ref
        let with_evidence: Vec<_> =
            raw.iter().filter(|r| r.evidence_ref.is_some()).collect();
        assert_eq!(with_evidence.len(), 1, "应恰好 1 条带 evidence_ref");
        // 全部 5 必填字段都应被 validate 接受
        let validated = validate(&raw).expect("完整 fixture 应通过 validate");
        assert_eq!(validated.len(), 6);
    }

    #[test]
    fn invalid_fixture_two_resolutions_both_missing_acceptance() {
        let content = build_invalid_resolutions_fixture();
        let raw = parse_llm_json(&content).expect("结构合法应被 parse 接受");
        assert_eq!(raw.len(), 2);
        // validate 应在第 1 条就失败（缺 owner_id + acceptance，short-circuit）
        let err = validate(&raw).expect_err("缺字段应被 validate 拒绝");
        assert!(matches!(err, crate::extract::validate::InvalidResolution::Missing(_)));
    }

    #[test]
    fn wrap_as_chat_completion_emits_choices_with_content_string() {
        let content = build_complete_resolutions_fixture();
        let body = wrap_as_chat_completion(&content);
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("应可解析");
        let choices = parsed.get("choices").expect("应含 choices");
        let arr = choices.as_array().expect("choices 必为 array");
        assert_eq!(arr.len(), 1);
        let msg = arr[0]
            .get("message")
            .expect("应含 message")
            .get("content")
            .expect("应含 content");
        // content 必须是 String（OpenAI 协议要求），嵌套对象会被 ModelGateway 视为空字符串
        assert!(msg.is_string(), "content 应为 string 类型");
        // 解析回去应等于原 content
        assert_eq!(msg.as_str().unwrap(), content);
    }

    #[test]
    fn mock_resolution_prefix_matches_mocks() {
        let content = build_complete_resolutions_fixture();
        assert!(content.contains(MOCK_RESOLUTION_PREFIX));
        assert!(content.contains("#[MOCK-LLM]01"));
        assert!(content.contains("#[MOCK-LLM]06"));
    }
}
