# 架构与数据流

> 本文面向**信息科工程师 / 二次开发者**：描述 `resolution-watch` 院务会决议督办台账的
> 端到端数据流、6 桶派生规则、5 态状态机与审计链结构，与开源参考能力的对照关系。
> 与本目录下 `integration.md`（OA 对接规范，**外部视角**）相对，本文是
> **代码内部视角**——把源代码模块与消费式 RACI 一一对应。

---

## 1. 系统边界

| 项 | 范围 |
|---|---|
| **输入边界** | `inbox/<week_id>/minutes.md`（秘书手工落地会议纪要）+ OA REST 端点（只读，凭据从 `OA_API_BEARER` / `OA_QUERY_DSN` env 读）|
| **输出边界** | `out/<week_id>/` 下 6 个文件：`resolutions.csv` + `overdue.jsonl` + `due_soon.jsonl` + `conflicts.jsonl` + `summary.json` + `audit_log.jsonl`，配套 `sha256_manifest.json` |
| **持久化** | `.reso_cache.sqlite`（决议推进表 + 周次幂等表），路径由 `RESO_DB_PATH` env 决定 |
| **不接触** | HIS / EMR / 任何患者可识别明细（硬红线）；OA 端**写回**（推进责任在 OA 端）|
| **决策面** | 不替秘书/科长签字；导出包带 sha256 + 元字段头供下游核对 |

---

## 2. 端到端 ASCII 数据流

```
┌──────────────────────────────────────────────────────────────────────────┐
│  输入侧（秘书 + OA）                                                      │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  inbox/2026-W41/minutes.md                  OA REST 端点                  │
│  ┌────────────────────────┐                ┌────────────────────┐        │
│  │ ## 决议 1: XXX         │                │ GET /oa/events?    │        │
│  │ ## 决议 2: YYY         │                │   week_id=2026-W41 │        │
│  │ ## 决议 3: ZZZ         │                │ Authorization:      │        │
│  │ ...                    │                │   Bearer ${TOKEN}   │        │
│  └─────────┬──────────────┘                └──────────┬─────────┘        │
│            │ md 段落切分                              │ OaEventDto 数组   │
│            │ (pulldown-cmark + 正则)                  │ (5 必填字段)      │
└────────────┼─────────────────────────────────────────┼───────────────────┘
             │                                         │
             ▼                                         ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  处理侧（CLI 单进程批处理 — Rust + tokio current_thread runtime）        │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌────────────────┐    ┌────────────────┐    ┌────────────────────┐     │
│  │ extract 模块   │    │ bucket 模块    │    │ store 模块          │     │
│  │ (LLM 抽取 +    │───▶│ (5 态状态机 +  │───▶│ (Sqlite 决议推进    │     │
│  │  deterministic │    │  6 桶剩余天数) │   │  表 + 周次幂等键)    │     │
│  │  fallback)    │    │                │    │                     │     │
│  └────────┬───────┘    └────────┬───────┘    └──────────┬──────────┘     │
│           │                     │                       │                │
│           │ Resolution[]        │ ResolutionEvent[]     │ idem-key       │
│           │ (5 必填 + 1 选填)    │ (5 态 + 6 桶)         │ (week_id, hash)│
│           │                     │                       │                │
└───────────┼─────────────────────┼───────────────────────┼────────────────┘
            │                     │                       │
            ▼                     ▼                       ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  导出侧（write_atomic → 6 文件 + sha256 manifest）                        │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  out/2026-W41/                                                            │
│  ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐         │
│  │ resolutions.csv  │ │ overdue.jsonl    │ │ due_soon.jsonl   │         │
│  │ 决议条目 + 桶    │ │ (overdue 桶)    │ │ (≤3 天)          │         │
│  └──────────────────┘ └──────────────────┘ └──────────────────┘         │
│  ┌──────────────────┐ ┌──────────────────┐ ┌──────────────────┐         │
│  │ conflicts.jsonl  │ │ summary.json     │ │ audit_log.jsonl  │         │
│  │ (口径不一致)     │ │ (4+1 桶统计)     │ │ (5 段审计链)     │         │
│  └──────────────────┘ └──────────────────┘ └──────────────────┘         │
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │ sha256_manifest.json — 6 文件 sha256 校验和                     │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
            │
            ▼
┌──────────────────────────────────────────────────────────────────────────┐
│  签字侧（秘书 / 科长 → OA 端推进，不在本工具边界内）                       │
└──────────────────────────────────────────────────────────────────────────┘
```

### 2.1 数据流关键节点

| 节点 | 模块 | 输入 | 输出 | 失败语义 |
|------|------|------|------|----------|
| **md 段落切分** | `extract::deterministic_fallback` | `inbox/<week_id>/minutes.md` | `Vec<RawResolution>` | 段落正则未命中 → 走 LLM 兜底 |
| **OA 拉取** | `oa_pull::client::fetch_events` | `OA_API_BEARER` + `OA_QUERY_DSN` | `Vec<OaEvent>` | 401/403 → `Auth`（CLI 退出码 2）|
| **决议抽取（LLM 路径）** | `extract::llm::ModelGateway::chat` | minutes.md | `Vec<Resolution>` JSON | 不可达 → 切 deterministic fallback |
| **决议抽取（fallback 路径）** | `extract::deterministic_fallback::parse` | minutes.md + 科室字典 | `Vec<Resolution>` | 缺字段 → `validate_resolution` 拒绝 |
| **Schema 校验** | `extract::validate::validate_resolution` | `Resolution` | `Ok(())` / `Err(InvalidResolution::Missing(field))` | 缺 5 类字段 → 重试 1 次 → 仍失败 fallback |
| **OA 事件应用** | `bucket::advance::advance` | `OaEvent` + 当前状态 | `Ok(new_status)` / `Err(InvalidTransition)` | 非法跃迁 → 不静默改状态 |
| **6 桶派生** | `bucket::due_bucket::bucket` | `(current_date, due_at)` | `DueBucket` | `due_at < current_date` → `Overdue` |
| **Sqlite 写入** | `store::repo::upsert_idempotent` | `(week_id, resolution_id, action, ...)` | `Ok(())` / `Ok(status_existed)` | 主键冲突 → 不覆盖 |
| **导出** | `export::atomic_rename::write_atomic` | `out_dir` + 闭包（6 文件写入）| `Ok(out_dir)` | 闭包 Err → RAII 清理 `.tmp/` |

---

## 3. 6 桶派生详细

### 3.1 桶边界

`bucket::due_bucket::bucket(current_date, due_at) -> DueBucket` 把每条决议的剩余天数归入 6 个枚举值：

| `due_in_days`（当前日期 - `due_at`） | 桶 | 业务含义 |
|--------------------------------------|-----|----------|
| `< 0`（未到期） | `Lt3` / `Lt7` / `Lt14` / `Lt30` / `Gt30` 按区间细分 | 待启动 / 临近 |
| `= 0`（当天到期） | `Lt3` | 临近 deadline（剩 ≤3 天）|
| `(0, 3]`（剩 1-3 天）| `Lt3` | 临近 deadline |
| `(3, 7]` | `Lt7` | 1 周内待推进 |
| `(7, 14]` | `Lt14` | 2 周内待推进 |
| `(14, 30]` | `Lt30` | 1 月内待推进 |
| `> 30` | `Gt30` | 远期决议 |
| `< 0`（`due_at < current_date`）| `Overdue` | **逾期（负值判定）** |

### 3.2 派生流程

```text
bucket(current_date, due_at):
    due_in_days = (due_at - current_date) as i64
    if due_in_days < 0:
        return Overdue
    if due_in_days <= 3:
        return Lt3
    if due_in_days <= 7:
        return Lt7
    if due_in_days <= 14:
        return Lt14
    if due_in_days <= 30:
        return Lt30
    return Gt30
```

### 3.3 桶与 `audit_log.jsonl` 的关系

| 桶 | `overdue.jsonl` | `due_soon.jsonl` | `summary.json` `by_due_bucket` |
|----|-----------------|-------------------|-------------------------------|
| `Overdue` | ✓ 写入 | 0 写入 | `Overdue += 1` |
| `Lt3` | 0 写入 | ✓ 写入（剩 ≤3 天）| `Lt3 += 1` |
| `Lt7` | 0 写入 | 0 写入 | `Lt7 += 1` |
| `Lt14` / `Lt30` / `Gt30` | 0 写入 | 0 写入 | 各自 `+= 1` |

> **关键不变量**：`overdue.jsonl` 包含所有 `due_in_days < 0` 的决议；`due_soon.jsonl` 包含所有 `due_in_days ∈ [0, 3]` 的决议。两者不相交但覆盖所有"剩余天数 ≤3" 的关键桶。

### 3.4 跨年与跨月边界

| 边界 case | 派生结果 |
|----------|---------|
| `due_at = 2026-12-31`, `current_date = 2026-01-05` | `due_in_days = 360` → `Gt30` |
| `due_at = 2026-01-01`, `current_date = 2026-12-31` | `due_in_days = -364` → `Overdue`（负值判定生效）|
| `due_at = 2026-03-01`, `current_date = 2026-02-28` | `due_in_days = 1` → `Lt3` |
| `due_at = 2026-W01`, `current_date = 2025-W53` | 跨 ISO 年边界，由 `WeekId::parse` 容错 |

> **跨年 ISO week**：分桶按 `due_at - current_date` 的日历差计算（`chrono::NaiveDate` 算术），不依赖 ISO 周次。ISO 周次仅用于 `inbox/<week_id>/` 路径与 `week_id` 字符串标识。

---

## 4. 状态机详细

### 4.1 5 态枚举 + 终态

`bucket::state_machine::ResolutionStatus` 5 态：

| 态 | 业务语义 | 是否活跃 | 是否终态 |
|----|---------|----------|----------|
| `Pending` | 待启动（决议刚录入）| ✓ | ✗ |
| `InProgress` | 进行中（已派发 / 部门办理）| ✓ | ✗ |
| `DueSoon` | 临近 deadline（剩 ≤3 天）| ✓ | ✗ |
| `Overdue` | 逾期（deadline 已过未完成）| ✓ | ✗ |
| `Completed` | 完成（OA `completed` 事件已落地）| ✗ | ✓ |

### 4.2 跃迁规则

```text
advance(current, oa_event):
    match (current, oa_event.action):
        # 合法跃迁
        (Pending,    Started)    => InProgress
        (Pending,    InProgress) => InProgress
        (InProgress, Started)    => InProgress    # 重复 started 不动
        (InProgress, InProgress) => InProgress    # 保持
        (InProgress, Blocked)    => InProgress    # 阻塞保持 in_progress
        (InProgress, Submitted)  => DueSoon       # 若 due_in_days ≤ 3
        (DueSoon,    Submitted)  => InProgress    # 推进后回 in_progress
        (DueSoon,    Completed)  => Completed
        (Overdue,    Completed)  => Completed
        (_,          Completed)  => Completed     # 任意 → completed

        # 非法跃迁（不静默改状态）
        (Pending,    Submitted)  => Err(InvalidTransition)  # 跳 in_progress
        (Pending,    DueSoon)   => Err(InvalidTransition)  # 跳两步
        (Pending,    Overdue)   => Err(InvalidTransition)
        (InProgress, DueSoon)   => Err(InvalidTransition)  # 必须先 submitted
        (Completed,  _)         => Err(InvalidTransition)  # 终态后不动
        (DueSoon,    Pending)   => Err(InvalidTransition)  # 倒退
        (Overdue,    Pending)   => Err(InvalidTransition)
        (Overdue,    InProgress) => Err(InvalidTransition) # 倒退
```

### 4.3 非法跃迁拒绝

`bucket::advance::advance(current, oa_event)` 在状态机校验失败时返回：

```rust
Err(BucketError::InvalidTransition(InvalidTransition {
    from: current,           // 起始态（如 Pending）
    to:   requested_target,  // 试图到达的态（如 DueSoon）
    reason: format!(
        "OA action {:?} 试图从 {} → {}，需先经由 {} / {} 推进",
        oa_event.action, current, requested_target,
        "Started", "Submitted",
    ),
}))
```

**绝不静默改状态**：OA action 与状态机不匹配时，`resolution_event.status` 字段不被更新；导出包仍按旧状态出桶，下周 Secretary 收到一致结果。

### 4.4 与 `audit_log.jsonl` 的关系

| 跃迁结果 | `audit_log.jsonl` 段 |
|---------|---------------------|
| 合法跃迁 | 写一条 `bucket_advance` 段，含 `from` / `to` / `oa_event_id` / `resolution_id` |
| 非法跃迁 | **不写**决议推进事件，但写一条 `bucket_advance_rejected` 段（含 `from` / `requested_to` / `reason` / `oa_event_id`）|
| 跃迁后落地（写入 Sqlite） | 关联 `bucket_advance` 段 + `store::repo::upsert_idempotent` 落库 |

### 4.5 6 桶 × 5 态 的笛卡尔覆盖

每个决议同时持有 `status`（5 态之一）与 `due_bucket`（6 桶之一），两者**正交**：

```
                Pending  InProgress  DueSoon  Overdue  Completed
   Overdue       0        0           0        ✓        ✓（已完成的逾期）
   Lt3           ✓        ✓           ✓        0        ✓
   Lt14          ✓        ✓           0        0        ✓
   Lt30          ✓        ✓           0        0        ✓
   Gt30          ✓        ✓           0        0        ✓
   (no overdue)
```

> **正交性保证**：`status` 由 OA event 推进驱动（事件驱动型），`due_bucket` 由日历派生（时间驱动型），两者解耦。

---

## 5. 5 段审计链详细

### 5.1 5 段顺序

`out/<week_id>/audit_log.jsonl` 每行一条 JSON 对象，按下方顺序在跑批期间落地：

| 序号 | 段 | 触发时机 | 段内字段 |
|------|------|---------|---------|
| 1 | `cli_start` | CLI 启动 + env 校验通过 | `tool_version` / `env_hash` / `model_name` / `model_timeout_ms` |
| 2 | `oa_pull_complete` | OA 拉取完成（含翻页）| `events_count` / `pages_count` / `week_id` / `latency_ms` / `status` |
| 3 | `extract_call` | LLM 抽取调用完成（每次 1 段）| `model` / `prompt_hash`（SHA-256 hex）/ `prompt_tokens` / `completion_tokens` / `total_tokens` / `latency_ms` / `status`（Ok / Timeout / Auth / Unreachable / SchemaMismatch）|
| 4 | `bucket_advance` | 每条合法跃迁 | `from` / `to` / `oa_event_id` / `resolution_id` / `actor_id` / `event_time` |
| 5 | `export_complete` | 6 文件 + sha256 全部落地 | `files_count` / `sha256_manifest_path` / `total_bytes` / `latency_ms` |

### 5.2 段内元数据

每段统一含 `commit_hash`（构建时由 `build.rs` 注入）与 `env_hash`（env 校验通过后对 8 项 env 计算 SHA-256），保证跨周跑批可追溯。

### 5.3 sha256 manifest

`out/<week_id>/sha256_manifest.json` 含 6 文件的 SHA-256 hex 校验和 + `deployed_at` / `week_id` / `tool_version` + `ai_degraded`：

```json
{
  "deployed_at": "2026-10-12T09:30:00+08:00",
  "week_id": "2026-W41",
  "tool_version": "0.1.0",
  "ai_degraded": false,
  "files": {
    "resolutions.csv": "sha256:abc123...",
    "overdue.jsonl":   "sha256:def456...",
    "due_soon.jsonl":  "sha256:ghi789...",
    "conflicts.jsonl": "sha256:jkl012...",
    "summary.json":    "sha256:mno345...",
    "audit_log.jsonl": "sha256:pqr678..."
  }
}
```

下游 OA / 共享 NAS 拷贝前可用 `sha256sum -c sha256_manifest.json` 一键验真。

---

## 6. 关键不变量（不变量 = "删了这条代码就破坏产品语义"）

### 6.1 幂等键

```text
resolution_event 表的复合主键：(week_id, resolution_id)
week_idempotency 表的主键：week_id（hash 写入即可查重）
```

同周第二次跑批（含周一 `cron` 错过后周二补跑）走 `--no-cache` 时，`upsert_idempotent` 按 `(week_id, resolution_id)` 主键查重，已存在不覆盖；同周第一条决议**绝不重复写两次**。

### 6.2 并发 / 重放防护

| 项 | 防护机制 |
|----|----------|
| 决议条目 ID 冲突（同周两条决议条目同号）| CLI 启动时 schema 校验，命中即返回非零退出码（exit 4）|
| OA 事件 ID 重复（同一事件多次回放）| `oa_pull::models::from_dto` 用 `oa_event_id` 作天然去重键，不在 `resolution_event` 表里插入两次 |
| LLM 重复调用（prompt_hash 相同）| `extract::audit::prompt_fingerprint` 计算 SHA-256 hex，`audit_log.jsonl` 段可查重 |

### 6.3 状态校验

| 项 | 校验位 |
|----|--------|
| `Resolution` 5 类缺项 | `extract::validate::validate_resolution` 5 类 `Missing(field)` 拒绝 |
| 5 状态机非法跃迁 | `bucket::advance::advance` 返回 `Err(InvalidTransition)` |
| 周次超界（W00 / W54+）| `cli::cmd::WeekId::parse` 拒绝 |
| `OA` action 不在 5 值 | `oa_pull::models::from_dto` 返回 `Err(OaPullError::SchemaMissing)` |
| `due_at < current_date` | `bucket::due_bucket::bucket` 落到 `Overdue` 桶 |

### 6.4 失败不静默改数据

| 失败类型 | 行为 |
|---------|------|
| OA 拉取失败 | 不写 Sqlite / 不写导出包，返回非零退出码 |
| LLM 不可达 + deterministic 也失败 | 不写导出包（落 `extract_call` 段标 `SchemaMismatch`），返回非零退出码 |
| Schema 校验失败 | 重试 1 次 + 仍失败走 fallback，**最终决议条目数 = 0 时**返回非零退出码 |
| 桶派生失败 | `BucketError::InvalidDate` 即返回非零退出码 |
| Sqlite 写入失败 | 不写导出包（write_atomic 闭包失败 RAII 清理），返回非零退出码 |
| 导出 6 文件任一失败 | RAII 清理 `.tmp/`，`out/<week_id>/` **要么不存在，要么完整** |

### 6.5 同父目录原子重命名

`fs::rename` 在 macOS / Linux 上**同父目录**下原子提交：

```text
out/.tmp/run-<nanos>-<pid>/  ──fs::rename──▶  out/<week_id>/
```

任一文件写入失败 → 闭包返回 Err → `remove_dir_all(.tmp/run-<nanos>-<pid>)` + `remove_dir(out/.tmp/)` → **不留半成品**。重新跑批前 `out/<week_id>/` 要么不存在（上次失败），要么完整（上次成功）。

---

## 7. 与开源参考能力的对照

> 本节把融合后医院产品能力逐项回链到本工具采纳的开源参考能力（**只描述能力，不引用源项目名**）。源产品机制的代码证据见 `spec.md` §6「开源对照」矩阵。

| 融合后医院产品能力 | 采纳的开源参考能力（不引用项目名）| 落地位置 |
|---|---|---|
| **决议条目结构化抽取** | 沙箱 + 类型化校验 | `extract::validate_resolution`（5 类缺项拒绝）+ `extract::deterministic_fallback`（段落切分正则）|
| **决议状态机推进 + 6 桶派生** | 审计链二级索引 + 时间桶 | `bucket::advance::advance`（5 态合法跃迁 + 非法拒绝）+ `bucket::due_bucket::bucket`（6 桶派生）+ `store::schema::CREATE_IDX_OWNER_BUCKET`（二级索引）|
| **强一致任务提交 + 不写半成品** | 强一致 runtime + 确定性执行 | `cli::dispatch::run_handler`（任何 Err 立即返回非零退出码）+ `export::atomic_rename::write_atomic`（同父目录原子 rename）|
| **Schema-first 类型化输出** | Validator → 强类型派生 | `schemas::resolution.rs`（serde + schemars derive）+ `extract::validate`（缺 5 字段立即拒绝）|
| **Env vars 启动注入 + 校验** | withPlatformEnvVars 模式 | `cli::env::validate_env`（8 项 env 校验 + 缺失立即报错）+ `cli::cmd::ResoCli`（--out-dir / --inbox-dir env 自动注入）|

---

## 8. 模块依赖图

```
cli
 ├── schemas        (serde + schemars 派生)
 ├── oa_pull        (reqwest + rusqlite 0)
 │    └── schemas
 ├── extract        (reqwest + pulldown-cmark + regex)
 │    ├── schemas
 │    └── oa_pull::OaEvent (重用 types)
 ├── bucket         (chrono 算术)
 │    └── schemas
 ├── store          (rusqlite bundled)
 │    └── schemas
 └── export         (csv + json + sha2)
      └── schemas
```

**层级规则**：
- `cli` 是顶层入口（4 子命令 `run` / `ingest` / `verify` / `sign`），依赖所有底层模块
- `schemas` 不依赖任何模块（pure data + serde derive）
- `oa_pull` / `extract` / `bucket` / `store` / `export` 互相不依赖（除通过 `schemas` 共享类型）
- `extract` 重用 `oa_pull::OaEvent` 类型，避免重新定义

---

## 9. 部署与运维

| 项 | 规范 |
|---|------|
| **部署位置** | 院办工作站（堡垒机内网），不部署到公网 |
| **凭据来源** | `OA_API_BEARER` / `OA_QUERY_DSN` / `MODEL_API_KEY` 写到 `.env`（由信息科发版），不写到个人电脑 |
| **产物落地** | `out/<week_id>/` 同步到院办 NAS 共享目录供科长打开 |
| **运维 cron** | `cron` / Windows 任务计划 周一上午 09:30 跑 `reso run --week current`；失败告警位留 `WECOM_WEBHOOK_URL` 接口（**本轮不实现推送**）|
| **升级路径** | `cargo build --release` 产出 `target/release/reso` 单文件二进制，复制替换即可（Sqlite 文件就地升级，不需 migrate 数据）|
| **故障排查** | 先看 `audit_log.jsonl` 的 `cli_start` 段 → 确认 `env_hash` 与上周一致；再看 `oa_pull_complete` 段的 `events_count`；最后看 `export_complete` 段的 `total_bytes` 与 `sha256_manifest.json` 的 `deployed_at` |

---

## 10. 与其他文档的关系

| 文档 | 视角 | 受众 |
|------|------|------|
| `README.md`（项目根）| **用户视角**：快速开始 + 命令 + 典型场景 | 院办秘书 / 信息科工程师 |
| `docs/integration.md` | **外部视角**：OA 对接规范 | 信息科 / OA 系统集成方 |
| `docs/architecture.md`（本文）| **内部视角**：数据流 + 状态机 + 审计链 | 二次开发者 / 信息科 |
| `docs/examples.md`（task 14 实现）| **样本视角**：导出包片段 | 信息科 QA / 测试 |
| `docs/concepts/visual-goal.md`（task 14 实现）| **视觉目标**：3 状态描述 | 见 README 文末插页 |
| `spec.md`（round 根）| **产品规约**：必填字段 + 工作闭环 | 跨轮追溯 / 项目评审 |
| `plan.md`（round 根）| **实现规划**：模块与任务对应 | |