//! CLI 命令骨架（clap v4 derive）。
//!
//! 定义 `ResoCli` 顶层结构 + 4 个子命令：
//! - `run`     — 周频批处理主命令（extract + bucket + store + export 全链路）
//! - `ingest`  — 仅拉取 OA + LLM 抽取（不写决议推进 / 不导出包）
//! - `verify`  — 校验已导出的决议包 sha256 + 5 段审计链完整性
//! - `sign`    — 在导出的决议包上落签字（记录签字人到 sign_manifest.json）
//!
//! 本任务仅覆盖 clap 子命令定义与 parse —— env 校验、退出码、tracing、实际 handler
//! 在 task 11/12/后续 task 续接。
//!
//! 命名说明：clap 把 `Subcommand` 暴露为 derive trait，故本模块将自身枚举命名为
//! `ResoSubcommand` 避免冲突（统一 `use clap::Subcommand as _` 仅取 derive 宏）。
//!
//! 源产品主路径同构：源 `convex-backend/npm-packages/convex/src/cli/codegen_templates/server.ts`
//! 的 `PLATFORM_ENV_VARS` + `withPlatformEnvVars` env 注入校验（必填 env 变量在工具入口
//! 一次性列出并校验，运行时不再分段取值）—— 本任务以 `--week` 必填 + 4 子命令骨架
//! 做相同的「入口处显式声明、不允许运行时静默为 None」思路。

use clap::{Args, Parser};
use std::fmt;
use std::str::FromStr;

/// 顶层 CLI 结构 —— `reso <subcommand>`。
#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[command(name = "reso", version, about, long_about = None)]
pub struct ResoCli {
    /// ISO 周次（格式 `YYYY-Www`，例如 `2026-W41`）。
    ///
    /// 必填。`--week` 写在子命令之前（如 `reso --week 2026-W41 run`），clap 在 parse
    /// 阶段校验缺失立即报错（exit code 2）。**必须**出现在子命令前 —— 强制把全局上下文放
    /// 在子命令之前，避免子命令名被多个 global arg 干扰。
    #[arg(long, value_name = "WEEK_ID")]
    pub week: String,

    /// 输出目录（默认 `./out`，按 `--week` 子目录落包）。
    ///
    /// 全局可选：未提供时使用 `OUT_DIR` 环境变量，再回落默认。可写在子命令之前或之后。
    #[arg(long, value_name = "DIR", global = true, env = "OUT_DIR", default_value = "./out")]
    pub out_dir: String,

    /// 周次纪要目录（默认 `./inbox`，按 `--week` 子目录读 `minutes.md`）。
    ///
    /// 全局可选：未提供时使用 `RESO_MINUTES_DIR` 环境变量，再回落默认。可写在子命令之前或之后。
    #[arg(
        long,
        value_name = "DIR",
        global = true,
        env = "RESO_MINUTES_DIR",
        default_value = "./inbox"
    )]
    pub inbox_dir: String,

    /// 子命令。
    #[command(subcommand)]
    pub command: ResoSubcommand,
}

/// 4 个子命令枚举。
///
/// 每个子命令独立 `Args` 结构 —— 便于接入 `clap` 的子命令级参数（含中文 help 文本）。
#[derive(clap::Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum ResoSubcommand {
    /// 周频批处理 —— 拉取 OA、抽取决议、推进状态、落库、导出 6 文件决议包。
    Run(RunArgs),

    /// 仅 ingest —— 拉取 OA + LLM 抽取（不进入状态机 / 不写库）。
    Ingest(IngestArgs),

    /// 校验导出包 —— sha256 + 5 段审计链完整性比对。
    Verify(VerifyArgs),

    /// 在导出包上落签字 —— 记录签字人到 `sign_manifest.json`。
    Sign(SignArgs),
}

/// `run` 子命令参数。
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// 强制 deterministic 解析（跳过 LLM，按 `## 决议 N:` 段落正则）。
    #[arg(long)]
    pub deterministic: bool,

    /// 干跑 —— 不写库不导出，仅打印 main path 摘要。
    #[arg(long)]
    pub dry_run: bool,
}

/// `ingest` 子命令参数。
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct IngestArgs {
    /// 强制 deterministic 解析（跳过 LLM）。
    #[arg(long)]
    pub deterministic: bool,

    /// LLM 替身 URL（覆盖 `MODEL_BASE_URL`，便于测试 / 干跑）。
    #[arg(long, value_name = "URL")]
    pub model_base_url: Option<String>,

    /// LLM 模型名（覆盖 `MODEL_NAME`）。
    #[arg(long, value_name = "NAME")]
    pub model_name: Option<String>,
}

/// `verify` 子命令参数。
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct VerifyArgs {
    /// 仅显示 sha256 不比较（不报错）。
    #[arg(long)]
    pub print_only: bool,
}

/// `sign` 子命令参数。
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct SignArgs {
    /// 签字人工号（必填）。
    #[arg(long = "signer-id", value_name = "ID")]
    pub signer_id: String,

    /// 签字人姓名（必填）。
    #[arg(long = "sign-name", value_name = "NAME")]
    pub sign_name: String,

    /// 签字备注（可选，记录到 `sign_manifest.json`）。
    #[arg(long, value_name = "NOTE")]
    pub note: Option<String>,
}

impl ResoCli {
    /// 从环境变量 + 命令行参数解析 —— 失败返回 clap 错误（带 exit code 2）。
    ///
    /// `argv` 通常是 `std::env::args().collect::<Vec<_>>()`；测试时可注入自定义数组。
    pub fn from_args(argv: Vec<String>) -> Result<Self, clap::Error> {
        Self::try_parse_from(argv)
    }

    /// 周次（统一从顶层读，所有子命令共用）。
    pub fn week_id(&self) -> &str {
        &self.week
    }

    /// 周次（包装 `WeekId` 强类型），供后续 store / export 模块使用 —— 后续 task 实现。
    pub fn parsed_week_id(&self) -> Result<WeekId, WeekIdParseError> {
        WeekId::parse(&self.week)
    }
}

/// 周次 ID 的强类型包装（`YYYY-Www` 格式）。
///
/// 保留作后续 task（store / export）使用的入口强类型 —— `clap` 不直接支持自定义标量类型解析，
/// 但可以 `from_str` 后手工校验（`WeekId::parse(week_id: &str) -> Result<WeekId, _>`）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WeekId(String);

impl WeekId {
    /// 解析 `YYYY-Www` 格式（如 `2026-W41` / `2025-W53`）。
    ///
    /// 接受两种形式：canonical 带连字符 `2026-W41`、紧凑无连字符 `2026W41`，以及
    /// 1-2 位周次（`2026-W5`）。**严格区分大小写**：仅大写 `W` 接受，连接处可省略连字符。
    pub fn parse(input: &str) -> Result<Self, WeekIdParseError> {
        let raw = input.trim();
        if raw.is_empty() {
            return Err(WeekIdParseError::Empty);
        }

        // 必须含严格大写 'W'
        let (year_with_dash, week_raw) = raw
            .split_once('W')
            .ok_or(WeekIdParseError::Malformed)?;
        // canonical 形式 `2026-W41` 在 split_after 'W' 之前有连字符
        let year_raw = year_with_dash.trim_end_matches('-');

        // 年份必须严格 4 位数字
        if year_raw.len() != 4 || !year_raw.chars().all(|c| c.is_ascii_digit()) {
            return Err(WeekIdParseError::Malformed);
        }
        // 周次必须 1-2 位数字
        if week_raw.is_empty()
            || week_raw.len() > 2
            || !week_raw.chars().all(|c| c.is_ascii_digit())
        {
            return Err(WeekIdParseError::Malformed);
        }
        let week_num: u32 = week_raw
            .parse()
            .map_err(|_| WeekIdParseError::Malformed)?;
        if !(1..=53).contains(&week_num) {
            return Err(WeekIdParseError::OutOfRange {
                week: week_num,
            });
        }
        Ok(Self(raw.to_string()))
    }

    /// ISO 周次字符串（如 `2026-W41`）。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WeekId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for WeekId {
    type Err = WeekIdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// 周次解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeekIdParseError {
    /// 空字符串。
    Empty,
    /// 格式不匹配（缺 `W` / 年份非 4 位 / 周次非数字）。
    Malformed,
    /// 周次超界（不在 1-53）。
    OutOfRange { week: u32 },
}

impl fmt::Display for WeekIdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("week id is empty"),
            Self::Malformed => f.write_str("week id must match YYYY-Www (e.g. 2026-W41)"),
            Self::OutOfRange { week } => write!(
                f,
                "week id week number {} is out of ISO range 1..=53",
                week
            ),
        }
    }
}

impl std::error::Error for WeekIdParseError {}

#[cfg(test)]
mod tests_cmd {
    use super::*;

    /// 4 子命令都能正确 parse —— 必答。
    #[test]
    fn four_subcommands_each_parse_to_correct_variant() {
        let run = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "run".into(),
        ])
        .expect("run parses");
        assert!(matches!(run.command, ResoSubcommand::Run(_)));
        assert_eq!(run.week_id(), "2026-W41");

        let ingest = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "ingest".into(),
            "--deterministic".into(),
        ])
        .expect("ingest parses");
        assert!(matches!(ingest.command, ResoSubcommand::Ingest(_)));
        let ingest_args = match ingest.command {
            ResoSubcommand::Ingest(args) => args,
            _ => unreachable!(),
        };
        assert!(ingest_args.deterministic);

        let verify = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "verify".into(),
            "--print-only".into(),
        ])
        .expect("verify parses");
        assert!(matches!(verify.command, ResoSubcommand::Verify(_)));
        let verify_args = match verify.command {
            ResoSubcommand::Verify(args) => args,
            _ => unreachable!(),
        };
        assert!(verify_args.print_only);

        let sign = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "sign".into(),
            "--signer-id".into(),
            "U001".into(),
            "--sign-name".into(),
            "张三".into(),
            "--note".into(),
            "科务会复核通过".into(),
        ])
        .expect("sign parses");
        assert!(matches!(sign.command, ResoSubcommand::Sign(_)));
        let sign_args = match sign.command {
            ResoSubcommand::Sign(args) => args,
            _ => unreachable!(),
        };
        assert_eq!(sign_args.signer_id, "U001");
        assert_eq!(sign_args.sign_name, "张三");
        assert_eq!(sign_args.note.as_deref(), Some("科务会复核通过"));
    }

    /// `--week` 必填校验 —— 缺失立即报错（必答）。
    #[test]
    fn missing_required_week_returns_parse_error() {
        let err = ResoCli::from_args(vec!["reso".into(), "run".into()])
            .expect_err("--week missing should error");
        assert!(err.kind() == clap::error::ErrorKind::MissingRequiredArgument);
    }

    /// `--week` 在子命令前 —— 即使是 sign 子命令也必须出现。
    #[test]
    fn sign_subcommand_requires_signer_id() {
        let err = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "sign".into(),
        ])
        .expect_err("sign without --signer-id should error");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    }

    /// 全局 `--out-dir` 与 `--inbox-dir` 可选 + 默认值生效。
    #[test]
    fn global_out_dir_default_value_applies_when_omitted() {
        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "run".into(),
        ])
        .expect("default out_dir applies");
        assert_eq!(cli.out_dir, "./out");
        assert_eq!(cli.inbox_dir, "./inbox");
    }

    /// 全局 `--out-dir` 显式覆盖默认值（可在子命令后）。
    #[test]
    fn global_out_dir_explicit_override_takes_precedence() {
        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "--out-dir".into(),
            "/var/reso/out".into(),
            "run".into(),
        ])
        .expect("explicit out_dir parses");
        assert_eq!(cli.out_dir, "/var/reso/out");
    }

    /// 未知子命令立即报错（不静默落到 run）。
    #[test]
    fn unknown_subcommand_returns_parse_error() {
        let err = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "explode".into(),
        ])
        .expect_err("unknown subcommand should error");
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    /// `parsed_week_id()` —— 强类型解析 happy path。
    #[test]
    fn parsed_week_id_accepts_iso_format() {
        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W41".into(),
            "run".into(),
        ])
        .expect("parses");
        let week_id = cli.parsed_week_id().expect("valid iso week id");
        assert_eq!(week_id.as_str(), "2026-W41");
        assert_eq!(format!("{}", week_id), "2026-W41");
    }

    /// `parsed_week_id()` —— 跨年/边界：`2025-W53` / `2026-W01` / `2027-W01`。
    #[test]
    fn parsed_week_id_handles_year_boundary_iso_weeks() {
        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2025-W53".into(),
            "run".into(),
        ])
        .expect("parses");
        assert_eq!(cli.parsed_week_id().expect("valid iso").as_str(), "2025-W53");

        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W01".into(),
            "run".into(),
        ])
        .expect("parses");
        assert_eq!(cli.parsed_week_id().expect("valid iso").as_str(), "2026-W01");

        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2027-W01".into(),
            "run".into(),
        ])
        .expect("parses");
        assert_eq!(cli.parsed_week_id().expect("valid iso").as_str(), "2027-W01");
    }

    /// `parsed_week_id()` —— 周次超界 (1..=53) 立即报错。
    #[test]
    fn parsed_week_id_rejects_out_of_range_week_number() {
        let cli = ResoCli::from_args(vec![
            "reso".into(),
            "--week".into(),
            "2026-W54".into(),
            "run".into(),
        ])
        .expect("parses");
        let err = cli.parsed_week_id().expect_err("W54 out of range");
        assert_eq!(err, WeekIdParseError::OutOfRange { week: 54 });
    }

    /// `parsed_week_id()` —— 格式不匹配立即报错（含空字符串 / 缺 W / 年份非 4 位）。
    #[test]
    fn parsed_week_id_rejects_malformed_input() {
        let bad_inputs = ["", "   "];
        for bad in bad_inputs {
            let cli = ResoCli::from_args(vec![
                "reso".into(),
                "--week".into(),
                bad.into(),
                "run".into(),
            ])
            .expect("clap does not validate ISO format; it accepts any string here");
            let err = cli
                .parsed_week_id()
                .expect_err(format!("input {:?} rejected", bad).as_str());
            assert!(
                matches!(err, WeekIdParseError::Empty),
                "input {:?} should reject as Empty: got {:?}",
                bad,
                err
            );
        }
        let bad_inputs = ["202641", "2026-41", "abc-W41", "26-W41", "2026-WXX", "20A6-W41"];
        for bad in bad_inputs {
            let cli = ResoCli::from_args(vec![
                "reso".into(),
                "--week".into(),
                bad.into(),
                "run".into(),
            ])
            .expect("clap does not validate ISO format; it accepts any string here");
            let err = cli
                .parsed_week_id()
                .expect_err(format!("input {:?} rejected", bad).as_str());
            assert!(
                matches!(err, WeekIdParseError::Malformed),
                "input {:?} should reject: got {:?}",
                bad,
                err
            );
        }
    }

    /// `reso --help` 与子命令 `--help` 不 panic（仅校验不报错）。
    #[test]
    fn help_outputs_do_not_panic() {
        for argv in [
            vec!["reso".to_string(), "--help".to_string()],
            vec![
                "reso".to_string(),
                "--week".to_string(),
                "2026-W41".to_string(),
                "run".to_string(),
                "--help".to_string(),
            ],
            vec![
                "reso".to_string(),
                "--week".to_string(),
                "2026-W41".to_string(),
                "sign".to_string(),
                "--help".to_string(),
            ],
        ] {
            let _ = ResoCli::from_args(argv);
        }
    }
}
