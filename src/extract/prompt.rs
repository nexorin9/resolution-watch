//! LLM prompt 层（SYSTEM_PROMPT + USER_TEMPLATE + JSON Schema 描述）。
//!
//! LLM 抽取规则固定写死在本模块；不同医院可调整 SYSTEM_PROMPT 文案但 JSON Schema 不可改。
//!
//! ## 与源同构
//!
//! 源 `crates/isolate/src/udf_runtime.rs` 的 UDF 沙箱类型化校验 — 本任务以 JSON Schema
//! 替代 V8 isolate，决议条目结构化输出必经校验后才进入业务层。

/// SYSTEM_PROMPT（决议抽取规则 + JSON Schema）。
pub const SYSTEM_PROMPT: &str = r#"你是医院院务会决议抽取助手。从会议纪要 markdown 中抽取所有决议条目，输出 JSON 数组。

**严格按以下 JSON Schema 输出**，缺任一必填字段都会被拒绝（不会自动补齐）：

```json
{
  "resolutions": [
    {
      "resolution_id": "周内唯一决议编号，派生式 '#[W<week>]NN'，例如 '#[W41]01'",
      "title": "决议标题，≤ 80 字符",
      "owner_department": "责任科室，命中院内科室字典",
      "owner_id": "责任人工号，6 位数字",
      "due_at": "YYYY-MM-DD 格式截止日期",
      "acceptance": "验收标准，≤ 200 字符",
      "evidence_ref": "决议推进时的 OA 流转 id（可空字符串）"
    }
  ]
}
```

**必须输出顶层 `resolutions` 数组**。如果会议纪要没有任何决议条目，输出 `{"resolutions": []}`。

不要输出诊断建议、不要建议治疗方案、不要替 OA 端写回动作。"#;

/// USER_TEMPLATE（占位 {minutes_md} + {week_id}）。
pub const USER_TEMPLATE: &str = r#"以下为本周（ISO week = {week_id}）院务会会议纪要 markdown，请抽取所有决议条目：

```markdown
{minutes_md}
```

只返回符合上述 JSON Schema 的 `{{"resolutions": [...]}}` 结构。"#;

/// 拼装 user 消息。
pub fn render_user(week_id: &str, minutes_md: &str) -> String {
    USER_TEMPLATE
        .replace("{week_id}", week_id)
        .replace("{minutes_md}", minutes_md)
}

/// JSON Schema（决议条目 schema 字符串描述，附在 system prompt 让 LLM 严格遵守）。
pub const JSON_SCHEMA_DESCRIPTION: &str = r#"JSON Schema for Resolution:
- resolution_id: string, 周内唯一, format #[W<week>]NN
- title: string, maxLength 80
- owner_department: string, maxLength 20
- owner_id: string, 工号
- due_at: string, format YYYY-MM-DD
- acceptance: string, maxLength 200
- evidence_ref: string | null, optional"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_user_replaces_placeholders() {
        let user = render_user("2026-W41", "# 决议\n张三");
        assert!(user.contains("2026-W41"));
        assert!(user.contains("# 决议"));
        assert!(!user.contains("{week_id}"));
        assert!(!user.contains("{minutes_md}"));
    }

    #[test]
    fn system_prompt_must_contain_required_keys() {
        for k in [
            "resolution_id",
            "title",
            "owner_department",
            "owner_id",
            "due_at",
            "acceptance",
        ] {
            assert!(SYSTEM_PROMPT.contains(k), "SYSTEM_PROMPT 缺字段 {k}");
        }
    }
}