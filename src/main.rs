//! `reso` CLI 入口。
//!
//! 启动序列：
//! 1. `init_tracing` —— tracing-subscriber JSON 结构化日志 → stderr（不写文件）
//! 2. `ResoCli::from_args` —— clap derive 解析 argv（必填 `--week` 立即拒）
//! 3. `dispatch_async` —— `#[tokio::main]` 异步 runtime 内调用 4 子命令 dispatcher
//! 4. 退出码由 `CliError::exit_code()` 映射（0=成功 / 1=env 缺失 / 2=clap / 3-7 业务错误）
//!
//! 源产品主路径同构：源 `crates/runtime/src/prod.rs` 的 `propagate_tracing` 把
//! tracing context 贯穿整个 runtime —— 本任务以 `tracing-subscriber::fmt().json()`
//! 落到 stderr，与源 `propagate_tracing` 把日志接到 runtime 一致。

use resolution_watch::cli::{dispatch_async, init_tracing, ResoCli};
use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // tracing JSON 结构化日志初始化（输出到 stderr，不写日志文件）
    init_tracing();

    let argv: Vec<String> = std::env::args().collect();
    let cli = match ResoCli::from_args(argv) {
        Ok(c) => c,
        Err(e) => {
            // clap 错误 → exit code 2（参数错误）
            eprint!("{e}");
            return ExitCode::from(2);
        }
    };

    match dispatch_async(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reso error: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
