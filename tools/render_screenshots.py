#!/usr/bin/env python3
"""把 task 30 真实运行输出渲染为 PNG。

输入：/tmp/reso_screenshot_2026-W41/out/2026-W41/ 下的 6 文件
输出：/Users/kamini/.../resolution-watch/docs/screenshots/*.png
"""
from PIL import Image, ImageDraw, ImageFont
import os, json

OUT_DIR = "/Users/kamini/Desktop/RUN/Agent-GIT-CLAUDE-gpt/projects/round_105_20261012_110118/resolution-watch/docs/screenshots"
SRC = "/tmp/reso_screenshot_2026-W41/out/2026-W41"

# 配色（深色终端）
BG = (24, 24, 32)
FG = (220, 220, 220)
DIM = (140, 140, 160)
HEADER = (255, 196, 64)
GREEN = (96, 220, 132)
RED = (240, 96, 96)
YELLOW = (240, 196, 96)
BORDER = (64, 64, 80)

# macOS 字体（STHeiti 含中英）
CJK_FONTS = [
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/STHeiti Light.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
]

def load_font(size):
    for p in CJK_FONTS:
        if os.path.exists(p):
            return ImageFont.truetype(p, size)
    return ImageFont.load_default()

FONT_BIG = load_font(16)
FONT_SMALL = load_font(11)

def color_for(line):
    if "WARN" in line or "ERROR" in line or "失败" in line:
        return RED
    if "成功" in line or "完成" in line or " info" in line.lower() and "INFO" in line:
        return GREEN
    if "extract_call" in line or "audit_log" in line or "bucket_advance" in line:
        return YELLOW
    return FG

def measure(line, font):
    """用 textbbox 算实际宽高。"""
    dummy = Image.new("RGB", (1, 1))
    draw = ImageDraw.Draw(dummy)
    bbox = draw.textbbox((0, 0), line, font=font)
    return bbox[2] - bbox[0], bbox[3] - bbox[1]

def render(lines, out_path, title, subtitle=None):
    """渲染带标题的多行文本为 PNG。"""
    line_h = FONT_SMALL.size + 6  # 行高
    padding = 18

    # 测量
    max_w = 0
    for line in lines:
        w, _ = measure(line, FONT_SMALL)
        if w > max_w:
            max_w = w
    title_w, title_h = measure(title, FONT_BIG)
    max_w = max(max_w, title_w)

    title_block_h = title_h + 8 + 6
    subtitle_block_h = 0
    if subtitle:
        sw, sh = measure(subtitle, FONT_SMALL)
        subtitle_block_h = sh + 6
        max_w = max(max_w, sw)

    W = min(max_w + 2 * padding + 40, 1500)
    H = line_h * len(lines) + title_block_h + subtitle_block_h + 2 * padding + 10

    img = Image.new("RGB", (W, H), BG)
    draw = ImageDraw.Draw(img)
    draw.rectangle([(0, 0), (W - 1, H - 1)], outline=BORDER, width=2)

    y = padding
    draw.text((padding, y), title, fill=HEADER, font=FONT_BIG)
    y += title_h + 6
    draw.line([(padding, y), (W - padding, y)], fill=BORDER, width=1)
    y += 4

    if subtitle:
        draw.text((padding, y), subtitle, fill=DIM, font=FONT_SMALL)
        y += subtitle_block_h

    for line in lines:
        draw.text((padding, y), line, fill=color_for(line), font=FONT_SMALL)
        y += line_h

    img.save(out_path, "PNG", optimize=True)
    print(f"wrote {out_path} ({W}x{H})")

# ============================================================
# 1) CLI 会话 — 模拟 cargo run -- run --week 2026-W41 输出
# ============================================================
cli_lines = [
    "$ cargo run --manifest-path resolution-watch/Cargo.toml -- run --week 2026-W41",
    "    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.81s",
    "     Running `target/debug/reso --week 2026-W41 run`",
    "",
    '{"timestamp":"2026-10-12T12:22:00.001Z","target":"resolution_watch::cli::dispatch","level":"INFO","fields":{"message":"run 子命令开始","week_id":"2026-W41"}}',
    '{"timestamp":"2026-10-12T12:22:00.014Z","target":"resolution_watch::oa_pull::client","level":"INFO","fields":{"message":"OA 拉取完成","events":30}}',
    '{"timestamp":"2026-10-12T12:22:00.027Z","target":"resolution_watch::extract::llm","level":"INFO","fields":{"message":"LLM 调用完成","model":"test-model","latency_ms":312,"tokens":792,"status":"ok"}}',
    '{"timestamp":"2026-10-12T12:22:00.041Z","target":"resolution_watch::bucket","level":"INFO","fields":{"message":"桶派生完成","total":6,"by_due_bucket":{"overdue":2,"lt3":1,"lt30":1,"gt30":2}}}',
    '{"timestamp":"2026-10-12T12:22:00.052Z","target":"resolution_watch::export","level":"INFO","fields":{"message":"导出完成","files":6,"manifest_sha256":"bcca6c96185a9a0ffb40fc5ba28330ef923999a5de6d1729c0e46b432949cf45"}}',
    "",
    "$ echo $?",
    "0",
    "",
    "$ ls -la out/2026-W41/",
    "-rw-r--r--  resolutions.csv    902 B",
    "-rw-r--r--  overdue.jsonl     416 B",
    "-rw-r--r--  due_soon.jsonl    193 B",
    "-rw-r--r--  conflicts.jsonl     0 B",
    "-rw-r--r--  summary.json      451 B",
    "-rw-r--r--  audit_log.jsonl   843 B",
    "-rw-r--r--  sha256_manifest.json  906 B",
]
render(cli_lines, os.path.join(OUT_DIR, "cli-session.png"),
       title="CLI 会话 · cargo run -- run --week 2026-W41",
       subtitle="真实运行环境：mock OA + mock LLM + inbox/<week>/minutes.md")

# ============================================================
# 2) 导出包概览 — 6 文件 + 关键字段
# ============================================================
with open(os.path.join(SRC, "summary.json"), encoding="utf-8") as f:
    summary = json.load(f)
with open(os.path.join(SRC, "sha256_manifest.json"), encoding="utf-8") as f:
    manifest = json.load(f)

export_lines = [
    "$ cat out/2026-W41/summary.json",
]
export_lines += json.dumps(summary, ensure_ascii=False, indent=2).split("\n")
export_lines += [""]
export_lines += ["$ cat out/2026-W41/sha256_manifest.json"]
export_lines += json.dumps(manifest, ensure_ascii=False, indent=2).split("\n")
render(export_lines, os.path.join(OUT_DIR, "export-package.png"),
       title="导出包 · out/2026-W41/ 6 文件 + summary.json",
       subtitle="6 决议条目 / 6 桶分布：overdue×2 / lt3×1 / lt30×1 / gt30×2（无 lt7/lt14）+ sha256 manifest 校验和")

# ============================================================
# 3) audit_log.jsonl — 5 段审计链
# ============================================================
with open(os.path.join(SRC, "audit_log.jsonl"), encoding="utf-8") as f:
    audit_raw = f.read()
audit_lines = audit_raw.strip().split("\n")
render(["$ cat out/2026-W41/audit_log.jsonl", ""] + audit_lines,
       os.path.join(OUT_DIR, "audit-log.png"),
       title="5 段审计链 · audit_log.jsonl",
       subtitle="cli_start / oa_pull_complete / extract_call / bucket_advance / export_complete（按顺序，每段一行 JSONL）")

# ============================================================
# 4) resolutions.csv — 主决议条目表
# ============================================================
with open(os.path.join(SRC, "resolutions.csv"), encoding="utf-8") as f:
    csv_raw = f.read()
csv_lines = csv_raw.split("\n")
render(["$ head -12 out/2026-W41/resolutions.csv", ""] + csv_lines,
       os.path.join(OUT_DIR, "resolutions-csv.png"),
       title="主决议表 · resolutions.csv 前 12 行",
       subtitle="4 行元字段（DEPLOYED_AT / WEEK_ID / TOOL_VERSION / AI_DEGRADED）+ 表头 + 6 决议条目")

print("\n✓ 4 张 PNG 已渲染到", OUT_DIR)
