//! CLI 子命令层（clap 骨架 + env 校验 + 退出码 + tracing dispatch）。
//!
//! 子模块：
//! - `cmd.rs`      — clap 子命令 run/ingest/verify/sign + 参数定义 + WeekId 强类型
//! - `env.rs`      — 启动时 8 项 env 校验
//! - `error.rs`    — CliError + 退出码 0-7 映射
//! - `dispatch.rs` — 按 subcommand 分发到各 handler

pub mod cmd;
pub mod dispatch;
pub mod env;
pub mod error;

#[cfg(test)]
mod tests;

/// 测试期共享的进程级 env 锁（cli 各子模块测试用同一把锁，防止并行测试间 env 互踩）。
#[cfg(test)]
pub mod test_util {
    use std::sync::Mutex;
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());
}

// 重导出，便于 `use cli::*` 调用层级扁平
pub use cmd::{
    IngestArgs, ResoCli, ResoSubcommand, RunArgs, SignArgs, VerifyArgs, WeekId,
    WeekIdParseError,
};
pub use dispatch::dispatch as dispatch_async;
pub use env::{validate_env, ValidatedEnv, REQUIRED_ENV};
pub use error::CliError;

/// 初始化 tracing-subscriber JSON 结构化日志（输出到 stderr，不写日志文件）。
///
/// 入口幂等：`tracing_subscriber::fmt().try_init()` 在已初始化时返回 `Err` 但本函数
/// 用 `.ok()` 吞掉，所以多次调用不 panic。`main.rs` 与 `cli/tests.rs` 都通过
/// 本函数入口初始化 —— 强参考：
/// - 源 `crates/common/src/runtime/mod.rs` 的 `propagate_tracing` 强一致日志可见
/// - 源 `crates/runtime/src/prod.rs` 的 `propagate_tracing` 一致性要求
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let _ = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,resolution_watch=debug"))
        .map(|filter| {
            fmt()
                .with_env_filter(filter)
                .json()
                .with_writer(std::io::stderr)
                .with_target(true)
                .with_current_span(false)
                .try_init()
                .ok();
        });
}

/// 同步 dispatch 入口（供 cli/tests.rs 等同步上下文使用）。
///
/// `main.rs` 用 `#[tokio::main]` 直接 await `dispatch_async`；测试代码在
/// 同步上下文调用 `dispatch()`，内部新建 `current_thread` tokio runtime 后
/// 阻塞运行异步 dispatcher。
pub fn dispatch(cli: ResoCli) -> Result<(), CliError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::Other(format!("tokio runtime 启动失败：{e}")))?;
    rt.block_on(dispatch_async(cli))
}