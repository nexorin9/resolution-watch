//! 决议条目校验：5 类必填字段缺一返回 `InvalidResolution::Missing(field)`。
//!
//! 校验规则：
//! - 5 必填字段任一为空或缺失 → 拒绝
//! - `due_at` 必须是 `YYYY-MM-DD` 格式合法日期
//! - `resolution_id` 必须非空字符串
//! - 缺字段返回具名 `field`，便于 caller 定位

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::schemas::Resolution;

/// 决议条目校验错误。
#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
pub enum InvalidResolution {
    /// 必填字段缺失或为空。
    #[error("决议条目缺字段：{0}")]
    Missing(&'static str),
    /// `due_at` 解析失败。
    #[error("决议条目 due_at 解析失败：{0}")]
    InvalidDate(String),
}

impl InvalidResolution {
    pub fn field(&self) -> &'static str {
        match self {
            InvalidResolution::Missing(f) => f,
            InvalidResolution::InvalidDate(_) => "due_at",
        }
    }
}

/// 校验 JSON 视图（LLM 输出） → 决议条目数组。
///
/// 接受 `Vec<RawResolution>`，逐条校验；遇到首个失败立即返回 `InvalidResolution`（不静默忽略）。
pub fn validate(raw: &[RawResolution]) -> Result<Vec<Resolution>, InvalidResolution> {
    let mut out = Vec::with_capacity(raw.len());
    for (i, r) in raw.iter().enumerate() {
        if r.resolution_id.as_deref().unwrap_or("").trim().is_empty() {
            return Err(InvalidResolution::Missing(field_name("resolution_id", i)));
        }
        if r.title.as_deref().unwrap_or("").trim().is_empty() {
            return Err(InvalidResolution::Missing(field_name("title", i)));
        }
        if r.owner_department.as_deref().unwrap_or("").trim().is_empty() {
            return Err(InvalidResolution::Missing(field_name("owner_department", i)));
        }
        if r.owner_id.as_deref().unwrap_or("").trim().is_empty() {
            return Err(InvalidResolution::Missing(field_name("owner_id", i)));
        }
        if r.due_at.as_deref().unwrap_or("").trim().is_empty() {
            return Err(InvalidResolution::Missing(field_name("due_at", i)));
        }
        if r.acceptance.as_deref().unwrap_or("").trim().is_empty() {
            return Err(InvalidResolution::Missing(field_name("acceptance", i)));
        }

        let due_at_str = r.due_at.as_deref().unwrap();
        let due_at = NaiveDate::parse_from_str(due_at_str, "%Y-%m-%d")
            .map_err(|_| InvalidResolution::InvalidDate(due_at_str.to_string()))?;

        out.push(Resolution {
            resolution_id: r.resolution_id.clone().unwrap(),
            title: r.title.clone().unwrap(),
            owner_department: r.owner_department.clone().unwrap(),
            owner_id: r.owner_id.clone().unwrap(),
            due_at,
            acceptance: r.acceptance.clone().unwrap(),
            evidence_ref: r
                .evidence_ref
                .clone()
                .filter(|s| !s.is_empty() && s != "null"),
        });
    }
    Ok(out)
}

fn field_name(base: &'static str, _index: usize) -> &'static str {
    // 简化：直接返回 base，不带 index；便于 caller 精确知道哪个决议条目缺哪字段可通过 raw 列表索引
    base
}

/// LLM 输出决议条目原始结构（每字段 Option，缺值时 validate 拒）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawResolution {
    pub resolution_id: Option<String>,
    pub title: Option<String>,
    pub owner_department: Option<String>,
    pub owner_id: Option<String>,
    pub due_at: Option<String>,
    pub acceptance: Option<String>,
    #[serde(default)]
    pub evidence_ref: Option<String>,
}

/// 从 LLM 输出 JSON `{"resolutions": [...]}` 解析为 `Vec<RawResolution>`。
pub fn parse_llm_json(content: &str) -> Result<Vec<RawResolution>, InvalidResolution> {
    let v: serde_json::Value = serde_json::from_str(content).map_err(|_| {
        InvalidResolution::Missing("llm_response_top_level")
    })?;
    let arr = v
        .get("resolutions")
        .ok_or(InvalidResolution::Missing("resolutions"))?;
    let raw: Vec<RawResolution> = serde_json::from_value(arr.clone())
        .map_err(|_| InvalidResolution::Missing("resolutions_parse"))?;
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> RawResolution {
        RawResolution {
            resolution_id: Some("#[W41]01".into()),
            title: Some("标题".into()),
            owner_department: Some("医务科".into()),
            owner_id: Some("E10086".into()),
            due_at: Some("2026-10-31".into()),
            acceptance: Some("验收标准".into()),
            evidence_ref: None,
        }
    }

    #[test]
    fn accepts_complete_resolution() {
        let raw = vec![ok()];
        let r = validate(&raw).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].resolution_id, "#[W41]01");
    }

    #[test]
    fn rejects_missing_resolution_id() {
        let mut r = ok();
        r.resolution_id = None;
        let err = validate(&vec![r]).unwrap_err();
        assert_eq!(err, InvalidResolution::Missing("resolution_id"));
    }

    #[test]
    fn rejects_missing_title() {
        let mut r = ok();
        r.title = Some("".into());
        let err = validate(&vec![r]).unwrap_err();
        assert_eq!(err, InvalidResolution::Missing("title"));
    }

    #[test]
    fn rejects_missing_owner_department() {
        let mut r = ok();
        r.owner_department = None;
        let err = validate(&vec![r]).unwrap_err();
        assert_eq!(err, InvalidResolution::Missing("owner_department"));
    }

    #[test]
    fn rejects_missing_owner_id() {
        let mut r = ok();
        r.owner_id = Some("  ".into()); // 空白
        let err = validate(&vec![r]).unwrap_err();
        assert_eq!(err, InvalidResolution::Missing("owner_id"));
    }

    #[test]
    fn rejects_missing_due_at() {
        let mut r = ok();
        r.due_at = Some("".into());
        let err = validate(&vec![r]).unwrap_err();
        assert_eq!(err, InvalidResolution::Missing("due_at"));
    }

    #[test]
    fn rejects_invalid_due_at_format() {
        let mut r = ok();
        r.due_at = Some("2026/10/31".into());
        let err = validate(&vec![r]).unwrap_err();
        match err {
            InvalidResolution::InvalidDate(s) => assert_eq!(s, "2026/10/31"),
            _ => panic!("应为 InvalidDate"),
        }
    }

    #[test]
    fn rejects_missing_acceptance() {
        let mut r = ok();
        r.acceptance = None;
        let err = validate(&vec![r]).unwrap_err();
        assert_eq!(err, InvalidResolution::Missing("acceptance"));
    }

    #[test]
    fn parse_llm_json_unwraps_resolutions_key() {
        let content = r#"{"resolutions":[{"resolution_id":"r1"}]}"#;
        let raw = parse_llm_json(content).unwrap();
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].resolution_id.as_deref(), Some("r1"));
    }

    #[test]
    fn parse_llm_json_rejects_missing_resolutions_key() {
        let content = r#"{"foo": []}"#;
        let err = parse_llm_json(content).unwrap_err();
        assert!(matches!(err, InvalidResolution::Missing(_)));
    }
}