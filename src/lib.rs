//! `resolution-watch` 库 + CLI 的模块导出层。
//!
//! 主路径：会议纪要 md 段落切分 → 决议条目结构化抽取（LLM 或 deterministic fallback）
//!   → OA 流转事件拉取（REST/SQL） → 决议状态机推进（5 态非法跃迁拒绝）
//!   → 6 桶剩余天数派生（Lt3/Lt7/Lt14/Lt30/Gt30/Overdue）
//!   → Sqlite 周次级幂等存储 → 导出包（resolutions.csv + overdue/due_soon/conflicts/audit_log
//!     四种 JSONL + summary.json + sha256 manifest + 5 段审计链）。
//!
//! 模块按职责拆分：
//! - `schemas`   — schema-first 强类型化（决议条目、OA 事件、桶、审计段）
//! - `cli`       — clap 子命令 + env 校验 + 退出码 + tracing dispatch
//! - `oa_pull`   — 出站 OA 拉取（bearer 鉴权 + 重试 + 翻页 + mock）
//! - `extract`   — 决议抽取（OpenAI-compatible ModelGateway + deterministic fallback + 5 类缺项校验）
//! - `bucket`    — 5 状态机非法跃迁拒绝 + 6 桶剩余天数派生
//! - `store`     — Sqlite 决议推进表 + 周次幂等键
//! - `export`    — csv + 4 种 JSONL + summary + sha256 + atomic_rename 不写半成品

pub mod bucket;
pub mod cli;
pub mod export;
pub mod extract;
pub mod oa_pull;
pub mod schemas;
pub mod store;
