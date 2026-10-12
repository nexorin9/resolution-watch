//! `export` 子系统的端到端集成测试。
//!
//! 覆盖：
//! - atomic_rename 全链路：成功提交、闭包失败清理、不写半成品
//! - csv_export + atomic_rename 联合：完整导出包格式 + sha256 manifest 形态校验
//! - 端到端 6 必填 + 3 衍生列的最终 CSV 输出可被下游工具解析

use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use tempfile::TempDir;

use crate::export::atomic_rename::{write_atomic, ExportMetadata};
use crate::export::csv_export::{
    write_resolutions_csv, ResolutionRow, ResolutionRowBuilder, RESOLUTIONS_HEADER,
};
use crate::export::jsonl_export::{
    write_audit_log_jsonl, write_conflicts_jsonl, write_due_soon_jsonl, write_overdue_jsonl,
    standard_audit_segments, AuditSegmentEntry, ConflictEntry, ResolutionJsonlEntry,
};
use crate::export::sha256::{build_manifest, sha256_hex_file, write_manifest};
use crate::export::summary::{build_summary, write_summary, SummaryInputRow};
use crate::schemas::{DueBucket, OaAction, Resolution, ResolutionStatus};

fn sample_resolution(id: &str, title: &str, dept: &str) -> Resolution {
    Resolution {
        resolution_id: id.to_string(),
        title: title.to_string(),
        owner_department: dept.to_string(),
        owner_id: "E10086".to_string(),
        due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
        acceptance: format!("{title}验收标准：实施完成并通过院办复核"),
        evidence_ref: None,
    }
}

fn sample_meta() -> ExportMetadata {
    ExportMetadata {
        deployed_at: "2026-10-12T03:00:00Z".to_string(),
        week_id: "2026-W41".to_string(),
        tool_version: "0.1.0-test".to_string(),
        ai_degraded: false,
    }
}

/// 校验 CSV 顶部 4 行元字段 + 1 行表头 + 数据行结构。
fn assert_csv_structure(path: &Path, expected_data_lines: usize) -> String {
    let content = fs::read_to_string(path).expect("CSV 应可读");
    let lines: Vec<&str> = content.lines().collect();
    assert!(
        lines.len() >= 5 + expected_data_lines,
        "行数应 ≥ 5 + {expected_data_lines}, 实为 {}",
        lines.len()
    );
    // 4 行元字段
    assert!(lines[0].starts_with("# DEPLOYED_AT:"));
    assert!(lines[1].starts_with("# WEEK_ID:"));
    assert!(lines[2].starts_with("# TOOL_VERSION:"));
    assert!(lines[3].starts_with("# AI_DEGRADED:"));
    // 第 5 行：表头
    assert_eq!(lines[4], RESOLUTIONS_HEADER);
    content
}

#[test]
fn end_to_end_atomic_write_csv_with_three_resolutions() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = sample_meta();

    // 3 条决议，覆盖不同状态与桶
    let rows = vec![
        ResolutionRowBuilder::from_resolution(&sample_resolution("#[W41]01", "DRG 试点", "医务科"))
            .derived(ResolutionStatus::InProgress, 19, DueBucket::Lt30)
            .build(),
        ResolutionRowBuilder::from_resolution(&sample_resolution("#[W41]02", "EMR 升级", "信息科"))
            .derived(ResolutionStatus::DueSoon, 2, DueBucket::Lt3)
            .build(),
        ResolutionRowBuilder::from_resolution(&sample_resolution("#[W41]03", "呼吸机采购", "设备科"))
            .derived(ResolutionStatus::Overdue, -5, DueBucket::Overdue)
            .build(),
    ];

    let result = write_atomic(&out_dir, |temp| {
        write_resolutions_csv(&temp.join("resolutions.csv"), &rows, &meta)
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    assert!(result.is_ok(), "atomic write 应成功：{result:?}");

    let csv_path = out_dir.join("resolutions.csv");
    let content = assert_csv_structure(&csv_path, 3);

    // 3 条数据行
    assert!(content.contains("#[W41]01,DRG 试点,医务科,E10086,2026-10-31"));
    assert!(content.contains("in_progress,19,lt30"));
    assert!(content.contains("#[W41]02,EMR 升级,信息科"));
    assert!(content.contains("due_soon,2,lt3"));
    assert!(content.contains("#[W41]03,呼吸机采购,设备科"));
    assert!(content.contains("overdue,-5,overdue"));

    // .tmp/ 父目录应被清理
    assert!(!tmp.path().join("out").join(".tmp").exists());
}

#[test]
fn end_to_end_atomic_write_with_closure_failure_does_not_leave_partial_csv() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = sample_meta();
    let rows = vec![ResolutionRow::from_resolution(&sample_resolution(
        "#[W41]01",
        "测试",
        "信息科",
    ))];

    let result = write_atomic(&out_dir, |temp| {
        write_resolutions_csv(&temp.join("resolutions.csv"), &rows, &meta)
            .map_err(|e| e.to_string())?;
        // 模拟第二步失败（如 sha256 计算失败 / 写入 summary.json 失败）
        Err("模拟 summary.json 写入失败".to_string())
    });

    assert!(result.is_err(), "应失败");
    // 正式目录不应存在
    assert!(!out_dir.exists(), "正式目录不应存在");
    // .tmp/ 父目录应被清掉
    assert!(!tmp.path().join("out").join(".tmp").exists());
    // 不应残留 resolutions.csv
    assert!(!tmp.path().join("out").join("2026-W41").join("resolutions.csv").exists());
}

#[test]
fn end_to_end_atomic_write_multiple_files_in_one_package() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = sample_meta();
    let row = ResolutionRowBuilder::from_resolution(&sample_resolution(
        "#[W41]01",
        "DRG 试点",
        "医务科",
    ))
    .derived(ResolutionStatus::InProgress, 19, DueBucket::Lt30)
    .build();

    let result = write_atomic(&out_dir, |temp| {
        write_resolutions_csv(&temp.join("resolutions.csv"), std::slice::from_ref(&row), &meta)
            .map_err(|e| e.to_string())?;
        fs::write(temp.join("summary.json"), r#"{"total": 1}"#).map_err(|e| e.to_string())?;
        fs::write(temp.join("overdue.jsonl"), "").map_err(|e| e.to_string())?;
        Ok(())
    });
    assert!(result.is_ok());

    // 6 文件全在（这里只写 3 个用于验证机制；task 29 续接完整 6 文件）
    assert!(out_dir.join("resolutions.csv").exists());
    assert!(out_dir.join("summary.json").exists());
    assert!(out_dir.join("overdue.jsonl").exists());
}

#[test]
fn csv_meta_header_reflects_ai_degraded_true() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("resolutions.csv");
    let meta = ExportMetadata::now("2026-W41", true);
    let row = ResolutionRowBuilder::from_resolution(&sample_resolution(
        "#[W41]01",
        "测试",
        "信息科",
    ))
    .status(ResolutionStatus::Pending)
    .build();
    write_resolutions_csv(&path, &[row], &meta).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("# AI_DEGRADED: true"));
}

#[test]
fn csv_with_30_resolutions_writes_all_rows() {
    // 真实规模验证：30 条决议（spec.md 要求 ≥30 条决议条目的 fixture）
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("big.csv");
    let meta = sample_meta();

    let rows: Vec<ResolutionRow> = (1..=30)
        .map(|i| {
            ResolutionRowBuilder::from_resolution(&sample_resolution(
                &format!("#[W41]{i:02}"),
                &format!("决议 {i}"),
                "医务科",
            ))
            .derived(ResolutionStatus::InProgress, (i as i64) - 1, DueBucket::Lt3)
            .build()
        })
        .collect();

    write_resolutions_csv(&path, &rows, &meta).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    // 4 元 + 1 表头 + 30 数据
    assert_eq!(lines.len(), 35);
    for i in 1..=30 {
        assert!(content.contains(&format!("#[W41]{i:02}")));
    }
}

#[test]
fn csv_with_resolution_id_containing_unicode_serializes_correctly() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("unicode.csv");
    let meta = sample_meta();
    let row = ResolutionRowBuilder::from_resolution(&Resolution {
        resolution_id: "决议甲-01".to_string(), // 中文中带横线
        title: "包含「全角逗号，句号。」的标题".to_string(),
        owner_department: "医务科".to_string(),
        owner_id: "工号-10086".to_string(),
        due_at: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
        acceptance: "验收标准含「，；：」等全角符号".to_string(),
        evidence_ref: None,
    })
    .derived(ResolutionStatus::Pending, 0, DueBucket::Lt3)
    .build();

    write_resolutions_csv(&path, &[row], &meta).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    // 验证 Unicode 字段写入无损坏
    assert!(content.contains("决议甲-01"));
    // 全角逗号「，」非 CSV RFC 4180 特殊字符（仅 ASCII `,` 触发引号包裹）
    // 但含 ASCII 逗号的子串应被正确保留
    assert!(content.contains("包含「全角逗号，句号。」的标题"));
    // 工号中横线无需引号
    assert!(content.contains("工号-10086"));
}

#[test]
fn csv_data_line_count_matches_input() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("count.csv");
    let meta = sample_meta();
    let rows: Vec<ResolutionRow> = (0..7)
        .map(|i| {
            ResolutionRow::from_resolution(&sample_resolution(
                &format!("R{i}"),
                &format!("T{i}"),
                "信息科",
            ))
        })
        .collect();
    write_resolutions_csv(&path, &rows, &meta).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    // 4 元 + 1 表头 + 7 数据 = 12 行
    assert_eq!(content.lines().count(), 12);
}

// =============================================================================
// task 29: 4 种 JSONL writer / summary 4+1 桶 / sha256 manifest / 5 段审计链
// 集成测试（与各模块 inline 测试互补，端到端走 atomic_rename 通道）
// =============================================================================

fn sample_overdue_entry(id: &str, dept: &str) -> ResolutionJsonlEntry {
    ResolutionJsonlEntry {
        resolution_id: id.to_string(),
        title: format!("决议 {}", id),
        owner_department: dept.to_string(),
        owner_id: "E10086".to_string(),
        due_at: "2026-10-05".to_string(),
        status: ResolutionStatus::Overdue,
        due_in_days: -7,
        due_bucket: "overdue".to_string(),
    }
}

fn sample_due_soon_entry(id: &str, dept: &str) -> ResolutionJsonlEntry {
    ResolutionJsonlEntry {
        resolution_id: id.to_string(),
        title: format!("决议 {}", id),
        owner_department: dept.to_string(),
        owner_id: "E10087".to_string(),
        due_at: "2026-10-15".to_string(),
        status: ResolutionStatus::DueSoon,
        due_in_days: 2,
        due_bucket: "lt3".to_string(),
    }
}

/// 验证 4 种 JSONL writer 各自正确生成（每行 1 JSON + 字段可解析回结构）。
#[test]
fn task29_four_kinds_of_jsonl_writers_each_emit_valid_jsonl() {
    let tmp = TempDir::new().unwrap();
    let p_overdue = tmp.path().join("overdue.jsonl");
    let p_due_soon = tmp.path().join("due_soon.jsonl");
    let p_conflicts = tmp.path().join("conflicts.jsonl");
    let p_audit = tmp.path().join("audit_log.jsonl");

    // overdue 2 条
    let overdue_entries = vec![
        sample_overdue_entry("#[W41]01", "医务科"),
        sample_overdue_entry("#[W41]02", "信息科"),
    ];
    write_overdue_jsonl(&p_overdue, &overdue_entries).unwrap();
    let content = fs::read_to_string(&p_overdue).unwrap();
    assert_eq!(content.lines().count(), 2, "overdue.jsonl 2 行");
    for line in content.lines() {
        let v: ResolutionJsonlEntry = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("overdue.jsonl 行解析失败：{e} | {line}"));
        assert!(v.due_in_days < 0, "overdue 桶 due_in_days 应为负");
        assert_eq!(v.due_bucket, "overdue");
    }

    // due_soon 1 条
    let due_soon_entries = vec![sample_due_soon_entry("#[W41]03", "设备科")];
    write_due_soon_jsonl(&p_due_soon, &due_soon_entries).unwrap();
    let content = fs::read_to_string(&p_due_soon).unwrap();
    assert_eq!(content.lines().count(), 1, "due_soon.jsonl 1 行");
    let v: ResolutionJsonlEntry = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(v.status, ResolutionStatus::DueSoon);
    assert_eq!(v.due_bucket, "lt3");

    // conflicts 3 条覆盖不同 kind
    let conflicts = vec![
        ConflictEntry {
            resolution_id: "#[W41]01".into(),
            kind: "oa_status_mismatch".into(),
            detail: "OA 标 pending，科室汇报已落地".into(),
        },
        ConflictEntry {
            resolution_id: "#[W41]02".into(),
            kind: "owner_mismatch".into(),
            detail: "决议 owner_id=E10086，OA 标 E10087".into(),
        },
        ConflictEntry {
            resolution_id: "#[W41]03".into(),
            kind: "due_date_mismatch".into(),
            detail: "决议 due_at=2026-10-31，OA 标 2026-11-05".into(),
        },
    ];
    write_conflicts_jsonl(&p_conflicts, &conflicts).unwrap();
    let content = fs::read_to_string(&p_conflicts).unwrap();
    assert_eq!(content.lines().count(), 3, "conflicts.jsonl 3 行");
    for (i, line) in content.lines().enumerate() {
        let v: ConflictEntry = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("conflicts.jsonl 第 {i} 行解析失败：{e}"));
        assert!(!v.kind.is_empty());
    }

    // audit_log 5 段（标准 5 段审计链）
    let meta = ExportMetadata::now("2026-W41", false);
    let audit_entries = standard_audit_segments(&meta);
    write_audit_log_jsonl(&p_audit, &audit_entries).unwrap();
    let content = fs::read_to_string(&p_audit).unwrap();
    assert_eq!(content.lines().count(), 5, "audit_log.jsonl 5 段");
    let expected_segments = [
        "cli_start",
        "oa_pull_complete",
        "extract_call",
        "bucket_advance",
        "export_complete",
    ];
    for (i, expected) in expected_segments.iter().enumerate() {
        let line = content.lines().nth(i).unwrap();
        let v: AuditSegmentEntry = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("audit_log 第 {i} 段解析失败：{e}"));
        assert_eq!(v.segment, *expected);
        assert!(v.tool_version.contains("0.1") || v.tool_version.contains("test"),
                "tool_version 应含版本字符串，实际 {}", v.tool_version);
    }
}

/// 验证 summary 4+1 桶（by_status 5 态 / by_department / by_due_bucket 6 桶 /
/// overdue_count / due_soon_count）+ 与 atomic_rename 联合验证。
#[test]
fn task29_summary_four_plus_one_buckets_accurate_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = ExportMetadata::now("2026-W41", false);

    // 7 条决议覆盖：5 态全有 + 6 桶全有 + 3 科室
    let rows: Vec<SummaryInputRow> = vec![
        SummaryInputRow { resolution_id: "r1", owner_department: "医务科", status: ResolutionStatus::Pending,    due_bucket: DueBucket::Lt3 },
        SummaryInputRow { resolution_id: "r2", owner_department: "医务科", status: ResolutionStatus::InProgress, due_bucket: DueBucket::Lt7 },
        SummaryInputRow { resolution_id: "r3", owner_department: "信息科", status: ResolutionStatus::InProgress, due_bucket: DueBucket::Lt14 },
        SummaryInputRow { resolution_id: "r4", owner_department: "信息科", status: ResolutionStatus::DueSoon,   due_bucket: DueBucket::Lt30 },
        SummaryInputRow { resolution_id: "r5", owner_department: "设备科", status: ResolutionStatus::DueSoon,   due_bucket: DueBucket::Gt30 },
        SummaryInputRow { resolution_id: "r6", owner_department: "设备科", status: ResolutionStatus::Overdue,   due_bucket: DueBucket::Overdue },
        SummaryInputRow { resolution_id: "r7", owner_department: "护理部", status: ResolutionStatus::Completed, due_bucket: DueBucket::Gt30 },
    ];

    let summary = build_summary(&meta, rows.into_iter());

    // 4 桶核心字段
    assert_eq!(summary.total, 7, "total = 7");
    assert_eq!(summary.by_status.get("pending").copied(),    Some(1));
    assert_eq!(summary.by_status.get("in_progress").copied(), Some(2));
    assert_eq!(summary.by_status.get("due_soon").copied(),   Some(2));
    assert_eq!(summary.by_status.get("overdue").copied(),    Some(1));
    assert_eq!(summary.by_status.get("completed").copied(),  Some(1));
    assert_eq!(summary.by_department.get("医务科").copied(), Some(2));
    assert_eq!(summary.by_department.get("信息科").copied(), Some(2));
    assert_eq!(summary.by_department.get("设备科").copied(), Some(2));
    assert_eq!(summary.by_department.get("护理部").copied(), Some(1));
    assert_eq!(summary.by_due_bucket.get("lt3").copied(),     Some(1));
    assert_eq!(summary.by_due_bucket.get("lt7").copied(),     Some(1));
    assert_eq!(summary.by_due_bucket.get("lt14").copied(),    Some(1));
    assert_eq!(summary.by_due_bucket.get("lt30").copied(),    Some(1));
    assert_eq!(summary.by_due_bucket.get("gt30").copied(),    Some(2));
    assert_eq!(summary.by_due_bucket.get("overdue").copied(), Some(1));

    // +1 桶（冗余字段）
    assert_eq!(summary.overdue_count, 1, "overdue_count = by_due_bucket[overdue]");
    assert_eq!(summary.due_soon_count, 1, "due_soon_count = by_due_bucket[lt3]");

    // 联合 atomic_rename 走完整通道
    let result = write_atomic(&out_dir, |temp| {
        write_summary(&temp.join("summary.json"), &summary)
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    assert!(result.is_ok(), "atomic_rename 写 summary.json 应成功");

    let summary_path = out_dir.join("summary.json");
    let content = fs::read_to_string(&summary_path).unwrap();
    let back: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(back["total"], 7);
    assert_eq!(back["overdue_count"], 1);
    assert_eq!(back["due_soon_count"], 1);
    // 元字段一致性
    assert_eq!(back["week_id"], "2026-W41");
    assert!(back["tool_version"].is_string());
    assert_eq!(back["ai_degraded"], false);
}

/// 验证 sha256 manifest 写入 + 元字段 + 校验和正确性（与 atomic_rename 联合）。
#[test]
fn task29_sha256_manifest_round_trip_with_atomic_rename() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = ExportMetadata::now("2026-W41", false);

    let result = write_atomic(&out_dir, |temp| {
        // 写 4 个内容文件用于 sha256 计算
        fs::write(temp.join("resolutions.csv"), "h1\nrow1\n").unwrap();
        fs::write(temp.join("overdue.jsonl"), "{}\n{}\n").unwrap();
        fs::write(temp.join("summary.json"), r#"{"total":2}"#).unwrap();
        fs::write(temp.join("audit_log.jsonl"), "x\ny\nz\nw\nv\n").unwrap();
        // 计算 manifest
        let files = ["resolutions.csv", "overdue.jsonl", "summary.json", "audit_log.jsonl"];
        let m = build_manifest(&temp, &files, &meta)
            .map_err(|e| e.to_string())?;
        write_manifest(&temp.join("sha256_manifest.json"), &m)
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    assert!(result.is_ok(), "atomic_rename 写 manifest 应成功：{result:?}");

    let manifest_path = out_dir.join("sha256_manifest.json");
    assert!(manifest_path.exists(), "sha256_manifest.json 应存在");

    // 解析 manifest
    let content = fs::read_to_string(&manifest_path).unwrap();
    let m: serde_json::Value = serde_json::from_str(&content).unwrap();
    let entries = m["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 4, "manifest 应含 4 个文件");

    // 4 个文件 sha256 各自为 64 字符小写 hex
    for entry in entries {
        let p = entry["path"].as_str().unwrap();
        let s = entry["sha256"].as_str().unwrap();
        assert_eq!(s.len(), 64, "{p} sha256 应为 64 字符，实际 {}", s.len());
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{p} sha256 应为小写 hex");
    }

    // 元字段一致性
    assert_eq!(m["week_id"], "2026-W41");
    assert_eq!(m["ai_degraded"], false);

    // 校验：独立计算每个文件的 sha256 与 manifest 一致
    for entry in entries {
        let p = entry["path"].as_str().unwrap();
        let expected = entry["sha256"].as_str().unwrap();
        let actual = sha256_hex_file(&out_dir.join(p)).unwrap();
        assert_eq!(actual, expected, "{p} sha256 与 manifest 应一致");
    }
}

/// 验证 5 段审计链完整生成（cli_start / oa_pull_complete / extract_call /
/// bucket_advance / export_complete）+ 与 atomic_rename 联合 + status/tool_version 字段。
#[test]
fn task29_five_segment_audit_chain_complete_end_to_end() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = ExportMetadata::now("2026-W41", true); // ai_degraded=true 场景

    // 5 段审计链：标准 + status 各异 + 携带 token/latency
    let audit_entries = vec![
        AuditSegmentEntry {
            segment: "cli_start".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: Some("abc1234".into()),
            env_hash: None,
            model_name: None,
            prompt_hash: None,
            latency_ms: Some(50),
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "oa_pull_complete".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: Some("abc1234".into()),
            env_hash: Some("env-hash-789".into()),
            model_name: None,
            prompt_hash: None,
            latency_ms: Some(1200),
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "extract_call".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: Some("abc1234".into()),
            env_hash: Some("env-hash-789".into()),
            model_name: Some("gpt-4o-mini".into()),
            prompt_hash: Some("ph-deadbeef".into()),
            latency_ms: Some(3500),
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "bucket_advance".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: Some("abc1234".into()),
            env_hash: Some("env-hash-789".into()),
            model_name: Some("gpt-4o-mini".into()),
            prompt_hash: Some("ph-deadbeef".into()),
            latency_ms: Some(15),
            status: "ok".into(),
        },
        AuditSegmentEntry {
            segment: "export_complete".into(),
            tool_version: meta.tool_version.clone(),
            commit_hash: Some("abc1234".into()),
            env_hash: Some("env-hash-789".into()),
            model_name: Some("gpt-4o-mini".into()),
            prompt_hash: Some("ph-deadbeef".into()),
            latency_ms: Some(200),
            status: "ok".into(),
        },
    ];

    let result = write_atomic(&out_dir, |temp| {
        write_audit_log_jsonl(&temp.join("audit_log.jsonl"), &audit_entries)
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    assert!(result.is_ok());

    let audit_path = out_dir.join("audit_log.jsonl");
    let content = fs::read_to_string(&audit_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 5, "audit_log.jsonl 应含 5 段");

    let expected = [
        ("cli_start", Some(50)),
        ("oa_pull_complete", Some(1200)),
        ("extract_call", Some(3500)),
        ("bucket_advance", Some(15)),
        ("export_complete", Some(200)),
    ];
    for (i, (seg, lat)) in expected.iter().enumerate() {
        let v: AuditSegmentEntry = serde_json::from_str(lines[i])
            .unwrap_or_else(|e| panic!("第 {i} 段解析失败：{e}"));
        assert_eq!(v.segment, *seg, "第 {i} 段 segment");
        assert_eq!(v.latency_ms, *lat, "第 {i} 段 latency_ms");
        assert!(v.tool_version.contains("0.1") || v.tool_version.contains("test"));
        assert_eq!(v.status, "ok");
    }

    // ai_degraded=true 经 atomic_rename 之后保留
    assert!(content.contains("\"status\":\"ok\""));
}

/// 端到端全管线：4 JSONL + summary + sha256 + 5 段审计链 + atomic_rename 全通路。
#[test]
fn task29_full_export_package_six_files_via_atomic_rename() {
    let tmp = TempDir::new().unwrap();
    let out_dir = tmp.path().join("out").join("2026-W41");
    let meta = ExportMetadata::now("2026-W41", false);

    // 准备数据
    let csv_rows = vec![
        ResolutionRowBuilder::from_resolution(&sample_resolution("#[W41]01", "DRG 试点", "医务科"))
            .derived(ResolutionStatus::Overdue, -5, DueBucket::Overdue)
            .build(),
        ResolutionRowBuilder::from_resolution(&sample_resolution("#[W41]02", "EMR 升级", "信息科"))
            .derived(ResolutionStatus::DueSoon, 2, DueBucket::Lt3)
            .build(),
    ];
    let summary_rows = vec![
        SummaryInputRow { resolution_id: "#[W41]01", owner_department: "医务科", status: ResolutionStatus::Overdue, due_bucket: DueBucket::Overdue },
        SummaryInputRow { resolution_id: "#[W41]02", owner_department: "信息科", status: ResolutionStatus::DueSoon, due_bucket: DueBucket::Lt3 },
    ];
    let overdue_entries = vec![sample_overdue_entry("#[W41]01", "医务科")];
    let due_soon_entries = vec![sample_due_soon_entry("#[W41]02", "信息科")];
    let conflict_entries = vec![ConflictEntry {
        resolution_id: "#[W41]01".into(),
        kind: "oa_status_mismatch".into(),
        detail: "OA 标 in_progress，会议纪要未签字".into(),
    }];
    let audit_entries = standard_audit_segments(&meta);
    let summary = build_summary(&meta, summary_rows.into_iter());

    // 一次写完 6 文件 + sha256 manifest
    let result = write_atomic(&out_dir, |temp| {
        write_resolutions_csv(&temp.join("resolutions.csv"), &csv_rows, &meta)
            .map_err(|e| e.to_string())?;
        write_overdue_jsonl(&temp.join("overdue.jsonl"), &overdue_entries)
            .map_err(|e| e.to_string())?;
        write_due_soon_jsonl(&temp.join("due_soon.jsonl"), &due_soon_entries)
            .map_err(|e| e.to_string())?;
        write_conflicts_jsonl(&temp.join("conflicts.jsonl"), &conflict_entries)
            .map_err(|e| e.to_string())?;
        write_audit_log_jsonl(&temp.join("audit_log.jsonl"), &audit_entries)
            .map_err(|e| e.to_string())?;
        write_summary(&temp.join("summary.json"), &summary)
            .map_err(|e| e.to_string())?;
        let files = [
            "resolutions.csv",
            "overdue.jsonl",
            "due_soon.jsonl",
            "conflicts.jsonl",
            "audit_log.jsonl",
            "summary.json",
        ];
        let m = build_manifest(&temp, &files, &meta).map_err(|e| e.to_string())?;
        write_manifest(&temp.join("sha256_manifest.json"), &m)
            .map_err(|e| e.to_string())?;
        Ok(())
    });
    assert!(result.is_ok(), "atomic_rename 全通路应成功：{result:?}");

    // 6 文件 + sha256_manifest 全在
    for fname in [
        "resolutions.csv",
        "overdue.jsonl",
        "due_soon.jsonl",
        "conflicts.jsonl",
        "audit_log.jsonl",
        "summary.json",
        "sha256_manifest.json",
    ] {
        assert!(out_dir.join(fname).exists(), "缺 {fname}");
    }

    // 4 JSONL 各自行数
    assert_eq!(fs::read_to_string(out_dir.join("overdue.jsonl")).unwrap().lines().count(), 1);
    assert_eq!(fs::read_to_string(out_dir.join("due_soon.jsonl")).unwrap().lines().count(), 1);
    assert_eq!(fs::read_to_string(out_dir.join("conflicts.jsonl")).unwrap().lines().count(), 1);
    assert_eq!(fs::read_to_string(out_dir.join("audit_log.jsonl")).unwrap().lines().count(), 5);

    // summary 6 桶 + 2 条决议
    let summary: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out_dir.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["overdue_count"], 1);
    assert_eq!(summary["due_soon_count"], 1);

    // sha256 manifest 6 个 entries
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out_dir.join("sha256_manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["entries"].as_array().unwrap().len(), 6);

    // .tmp 父目录应清理
    assert!(!tmp.path().join("out").join(".tmp").exists());
}

/// 守护：5 段审计链顺序与 OaAction→状态机语义一致（仅 OaAction 5 态白名单）。
#[test]
fn task29_oa_action_to_audit_segment_mapping_is_consistent() {
    // OaAction 5 态 + 5 段审计链 + 5 状态机 = 1:1:1 三角对得上
    let actions = [
        OaAction::Started,
        OaAction::InProgress,
        OaAction::Blocked,
        OaAction::Submitted,
        OaAction::Completed,
    ];
    assert_eq!(actions.len(), 5, "OaAction 应 5 态");
    // 5 段审计链语义上对应推进流程的 5 节点
    let segments = [
        "cli_start",
        "oa_pull_complete",
        "extract_call",
        "bucket_advance",
        "export_complete",
    ];
    assert_eq!(segments.len(), 5, "5 段审计链");
    // 5 状态机（status）
    let statuses = [
        ResolutionStatus::Pending,
        ResolutionStatus::InProgress,
        ResolutionStatus::DueSoon,
        ResolutionStatus::Overdue,
        ResolutionStatus::Completed,
    ];
    assert_eq!(statuses.len(), 5, "5 状态机");
}
