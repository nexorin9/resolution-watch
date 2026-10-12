//! 决议条目 / 推进事件 / 分桶 / 审计段 schema-first 强类型化。
//!
//! 所有业务数据结构在这里集中定义。serde 派生负责 CSV / JSON / JSONL / OA 流转事件的
//! 正反序列化；schemars 派生负责对外（OA 对接规范、导出包头）的 JSON Schema 描述。
//!
//! 源产品主路径同构：本模块复刻源 `npm-packages/convex/src/cli/codegen_templates/server.ts`
//! 的 `validatorToType` 思路 — 把 schema 字段一次性派生为 Rust 强类型 + JSON Schema，
//! 业务层始终通过这些类型交互，不再写「键-值字符串字典」传参。

use chrono::NaiveDate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------- 决议条目（会议纪要抽取的产物）----------

/// 决议条目：会议纪要 md 段落 `## 决议 N:` 结构化抽取结果。
///
/// 6 个必填字段 + 1 个选填字段，缺任一必填字段则由 `extract::validate` 拒绝。
/// `evidence_ref` 由后续 OA 流转事件回填（哪条 OA 事件落地了该决议）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    /// 决议编号：周内唯一，派生式 `#[WL]'NN`（例如 `#[W41]01`）。
    pub resolution_id: String,
    /// 决议标题，≤ 80 字符。
    pub title: String,
    /// 责任科室（命中 `config/departments.yaml` 字典）。
    pub owner_department: String,
    /// 责任人 / 工号。
    pub owner_id: String,
    /// 截止日期 `YYYY-MM-DD`。
    pub due_at: NaiveDate,
    /// 验收标准，≤ 200 字符。
    pub acceptance: String,
    /// 决议推进时引用的 OA 流转 id（可空，初次创建时不填）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

/// 决议状态机 5 态：4 活跃 + 1 终态。
///
/// 推进规则见 `bucket::advance`；非法跃迁由 `bucket::InvalidTransition` 拒绝，
/// 不会静默改写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    /// 待启动：决议刚被抽取出来，OA 端未上报 started 事件。
    Pending,
    /// 进行中：OA 端已上报 started / in_progress 事件。
    InProgress,
    /// 临近 deadline：当前日期距 `due_at` ≤ 3 天但未 submitted。
    DueSoon,
    /// 逾期：`due_at < 当前日期` 且未 completed。
    Overdue,
    /// 已完成：OA 端已上报 completed 事件（终态，不再回退）。
    Completed,
}

impl ResolutionStatus {
    /// snake_case 字符串（与 serde 序列化一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolutionStatus::Pending => "pending",
            ResolutionStatus::InProgress => "in_progress",
            ResolutionStatus::DueSoon => "due_soon",
            ResolutionStatus::Overdue => "overdue",
            ResolutionStatus::Completed => "completed",
        }
    }
}

// ---------- OA 流转事件（OA REST/SQL 拉取的产物）----------

/// OA 流转事件：从院内 OA 系统的只读端点拉取。
///
/// 5 个核心字段 + 1 个状态字段（OA 端上报的当前决议状态，便于与本工具派生状态对账）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OaEvent {
    /// OA 端唯一事件 id。
    pub oa_event_id: String,
    /// 关联的决议编号（与 `Resolution::resolution_id` 对齐）。
    pub resolution_id: String,
    /// 事件动作：流转语义。
    pub action: OaAction,
    /// 触发人 / 工号。
    pub actor_id: String,
    /// 事件时间戳（RFC3339 / `YYYY-MM-DDTHH:MM:SS+08:00` 形式）。
    pub event_time: chrono::DateTime<chrono::Utc>,
}

/// OA 流转事件的 5 种动作。
///
/// 5 态对应 `bucket::advance()` 的状态跃迁；`Blocked` 不直接驱动状态变化，
/// 仅写入审计链（需要人工介入时使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OaAction {
    /// 决议已分派到责任科室，开始处理。
    Started,
    /// 责任科室在 OA 端更新进展。
    InProgress,
    /// 责任科室遇到阻塞，需要人工介入（写入审计但不推进状态）。
    Blocked,
    /// 责任科室提交验收（≤ 3 天内会进入 due_soon）。
    Submitted,
    /// 决议已完成（任何活跃态 → completed）。
    Completed,
}

impl OaAction {
    /// snake_case 字符串（与 serde 序列化一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            OaAction::Started => "started",
            OaAction::InProgress => "in_progress",
            OaAction::Blocked => "blocked",
            OaAction::Submitted => "submitted",
            OaAction::Completed => "completed",
        }
    }
}

// ---------- 剩余天数 6 桶 ----------

/// 剩余天数 6 桶：覆盖所有可能的 `due_in_days` 取值。
///
/// 派生规则（见 `bucket::due_bucket`）：
/// - `due_in_days ∈ [0, 3]`    → `Lt3`
/// - `due_in_days ∈ (3, 7]`    → `Lt7`
/// - `due_in_days ∈ (7, 14]`   → `Lt14`
/// - `due_in_days ∈ (14, 30]`  → `Lt30`
/// - `due_in_days > 30`        → `Gt30`
/// - `due_in_days < 0`         → `Overdue`（含 overdue 桶的负值判定）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DueBucket {
    /// ≤ 3 天
    Lt3,
    /// 4–7 天
    Lt7,
    /// 8–14 天
    Lt14,
    /// 15–30 天
    Lt30,
    /// > 30 天
    Gt30,
    /// 已逾期（负值）
    Overdue,
}

impl DueBucket {
    /// snake_case 字符串（与 serde 序列化一致）。
    pub fn as_str(&self) -> &'static str {
        match self {
            DueBucket::Lt3 => "lt3",
            DueBucket::Lt7 => "lt7",
            DueBucket::Lt14 => "lt14",
            DueBucket::Lt30 => "lt30",
            DueBucket::Gt30 => "gt30",
            DueBucket::Overdue => "overdue",
        }
    }
}

// ---------- 审计链 5 段 ----------

/// 审计链 5 段：每次决议推进/导出走全程的关键节点。
///
/// 写入 `out/<week_id>/audit_log.jsonl`，每行一段。
/// 段内统一含 `tool_version` / `commit_hash` / `env_hash` / `model_name` / `prompt_hash` /
/// `latency_ms` / `status` 七个字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditSegment {
    /// CLI 启动
    CliStart,
    /// OA 流转事件拉取完成
    OaPullComplete,
    /// 决议抽取（LLM 或 deterministic）调用完成
    ExtractCall,
    /// 状态机推进 / 6 桶派生完成
    BucketAdvance,
    /// 导出包写入完成（含 sha256 计算）
    ExportComplete,
}

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_serializes_roundtrip() {
        let r = Resolution {
            resolution_id: "#[W41]01".to_string(),
            title: "试点开展 DRG 入组校验".to_string(),
            owner_department: "医务科".to_string(),
            owner_id: "E10086".to_string(),
            due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
            acceptance: "骨科 3 个病组入组一致率 ≥ 95%".to_string(),
            evidence_ref: None,
        };
        let j = serde_json::to_string(&r).unwrap();
        let back: Resolution = serde_json::from_str(&j).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn resolution_status_snake_case() {
        let s = serde_json::to_string(&ResolutionStatus::DueSoon).unwrap();
        assert_eq!(s, "\"due_soon\"");
        let back: ResolutionStatus = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ResolutionStatus::DueSoon);
    }

    #[test]
    fn oa_action_all_five_variants_parse() {
        for (raw, expected) in [
            (r#""started""#, OaAction::Started),
            (r#""in_progress""#, OaAction::InProgress),
            (r#""blocked""#, OaAction::Blocked),
            (r#""submitted""#, OaAction::Submitted),
            (r#""completed""#, OaAction::Completed),
        ] {
            let parsed: OaAction = serde_json::from_str(raw).unwrap();
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn due_bucket_six_variants_serialize() {
        let names = [
            (DueBucket::Lt3, "\"lt3\""),
            (DueBucket::Lt7, "\"lt7\""),
            (DueBucket::Lt14, "\"lt14\""),
            (DueBucket::Lt30, "\"lt30\""),
            (DueBucket::Gt30, "\"gt30\""),
            (DueBucket::Overdue, "\"overdue\""),
        ];
        for (b, expected) in names {
            assert_eq!(serde_json::to_string(&b).unwrap(), expected);
        }
    }

    #[test]
    fn audit_segment_snake_case() {
        for (seg, expected) in [
            (AuditSegment::CliStart, "\"cli_start\""),
            (AuditSegment::OaPullComplete, "\"oa_pull_complete\""),
            (AuditSegment::ExtractCall, "\"extract_call\""),
            (AuditSegment::BucketAdvance, "\"bucket_advance\""),
            (AuditSegment::ExportComplete, "\"export_complete\""),
        ] {
            assert_eq!(serde_json::to_string(&seg).unwrap(), expected);
        }
    }

    #[test]
    fn json_schema_emits_object_for_resolution() {
        // 关键不变量：导出包 + OA 对接规范都用 schemars 派生，验证 schema 是合法 JSON Schema 对象。
        let schema = schemars::schema_for!(Resolution);
        let j = serde_json::to_string(&schema).unwrap();
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert!(v.get("properties").is_some(), "schema 缺 properties");
        assert_eq!(
            v.get("type").and_then(|x| x.as_str()),
            Some("object"),
            "schema root type 应为 object"
        );
    }
}
