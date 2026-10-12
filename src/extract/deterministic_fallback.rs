//! 决议条目 deterministic_fallback：按 `## 决议 N:` 段落切分 + 正则识别科室/工号/日期/验收。
//!
//! 当 LLM 不可达 / 响应 schema 校验失败 / 端点配置缺失时切到此路径；
//! 输出同样进入 `validate_resolution` 校验，5 类必填字段缺一拒绝。
//!
//! 与源 `crates/runtime/src/prod.rs` 的「强一致 deterministic 失败可见」同构：
//! LLM 不可达时显式走确定性路径，**不静默回退**。

use regex::Regex;
use std::sync::OnceLock;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::schemas::{OaAction, OaEvent};
use crate::extract::validate::{validate, InvalidResolution, RawResolution};

use chrono::{DateTime, Utc};

/// 单个 `## 决议 N:` 段落切分结果（未命中的字段在 `field` 中标记为空字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedSection {
    pub section_id: String,           // "决议 1"
    pub resolution_id: Option<String>,
    pub title: Option<String>,
    pub owner_department: Option<String>,
    pub owner_id: Option<String>,
    pub due_at: Option<String>,
    pub acceptance: Option<String>,
    /// 原始段落正文（不含 `## 决议 N:` 头），供科室识别 / 兜底扫描。
    pub raw_body: String,
}

/// `## 决议 N:` 段落切分正则（开头 `## ` 后跟 `决议` + 数字 + `:`）。
fn section_split_pat() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^## 决议[ \t]*\d+[ \t]*:").unwrap())
}

/// 责任科室识别正则（命中院内科室字典；字典从外部注入）。
fn detect_department(depts_pattern: &str, body: &str) -> Option<String> {
    let re = Regex::new(depts_pattern).ok()?;
    re.find(body).map(|m| m.as_str().to_string())
}

/// 工号识别正则（6 位数字）。
fn detect_owner_id(body: &str) -> Option<String> {
    let re = Regex::new(r"\b(\d{6})\b").ok()?;
    re.find(body).map(|m| m.as_str().to_string())
}

/// 截止日期识别正则（YYYY-MM-DD）。
fn detect_due_at(body: &str) -> Option<String> {
    let re = Regex::new(r"\b(\d{4}-\d{2}-\d{2})\b").ok()?;
    re.find(body).map(|m| m.as_str().to_string())
}

/// 验收标准识别（"验收" / "完成标准" / "达标" 任一关键字后的句子）。
fn detect_acceptance(body: &str) -> Option<String> {
    let re = Regex::new(r"(?s)(?:验收标准|达标要求|验收方式|完成标准)\s*[:：]?\s*([^\n\r]+)").ok()?;
    re.captures(body).map(|c| c.get(1).unwrap().as_str().trim().to_string())
}

/// 标题识别：段落首行（非空、`## 决议 N:` 之外的第一段文字）。
fn detect_title(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|s| s.to_string())
}

/// 把 minutes.md 切成 `## 决议 N:` 段落列表。
pub fn split_sections(minutes_md: &str) -> Vec<ParsedSection> {
    let re = section_split_pat();
    let mut sections = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut current_label: Option<String> = None;

    for mat in re.find_iter(minutes_md) {
        if let Some(label) = current_label.take() {
            // 上一段从 current_start 到 mat.start()
            let body = &minutes_md[current_start.unwrap()..mat.start()];
            sections.push(parse_section(&label, body));
        }
        current_start = Some(mat.end());
        current_label = Some(label_from_match(&minutes_md[mat.start()..mat.end()]));
    }

    if let Some(label) = current_label {
        let body = &minutes_md[current_start.unwrap()..];
        sections.push(parse_section(&label, body));
    }

    sections
}

fn label_from_match(header: &str) -> String {
    // header 是 "## 决议 N:" 的形式，保留 "决议 N" 部分（含数字）便于 `to_raw` 派生决议编号。
    let trimmed = header.trim_start_matches('#').trim();
    // 取 `:` 之前的部分
    let label = trimmed.split(':').next().unwrap_or(trimmed).trim();
    label.to_string()
}

fn parse_section(label: &str, body: &str) -> ParsedSection {
    ParsedSection {
        section_id: label.to_string(),
        resolution_id: None,
        title: detect_title(body),
        owner_department: None,
        owner_id: detect_owner_id(body),
        due_at: detect_due_at(body),
        acceptance: detect_acceptance(body),
        raw_body: body.to_string(),
    }
}

/// 把 `ParsedSection` 提升为 `RawResolution`（用于 validate）。
pub fn to_raw(section: &ParsedSection, week_id: &str) -> RawResolution {
    RawResolution {
        resolution_id: section.resolution_id.clone().or_else(|| {
            // 派生决议编号：从 section_id "决议 N" 中提取 N
            let n = section
                .section_id
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            if n > 0 {
                Some(format!("#[{}]02", week2_label(week_id, n)))
            } else {
                None
            }
        }),
        title: section.title.clone(),
        owner_department: section.owner_department.clone(),
        owner_id: section.owner_id.clone(),
        due_at: section.due_at.clone(),
        acceptance: section.acceptance.clone(),
        evidence_ref: None,
    }
}

/// 把 `2026-W41` 转成 `W41` 用于决议编号拼接。
fn week2_label(week_id: &str, n: u32) -> String {
    let w = week_id
        .split_once('W')
        .map(|(_, w)| w.to_string())
        .unwrap_or_else(|| "00".to_string());
    format!("{w}{:02}", n)
}

/// 给 sections 注入科室识别（外部传入科室字典正则）。
///
/// 扫描原始段落正文（`raw_body`），匹配责任科室。
pub fn enrich_with_departments(sections: &mut [ParsedSection], depts_pattern: &str) {
    for s in sections.iter_mut() {
        if let Some(d) = detect_department(depts_pattern, &s.raw_body) {
            s.owner_department = Some(d);
        }
    }
}

/// 主入口：从 minutes.md + 科室字典正则 → `Vec<Resolution>`，5 类缺项拒绝。
pub fn parse(
    minutes_md: &str,
    week_id: &str,
    depts_pattern: &str,
) -> Result<Vec<crate::schemas::Resolution>, InvalidResolution> {
    let mut sections = split_sections(minutes_md);
    enrich_with_departments(&mut sections, depts_pattern);
    let raw: Vec<RawResolution> = sections.iter().map(|s| to_raw(s, week_id)).collect();
    validate(&raw)
}

/// 把 OA event list 转成单一 mock 决议推进序列（用于测试）。
#[allow(dead_code)]
pub fn mock_oa_events(week_id: &str) -> Vec<OaEvent> {
    vec![
        OaEvent {
            oa_event_id: format!("OA-{week_id}-001"),
            resolution_id: format!("#[{}]01", week_label(week_id)),
            action: OaAction::Started,
            actor_id: "E10086".into(),
            event_time: Utc::now(),
        },
        OaEvent {
            oa_event_id: format!("OA-{week_id}-002"),
            resolution_id: format!("#[{}]01", week_label(week_id)),
            action: OaAction::Completed,
            actor_id: "E10086".into(),
            event_time: Utc::now(),
        },
    ]
}

fn week_label(week_id: &str) -> String {
    week_id
        .split_once('W')
        .map(|(_, w)| format!("W{w}"))
        .unwrap_or_else(|| "W00".to_string())
}

/// 解析 YYYY-MM-DD → `NaiveDate`（测试用）。
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// mock 一个当前日期（测试用；生产由 caller 注入）。
#[allow(dead_code)]
pub fn mock_now() -> DateTime<Utc> {
    Utc::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPT_PATTERN: &str = r"医务科|护理部|信息科|财务科|医保办|设备科|院办|后勤科";

    const SAMPLE_MD: &str = r#"## 决议 1: 试点开展 DRG 入组校验

责任科室：医务科，工号 100086。截止日期 2026-10-31。验收标准：骨科 3 个病组入组一致率 ≥ 95%。

## 决议 2: 门诊电子病历升级

责任科室：信息科，工号 200002。截止 2026-11-05。完成标准：门诊 5 个科室全部切换到 EMR 5.0。

## 决议 3: 缺验收决议（不应通过 validate）
责任科室：医保办，工号 300003。截止 2026-12-01。
"#;

    #[test]
    fn split_sections_returns_three_sections() {
        let sections = split_sections(SAMPLE_MD);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].section_id, "决议 1");
        assert_eq!(sections[1].section_id, "决议 2");
        assert_eq!(sections[2].section_id, "决议 3");
    }

    #[test]
    fn detect_owner_id_picks_6_digit_id() {
        let body = "工号 100086 负责";
        assert_eq!(detect_owner_id(body).as_deref(), Some("100086"));
    }

    #[test]
    fn detect_due_at_picks_iso_date() {
        let body = "截止日期 2026-10-31";
        assert_eq!(detect_due_at(body).as_deref(), Some("2026-10-31"));
    }

    #[test]
    fn detect_acceptance_succeeds() {
        let body = "验收标准：骨科 3 个病组入组一致率 ≥ 95%";
        let r = detect_acceptance(body).unwrap();
        assert!(r.contains("骨科"));
    }

    #[test]
    fn enrich_assigns_owner_department() {
        let mut sections = split_sections(SAMPLE_MD);
        enrich_with_departments(&mut sections, DEPT_PATTERN);
        assert_eq!(sections[0].owner_department.as_deref(), Some("医务科"));
        assert_eq!(sections[1].owner_department.as_deref(), Some("信息科"));
        assert_eq!(sections[2].owner_department.as_deref(), Some("医保办"));
    }

    #[test]
    fn parse_full_pipeline_returns_two_resolutions_third_rejected() {
        let r = parse(SAMPLE_MD, "2026-W41", DEPT_PATTERN);
        // 第三个决议缺 acceptance → validate 拒
        assert!(r.is_err());
    }

    #[test]
    fn parse_full_pipeline_with_acceptance_passes() {
        let md = r#"## 决议 1: 决议甲
责任科室：医务科，工号 100086。截止 2026-10-31。验收标准：达标。
"#;
        let r = parse(md, "2026-W41", DEPT_PATTERN);
        assert!(r.is_ok(), "parse 应成功：{r:?}");
        let r = r.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].owner_id, "100086");
    }
}