//! CLI 错误类型 + 退出码 0–7 映射。

use thiserror::Error;

use crate::bucket::BucketError;
use crate::export::WriteAtomicError;
use crate::extract::ExtractError;
use crate::oa_pull::OaPullError;
use crate::store::StoreError;

/// CLI 错误枚举。
#[derive(Debug, Error)]
pub enum CliError {
    /// 启动时 env 缺失。
    #[error("env 缺失：{0:?}")]
    EnvMissing(Vec<String>),

    /// OA 拉取失败（4xx / 5xx / schema 错）。
    #[error("OA 拉取失败：{0}")]
    OaPull(#[from] OaPullError),

    /// LLM 抽取失败 + deterministic_fallback 也失败。
    #[error("LLM 不可达且 fallback 也失败：{0}")]
    Extract(#[from] ExtractError),

    /// schema 校验失败（决议条目缺字段 / 状态机非法跃迁）。
    #[error("schema 校验失败：{0}")]
    Schema(String),

    /// 桶派生失败。
    #[error("桶派生失败：{0}")]
    Bucket(#[from] BucketError),

    /// Sqlite 存储失败（迁移 / 主键冲突 / 完整性违例）。
    #[error("存储失败：{0}")]
    Store(#[from] StoreError),

    /// 半成品回滚（导出包写到 .tmp/ 中途失败，需删除临时目录）。
    #[error("半成品回滚：{0}")]
    Rollback(String),

    /// 导出包写盘失败。
    #[error("导出失败：{0}")]
    Export(#[from] WriteAtomicError),

    /// 其它未分类错误。
    #[error("CLI 错误：{0}")]
    Other(String),
}

impl CliError {
    /// 退出码映射：
    /// 0 = 成功；1 = env 缺失；2 = OA 拉取失败；3 = LLM 不可达且 fallback 也失败；
    /// 4 = schema 校验失败 / 存储失败；5 = 桶派生失败；6 = 半成品回滚；7 = 导出失败 / 其它。
    pub fn exit_code(&self) -> u8 {
        match self {
            CliError::EnvMissing(_) => 1,
            CliError::OaPull(_) => 2,
            CliError::Extract(_) => 3,
            CliError::Schema(_) | CliError::Store(_) => 4,
            CliError::Bucket(_) => 5,
            CliError::Rollback(_) => 6,
            CliError::Export(_) | CliError::Other(_) => 7,
        }
    }
}