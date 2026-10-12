//! OA REST/SQL 拉取的内部 DTO。
//!
//! - `OaPullRequest`：发往 OA 的查询请求（week_id + cursor + page_size）
//! - `OaPullResponse`：OA 返回的分页响应（events + next_cursor）
//! - `from_dto` / `to_dto`：`schemas::OaEvent` ↔ `OaEventDto` 互转
//!
//! 字段命名遵循 `docs/integration.md` 的字段映射（oa_event_id / resolution_id / action /
//! actor_id / event_time / status 6 字段）。

use serde::{Deserialize, Serialize};

use crate::schemas::OaEvent;

/// OA 出站请求 DTO。
///
/// `week_id` 由 ISO 周编号（`2026-W41`）派生，`page_size` 默认 100，最大 500。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OaPullRequest {
    /// ISO 周编号（如 `2026-W41`）。
    pub week_id: String,
    /// 翻页 cursor（首次请求为 `None`）。
    pub cursor: Option<String>,
    /// 单页大小，1..=500。
    pub page_size: u32,
}

impl OaPullRequest {
    /// 构造首次请求（无 cursor，单页 100）。
    pub fn first(week_id: impl Into<String>) -> Self {
        Self {
            week_id: week_id.into(),
            cursor: None,
            page_size: 100,
        }
    }

    /// 构造后续翻页请求。
    pub fn next(week_id: impl Into<String>, cursor: impl Into<String>, page_size: u32) -> Self {
        Self {
            week_id: week_id.into(),
            cursor: Some(cursor.into()),
            page_size: page_size.min(500).max(1),
        }
    }
}

/// OA 返回的原始事件 DTO（与院内 OA 字段名完全对齐，便于 `docs/integration.md` 字段映射）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OaEventDto {
    /// OA 端唯一事件 id。
    pub oa_event_id: String,
    /// 关联的决议编号。
    pub resolution_id: String,
    /// 事件动作（snake_case 字符串：`started` / `in_progress` / `blocked` / `submitted` / `completed`）。
    pub action: String,
    /// 触发人 / 工号。
    pub actor_id: String,
    /// 事件时间戳，RFC3339 字符串。
    pub event_time: String,
}

/// OA 分页响应 DTO。
///
/// `next_cursor` 为 `None` 表示已翻到最后一页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OaPullResponse {
    /// 当前页事件列表。
    pub events: Vec<OaEventDto>,
    /// 下一页 cursor，`None` 表示已结束。
    #[serde(default)]
    pub next_cursor: Option<String>,
}

impl OaPullResponse {
    /// 是否最后一页。
    pub fn is_last_page(&self) -> bool {
        self.next_cursor.is_none()
    }
}

/// `OaEventDto` → `schemas::OaEvent` 转换（带 action 字符串 → `OaAction` 枚举解析）。
///
/// 解析失败返回 `OaPullError::SchemaMissing`。
pub fn from_dto(dto: OaEventDto) -> Result<OaEvent, String> {
    use crate::schemas::OaAction;
    let action = match dto.action.as_str() {
        "started" => OaAction::Started,
        "in_progress" => OaAction::InProgress,
        "blocked" => OaAction::Blocked,
        "submitted" => OaAction::Submitted,
        "completed" => OaAction::Completed,
        other => return Err(format!("unknown action: {other}")),
    };
    let event_time = chrono::DateTime::parse_from_rfc3339(&dto.event_time)
        .map_err(|e| format!("event_time RFC3339 解析失败：{e}"))?
        .with_timezone(&chrono::Utc);
    Ok(OaEvent {
        oa_event_id: dto.oa_event_id,
        resolution_id: dto.resolution_id,
        action,
        actor_id: dto.actor_id,
        event_time,
    })
}

/// 批量转换（任一失败立即返回第一个错误，便于调用方排查）。
pub fn from_dtos(dtos: &[OaEventDto]) -> Result<Vec<OaEvent>, String> {
    let mut out = Vec::with_capacity(dtos.len());
    for d in dtos {
        out.push(from_dto(d.clone())?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_defaults_to_page_size_100() {
        let r = OaPullRequest::first("2026-W41");
        assert_eq!(r.week_id, "2026-W41");
        assert!(r.cursor.is_none());
        assert_eq!(r.page_size, 100);
    }

    #[test]
    fn next_request_caps_page_size_to_500() {
        let r = OaPullRequest::next("2026-W41", "abc", 9999);
        assert_eq!(r.page_size, 500);
        assert_eq!(r.cursor.as_deref(), Some("abc"));
    }

    #[test]
    fn from_dto_parses_known_actions() {
        let dto = OaEventDto {
            oa_event_id: "OA-001".to_string(),
            resolution_id: "#[W41]01".to_string(),
            action: "started".to_string(),
            actor_id: "E10086".to_string(),
            event_time: "2026-10-12T09:30:00+08:00".to_string(),
        };
        let evt = from_dto(dto).expect("known action 应解析");
        assert_eq!(evt.oa_event_id, "OA-001");
        assert_eq!(evt.resolution_id, "#[W41]01");
        assert_eq!(evt.actor_id, "E10086");
        assert_eq!(evt.action, crate::schemas::OaAction::Started);
    }

    #[test]
    fn from_dto_rejects_unknown_action() {
        let dto = OaEventDto {
            oa_event_id: "OA-002".to_string(),
            resolution_id: "#[W41]01".to_string(),
            action: "archived".to_string(),
            actor_id: "E10086".to_string(),
            event_time: "2026-10-12T09:30:00+08:00".to_string(),
        };
        let err = from_dto(dto).expect_err("unknown action 应失败");
        assert!(err.contains("archived"), "错误信息应包含原始字段值：{err}");
    }

    #[test]
    fn from_dto_rejects_malformed_event_time() {
        let dto = OaEventDto {
            oa_event_id: "OA-003".to_string(),
            resolution_id: "#[W41]01".to_string(),
            action: "completed".to_string(),
            actor_id: "E10086".to_string(),
            event_time: "not-a-date".to_string(),
        };
        let err = from_dto(dto).expect_err("非法时间格式应失败");
        assert!(err.contains("event_time"), "错误信息应提示字段名：{err}");
    }

    #[test]
    fn pull_response_last_page_detected() {
        let resp = OaPullResponse {
            events: vec![],
            next_cursor: None,
        };
        assert!(resp.is_last_page());

        let resp2 = OaPullResponse {
            events: vec![],
            next_cursor: Some("cursor-2".to_string()),
        };
        assert!(!resp2.is_last_page());
    }
}