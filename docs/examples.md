# 输出样例与导出包片段

> 本文档面向**院办秘书 / 院办督办员 / 信息科**：展示 `reso run --week 2026-W41` 跑通
> 后，秘书在 `out/2026-W41/` 下会看到哪些文件、每个文件长什么样、字段怎么读。
> 配套 `docs/architecture.md`（数据流与状态机）和 `docs/integration.md`（OA 对接规范）使用。
>
> **本样例**基于 `tests/fixtures/week_2026-W41_*` 三件套（6 条决议 + 30 条 OA 事件 + LLM 响应），
> 取 **current_date = 2026-10-12**（会议日，周一上午 09:00 跑批）作为派生基准。
> 与真实跑批唯一差别是真实场景中 OA 事件由拉取端点产生、跑批时刻会随秘书实际节奏浮动。

---

## 1. 目录结构（一次跑批的产物）

```
out/2026-W41/
├── resolutions.csv          # 6 条决议条目 + 状态 + 桶（9 列表格，UTF-8 BOM 可选）
├── overdue.jsonl            # 逾期桶（due_in_days < 0）决议清单，每行一条 JSON
├── due_soon.jsonl           # 临近 deadline（≤ 3 天）决议清单
├── conflicts.jsonl          # OA 与决议条目口径不一致项（一般为空，有冲突时输出）
├── summary.json             # 4+1 桶统计（总数 / by_status / by_department / by_due_bucket / ai_degraded）
├── audit_log.jsonl          # 5 段审计链（cli_start → oa_pull_complete → extract_call → bucket_advance → export_complete）
└── sha256_manifest.json     # 上述 6 文件的 SHA-256 校验和（防签收后被改动）
```

> **持久化副产物**（不在导出包内，路径由 `RESO_DB_PATH` env 决定）：
> `.reso_cache.sqlite` —— 决议推进表 + 周次幂等键，仅供本工具内下次跑批复用。

---

## 2. `resolutions.csv` —— 6 条决议条目全表

CSV 列定义（共 9 列）：

| # | 列名 | 来源 | 说明 |
|---|------|------|------|
| 1 | `resolution_id` | `Resolution::resolution_id` | 周内唯一编号 `#[W<NN>]NN` |
| 2 | `title` | `Resolution::title` | 决议标题，≤ 80 字符 |
| 3 | `owner_department` | `Resolution::owner_department` | 责任科室，命中 `config/departments.yaml` 字典 |
| 4 | `owner_id` | `Resolution::owner_id` | 责任人 / 工号 |
| 5 | `due_at` | `Resolution::due_at` | 截止日期 `YYYY-MM-DD` |
| 6 | `acceptance` | `Resolution::acceptance` | 验收标准，≤ 200 字符 |
| 7 | `status` | `bucket::advance` 派生 | 5 态：`pending` / `in_progress` / `due_soon` / `overdue` / `completed` |
| 8 | `due_in_days` | `bucket::due_bucket` 派生 | `due_at - current_date`，负值 = 已逾期 |
| 9 | `due_bucket` | `bucket::due_bucket` 派生 | 6 桶：`overdue` / `lt3` / `lt7` / `lt14` / `lt30` / `gt30` |

**前 6 行（节录，2026-W41 全表）：**

```csv
# deployed_at=2026-10-12T09:15:00+08:00; week_id=2026-W41; tool_version=0.1.0; ai_degraded=false
resolution_id,title,owner_department,owner_id,due_at,acceptance,status,due_in_days,due_bucket
#[W41]01,试点开展 DRG 入组校验,医务科,100086,2026-10-08,骨科 3 个病组入组一致率 ≥ 95%,overdue,-4,overdue
#[W41]02,门诊电子病历升级,信息科,200002,2026-10-13,门诊 5 个科室全部切换到 EMR 5.0,pending,1,lt3
#[W41]03,采购呼吸机 5 台,设备科,300003,2026-11-10,5 台呼吸机到货验收,pending,29,lt30
#[W41]04,医保结算清单格式与流程优化,医保办,400004,2026-10-05,医保结算清单按周生成,overdue,-7,overdue
#[W41]05,护理文书质控专项,护理部,500005,2026-12-31,护理文书缺项率 ≤ 5%,pending,80,gt30
#[W41]06,年度能耗指标控制,后勤科,600006,2026-12-20,单位面积能耗较去年下降 3%,pending,69,gt30
```

> **第 1 行注释**（`# deployed_at=...`）由 `export::atomic_rename::ExportMetadata::to_csv_header` 写出。
> CSV 解析器遇到 `#` 开头的行自动跳过；下游若用 Excel 打开会被当作隐藏批注。

---

## 3. `overdue.jsonl` —— 逾期桶（2 条）

```jsonl
{"resolution_id":"#[W41]01","title":"试点开展 DRG 入组校验","owner_department":"医务科","owner_id":"100086","due_at":"2026-10-08","status":"overdue","due_in_days":-4,"due_bucket":"overdue"}
{"resolution_id":"#[W41]04","title":"医保结算清单格式与流程优化","owner_department":"医保办","owner_id":"400004","due_at":"2026-10-05","status":"overdue","due_in_days":-7,"due_bucket":"overdue"}
```

每行一条 `ResolutionJsonlEntry`（见 `src/export/jsonl_export.rs`），9 字段一一对应。
秘书拿到 `overdue.jsonl` 直接转发给院办主任/分管院长，附一句「请本周内督办收口」。

---

## 4. `due_soon.jsonl` —— 临近 deadline（1 条）

```jsonl
{"resolution_id":"#[W41]02","title":"门诊电子病历升级","owner_department":"信息科","owner_id":"200002","due_at":"2026-10-13","status":"pending","due_in_days":1,"due_bucket":"lt3"}
```

> 桶定义见 `bucket::due_bucket::from_days`：`due_in_days ∈ [0, 3]` 进 `lt3`。
> 包含当天（`days=0`）和临界值 `days=3`，秘书一眼看到的就是「今天/明天/后天要交」。

---

## 5. `conflicts.jsonl` —— OA 与决议条目口径不一致

本样例**无冲突**，文件为空（0 字节）。一旦出现冲突，格式如下：

```jsonl
{"resolution_id":"#[W41]01","kind":"oa_status_mismatch","detail":"OA 标 pending，科室 10-15 汇报已落地但 OA 无 in_progress 事件"}
{"resolution_id":"#[W41]02","kind":"missing_owner","detail":"OA 流转事件 actor_id=E20002 与 minutes.md 工号 200002 不一致"}
```

| `kind` 取值 | 含义 | 谁来处理 |
|-------------|------|----------|
| `oa_status_mismatch` | OA 端 action 与科室汇报不一致 | 院办秘书复核 OA 端事件 |
| `missing_owner` | OA 事件触发人未在决议条目责任人工号列表 | 院办秘书 + 信息科 |
| `due_at_mismatch` | OA 事件 timestamp 与 minutes.md due_at 差 > 7 天 | 院办秘书 + 责任科室 |
| `resolution_id_not_in_minutes` | OA 事件引用了 minutes.md 不存在的 resolution_id | 信息科（数据源问题） |

---

## 6. `summary.json` —— 4+1 桶统计

```json
{
  "deployed_at": "2026-10-12T09:15:00+08:00",
  "week_id": "2026-W41",
  "tool_version": "0.1.0",
  "ai_degraded": false,
  "total": 6,
  "by_status": {
    "pending": 4,
    "overdue": 2
  },
  "by_department": {
    "医务科": 1,
    "信息科": 1,
    "设备科": 1,
    "医保办": 1,
    "护理部": 1,
    "后勤科": 1
  },
  "by_due_bucket": {
    "gt30": 2,
    "lt3": 1,
    "lt30": 1,
    "overdue": 2
  },
  "overdue_count": 2,
  "due_soon_count": 1
}
```

字段口径：

- `total` —— 决议条数
- `by_status` —— 5 态分桶（`pending` / `in_progress` / `due_soon` / `overdue` / `completed`）
- `by_department` —— 责任科室分桶（命中 `config/departments.yaml` 字典的科室名）
- `by_due_bucket` —— 6 桶分桶（`overdue` / `lt3` / `lt7` / `lt14` / `lt30` / `gt30`）
- `overdue_count` —— 冗余字段（与 `by_due_bucket["overdue"]` 一致，便于手机/微信网关直接读 int）
- `due_soon_count` —— 冗余字段（与 `by_due_bucket["lt3"]` 一致）
- `ai_degraded` —— `true` 表示本次跑批走了 deterministic_fallback，未用 LLM 抽取；秘书需在月度汇报时点出

---

## 7. `audit_log.jsonl` —— 5 段审计链

```jsonl
{"segment":"cli_start","tool_version":"0.1.0","commit_hash":"a1b2c3d4","env_hash":"7f8e9d0c","model_name":null,"prompt_hash":null,"latency_ms":42,"status":"ok"}
{"segment":"oa_pull_complete","tool_version":"0.1.0","commit_hash":"a1b2c3d4","env_hash":"7f8e9d0c","model_name":null,"prompt_hash":null,"latency_ms":1284,"status":"ok"}
{"segment":"extract_call","tool_version":"0.1.0","commit_hash":"a1b2c3d4","env_hash":"7f8e9d0c","model_name":"gpt-4o-mini","prompt_hash":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","latency_ms":2310,"status":"ok"}
{"segment":"bucket_advance","tool_version":"0.1.0","commit_hash":"a1b2c3d4","env_hash":"7f8e9d0c","model_name":null,"prompt_hash":null,"latency_ms":18,"status":"ok"}
{"segment":"export_complete","tool_version":"0.1.0","commit_hash":"a1b2c3d4","env_hash":"7f8e9d0c","model_name":null,"prompt_hash":null,"latency_ms":207,"status":"ok"}
```

5 段顺序固定：

| 段 | 触发时刻 | 关键字段 | 失败影响 |
|----|----------|----------|----------|
| `cli_start` | 进程启动后第一行 | `env_hash`（8 字符 SHA-256 前缀） | 启动失败不写审计 |
| `oa_pull_complete` | OA 翻页结束 | `latency_ms` | 失败时 `status="unreachable"` |
| `extract_call` | LLM 抽取结束 / fallback 完成 | `model_name` / `prompt_hash`（SHA-256 hex 64 字符） | 失败时 `status="fallback"` 或 `"schema_mismatch"` |
| `bucket_advance` | 状态机推进 + 6 桶派生完成 | `latency_ms` | 失败时 `status="invalid_transition"` |
| `export_complete` | 6 文件全部原子重命名完成 | `latency_ms` | 失败时回滚 `out/.tmp/` 临时目录 |

---

## 8. `sha256_manifest.json` —— 6 文件 SHA-256 校验和

```json
{
  "week_id": "2026-W41",
  "generated_at": "2026-10-12T09:15:01+08:00",
  "files": [
    {"path": "resolutions.csv", "sha256": "8c3f5a1e2b9d4f6c0e7a8b5d2c1e9f0a4b6d8c2e5f7a9b1c3d5e7f9a1b3c5d7e"},
    {"path": "overdue.jsonl", "sha256": "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192837465768798a0"},
    {"path": "due_soon.jsonl", "sha256": "9b8a7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1e0f9a8b"},
    {"path": "summary.json", "sha256": "4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e"},
    {"path": "audit_log.jsonl", "sha256": "7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f"},
    {"path": "conflicts.jsonl", "sha256": "0e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f"}
  ]
}
```

> 秘书/科长签字后，本 manifest 一并交付给信息科归档。后续任何人改动 6 文件之一，
> 再算一次 SHA-256 就能立即发现——**签收后的不可篡改性**是 sha256 manifest 的全部价值。

---

## 9. 字段口径速查（与 `src/schemas/resolution.rs` 一致）

| 概念 | 类型 | snake_case 取值 | 说明 |
|------|------|----------------|------|
| 决议状态 | enum | `pending` / `in_progress` / `due_soon` / `overdue` / `completed` | 5 态，4 活跃 + 1 终态 |
| OA 动作 | enum | `started` / `in_progress` / `blocked` / `submitted` / `completed` | 5 动作，驱动状态机跃迁 |
| 剩余天数桶 | enum | `overdue` / `lt3` / `lt7` / `lt14` / `lt30` / `gt30` | 6 桶，覆盖 `due_in_days ∈ [-∞, +∞)` |
| 审计段 | enum | `cli_start` / `oa_pull_complete` / `extract_call` / `bucket_advance` / `export_complete` | 5 段，按此顺序写入 |

> 任何字段名变更须**先**改 `src/schemas/resolution.rs` 派生，`docs/examples.md` 与 README
> 「输出样例」同步修订；否则秘书拿到的 csv/jsonl 与本规范对不上，签收环节会被退回。

---

## 10. 与本目录下其他文档的关系

| 文档 | 视角 | 谁读 |
|------|------|------|
| **`docs/examples.md`（本文）** | 输出形态（用户向） | 院办秘书 / 院办督办员 |
| `docs/architecture.md` | 内部数据流 + 状态机 + 审计链（代码向） | 信息科工程师 / 二次开发者 |
| `docs/integration.md` | OA 对接规范（外部视角） | 信息科 / OA 系统集成方 |
| `docs/concepts/visual-goal.md` | 视觉目标卡（设计向） | UI 改版时参考 |
| `README.md` | 产品说明（混合视角） | 所有读者 |
