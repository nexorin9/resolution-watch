//! Sqlite 决议推进表 + 周次幂等键层错误类型。
//!
//! 与源 `crates/db_connection/src/lib.rs` 的「确定性失败可见」思路同构：
//! 任何数据库错误均不静默吞掉，由 `migrate` / 后续 `repo` 显式返回。

use thiserror::Error;

/// Sqlite 决议推进表 + 周次幂等键层错误。
///
/// 退出码映射（见 `cli/error.rs`）：
/// - `Open` / `Migrate` / `Config` → 退出码 5（桶派生/存储失败）
/// - 后续 `repo::upsert_idempotent` 冲突 → 退出码 4（schema 校验失败）
#[derive(Debug, Error)]
pub enum StoreError {
    /// 数据库打开失败（路径不存在 / 权限拒绝 / 磁盘满）。
    #[error("Sqlite 打开失败：{0}")]
    Open(String),

    /// DDL 迁移失败（CREATE TABLE / CREATE INDEX 错误）。
    #[error("Sqlite 迁移失败：{0}")]
    Migrate(String),

    /// 配置缺失（`RESO_DB_PATH` 解析失败，路径非法）。
    #[error("Sqlite 配置错误：{0}")]
    Config(String),
}

impl StoreError {
    /// 是否需要重试：本层错误均不重试（数据库层故障应由上层报警而非反复打开）。
    pub fn is_retryable(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_retryable_always_false() {
        // 任何数据库层错误都不在 store 层重试；上层报警 + 退出码 5 显式化
        assert!(!StoreError::Open("perm denied".into()).is_retryable());
        assert!(!StoreError::Migrate("syntax error".into()).is_retryable());
        assert!(!StoreError::Config("invalid path".into()).is_retryable());
    }

    #[test]
    fn error_display_contains_key_field() {
        // Display 必须包含关键字段，便于日志与最终用户定位
        assert!(StoreError::Open("perm denied".into())
            .to_string()
            .contains("perm denied"));
        assert!(StoreError::Migrate("syntax error".into())
            .to_string()
            .contains("syntax error"));
        assert!(StoreError::Config("invalid path".into())
            .to_string()
            .contains("invalid path"));
    }

    #[test]
    fn error_wraps_underlying_cause() {
        // 构造器接受任意 String，便于上层从 rusqlite::Error 包装
        let e = StoreError::Open("path=/nonexistent".into());
        let s = e.to_string();
        assert!(s.contains("Sqlite 打开失败"), "display 前缀：{s}");
    }
}
