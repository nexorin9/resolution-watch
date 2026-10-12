# 视觉目标卡（visual-goal）

> 本文档是 `resolution-watch` 的**结构化视觉目标**：在没有图像生成能力的 CI 环境下，
> 用文字 + ASCII 描述产品应有的画面，作为 Phase 4 收尾（task 30 采集真实截图）和
> README「概念 / 实测对照」小节的对照基准。
>
> **3 个状态**：起始态 / 首个可用瞬间 / 成形态；**当前交付**为 CLI 批处理形态，
> 视觉目标与之严格对齐（**不**画 Web 仪表盘、**不**画移动端）。

---

## 状态 0：起始态（inbox + out 都空）

**画面**：终端会话，cwd 在项目根目录。

```
$ ls -la inbox/ out/
ls: inbox: No such file or directory
ls: out: No such file or directory
$ ls config/
departments.yaml
$ cat .env.example | head -5
# OA（环境变量读入，缺一即报 exit 1）
OA_API_BEARER=__REPLACE_ME__
OA_QUERY_DSN=__REPLACE_ME__
...
```

**信息层级**：
- 终端黑色背景（`#000000` / `#0F0F0F` 之类）
- 提示符绿色（`#22C55E` 之类 macOS Terminal 默认色）
- 文件名白色 / 系统提示灰色
- 屏幕顶部状态栏（IDE / 编辑器打开本项目时）：左侧仓库名 / 右侧分支 + git status

**约束**：纯 CLI；不画 Web 仪表盘；不画管理后台。

---

## 状态 1：首个可用瞬间（CLI 跑批中，输出第一行汇总）

**画面**：终端会话，正在跑 `reso run --week 2026-W41`，跑批过程中陆续打印结构化日志。

```
$ reso run --week 2026-W41
[2026-10-12T09:00:42+08:00] reso 0.1.0 | commit=a1b2c3d4 | env=7f8e9d0c
[2026-10-12T09:00:42+08:00] [cli_start] tool_version=0.1.0 status=ok latency_ms=12
[2026-10-12T09:00:42+08:00] inbox/2026-W41/minutes.md 不存在，请先把会议纪要落到 inbox/
[2026-10-12T09:00:42+08:00] [error] 退出码 1：inbox 文件缺失（详见 progress.txt）
```

或正常路径（inbox 已落地、OA 端点连通）：

```
$ reso run --week 2026-W41
[2026-10-12T09:00:42+08:00] reso 0.1.0 | commit=a1b2c3d4 | env=7f8e9d0c
[2026-10-12T09:00:42+08:00] [cli_start] tool_version=0.1.0 status=ok latency_ms=12
[2026-10-12T09:00:43+08:00] [oa_pull_complete] events=30 pages=1 cursor=null latency_ms=1284
[2026-10-12T09:00:45+08:00] [extract_call] model=gpt-4o-mini prompt_hash=e3b0...b855 latency_ms=2310 status=ok
[2026-10-12T09:00:45+08:00] [bucket_advance] decisions=6 buckets={overdue:2, lt3:1, lt30:1, gt30:2} latency_ms=18
[2026-10-12T09:00:45+08:00] [export_complete] files=6 out_dir=out/2026-W41/ latency_ms=207
$ echo $?
0
```

**信息层级**：
- `[时间戳]` 灰色（`#9CA3AF` 之类）
- `reso 0.1.0 | commit=...` 亮白色加粗
- `[段名]` 蓝色（`#3B82F6` 之类）
- `status=ok` 绿色（`#22C55E`）
- `status=error` / `status=fallback` 红色（`#EF4444`）/ 黄色（`#F59E0B`）
- 关键数字（决议条数 / 桶分布 / 耗时）加粗白
- 退出码 `0` 绿色高亮

**约束**：
- 真实 terminal 输出，**不**画 IDE 高亮
- 字体族 = 系统默认 monospace（macOS Menlo / Linux DejaVu Sans Mono / Windows Cascadia Code）
- 行高 = 1.2 倍；每行不超过 120 字符（自动换行而非截断）
- 颜色仅 stderr JSON 日志与 stdout 人类可读输出两个通道，**不**用 ANSI 16 色

---

## 状态 2：成形态（out/2026-W41/ 下 6 文件 + sha256 manifest + 终端收尾汇总）

**画面**：终端会话，跑批结束 + 列出 out/ 目录 + 用 `cat summary.json` 给出最关键的 4+1 桶数字。

```
$ ls -la out/2026-W41/
total 56
drwxr-xr-x@ 8 kamini  staff   256 Oct 12 09:15 .
drwx------@ 3 kamini  staff    96 Oct 12 09:00 ..
-rw-r--r--@ 1 kamini  staff  1024 Oct 12 09:15 audit_log.jsonl
-rw-r--r--@ 1 kamini  staff    0 Oct 12 09:15 conflicts.jsonl
-rw-r--r--@ 1 kamini  staff   86 Oct 12 09:15 due_soon.jsonl
-rw-r--r--@ 1 kamini  staff  173 Oct 12 09:15 overdue.jsonl
-rw-r--r--@ 1 kamini  staff  1652 Oct 12 09:15 resolutions.csv
-rw-r--r--@ 1 kamini  staff   94 Oct 12 09:15 sha256_manifest.json
-rw-r--r--@ 1 kamini  staff  483 Oct 12 09:15 summary.json

$ cat out/2026-W41/summary.json | jq '{total, by_status, overdue_count, due_soon_count}'
{
  "total": 6,
  "by_status": {
    "overdue": 2,
    "pending": 4
  },
  "overdue_count": 2,
  "due_soon_count": 1
}

$ cat out/2026-W41/overdue.jsonl | jq -r '"\(.resolution_id) \(.title) 距今\(.due_in_days)天"'
#[W41]01 试点开展 DRG 入组校验 距今-4天
#[W41]04 医保结算清单格式与流程优化 距今-7天
```

**信息层级（成形态）**：
- 终端背景仍为黑色；`ls -la` 输出按 macOS `ls` 默认配色（目录蓝色 / 文件白色 / 可执行绿色）
- `summary.json` JSON 高亮：键名青色（`#06B6D4` 之类）、字符串值绿色、数字值黄色
- `overdue.jsonl` 用 `jq` 抽出的中文字段名（`决议标题`、`距今天数`）保持原样，不二次翻译

---

## 必须可达的约束（防止画出技术栈做不出的效果）

| 维度 | 可用 | 不可用 |
|------|------|--------|
| 终端 | ✅ 系统默认 terminal + 真实 CLI 输出 | ❌ 任何 IDE 截屏、VS Code 高亮、PowerShell 主题 |
| 字体 | ✅ monospace（系统默认） | ❌ Web font、CJK 美术字（如思源宋体）、手写体 |
| 颜色 | ✅ ANSI 16 色 / 256 色（stderr JSON 自动 escape） | ❌ 真彩色（24-bit）以外的「伪设计稿渐变」 |
| 图形 | ✅ ASCII 数据流图（用 `┌─┐│└┘▼` 字符） | ❌ mermaid、SVG 嵌入、Unicode emoji 在终端中显示的位图 |
| 布局 | ✅ 终端逐行输出（每行 ≤ 120 字符） | ❌ 网格、表格、仪表盘、分屏 |
| 动画 | ✅ 行逐步打印（按真实时间） | ❌ 进度条、spinner、骨架屏（CLI 无标准） |
| 图片 | ❌ 不画 | ❌ 不放 PNG / JPEG |
| 数据 | ✅ 6 条决议 + 30 条 OA 事件（fixture 跑通即可重现） | ❌ 真实医院数据（脱敏替身也只能用 fixture 量级） |

> **本环境无图像生成能力**（macOS darwin 25.5.0，未安装 SD / DALL-E 客户端），
> 按 `CLAUDE.md` 「没有图像能力时只写结构化视觉目标卡，不要伪造图片」执行。
> 真实截图采集在 task 30（`docs/screenshots/`）落盘。

---

## 与 README「概念 / 实测对照」小节的关系

- `docs/concepts/visual-goal.md`（本文）= **目标侧**（设计意图）
- `docs/screenshots/`（task 30 落盘）= **实测侧**（真实跑批截屏）

README「概念 / 实测对照」小节引用**两侧**，并写一句诚实标注：

> 目标与实测在视觉上**必然有差距**：终端配色受宿主 shell 主题影响、字体受操作系统影响、
> 行宽受终端宽度影响。本文档是结构化「靶子」，实测截图是真实「弹着点」，
> 两者在文字 + 数据上**应当一致**（决议条数、桶分布、字段名），在像素级
> **不必**逐字符对照。

---

## 配色色值参考（终端友好，host 自适应）

> 终端应用**不应**写死 ANSI 颜色码；下表是 macOS Terminal / iTerm2 / Linux gnome-terminal
> 默认主题下常见的近似色值，供截图评估时参考（**不**作为代码内硬编码）。

| 用途 | 建议色 | ANSI 近似 | 备注 |
|------|--------|-----------|------|
| 时间戳 | `#9CA3AF` 灰 | `\x1b[90m` | 不抢主视觉 |
| 段名（`[cli_start]` 等） | `#3B82F6` 蓝 | `\x1b[94m` | 唯一识别 |
| 状态 ok | `#22C55E` 绿 | `\x1b[32m` | 进度正向 |
| 状态 error | `#EF4444` 红 | `\x1b[31m` | 阻塞显式 |
| 状态 fallback | `#F59E0B` 黄 | `\x1b[33m` | 降级提示 |
| 关键数字 | `#FFFFFF` 白加粗 | `\x1b[1;37m` | 数据点 |
| 提示符 `$` | 系统默认绿 | - | 不动 |

> **不**用紫色 / 品红 / 青绿色 / 亮蓝等装饰色——终端配色一致性比「漂亮」更重要。
> 当前实现未在 stdout 写 ANSI 颜色码（仅 stderr JSON）；上述色值仅在**真实截图采集**
> 时由宿主 terminal 主题提供，作为视觉目标参考。
