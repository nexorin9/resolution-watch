//! CLI 启动 env 校验（8 项必需 env + 1 项可选超时）。

use std::env;

use crate::cli::error::CliError;

/// 8 项必需 env 变量名（启动时校验）。
pub const REQUIRED_ENV: &[&str] = &[
    "OA_API_BEARER",
    "OA_QUERY_DSN",
    "RESO_DB_PATH",
    "RESO_MINUTES_DIR",
    "OUT_DIR",
    "MODEL_BASE_URL",
    "MODEL_API_KEY",
    "MODEL_NAME",
];

/// 校验 8 项必需 env + 1 项可选 `MODEL_TIMEOUT_MS`（默认 15000）。
///
/// 任一必需项缺失立即返回 `CliError::EnvMissing`。
pub fn validate_env() -> Result<ValidatedEnv, CliError> {
    let mut missing = Vec::new();
    for k in REQUIRED_ENV {
        if env::var(k).map(|v| v.trim().is_empty()).unwrap_or(true) {
            missing.push((*k).to_string());
        }
    }
    if !missing.is_empty() {
        return Err(CliError::EnvMissing(missing));
    }
    Ok(ValidatedEnv {
        oa_bearer: env::var("OA_API_BEARER").unwrap(),
        oa_dsn: env::var("OA_QUERY_DSN").unwrap(),
        reso_db_path: env::var("RESO_DB_PATH").unwrap(),
        reso_minutes_dir: env::var("RESO_MINUTES_DIR").unwrap(),
        out_dir: env::var("OUT_DIR").unwrap(),
        model_base_url: env::var("MODEL_BASE_URL").unwrap(),
        model_api_key: env::var("MODEL_API_KEY").unwrap(),
        model_name: env::var("MODEL_NAME").unwrap(),
        model_timeout_ms: env::var("MODEL_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(15_000),
    })
}

/// 已校验的 env 配置（供后续 handler 直接读）。
#[derive(Debug, Clone)]
pub struct ValidatedEnv {
    pub oa_bearer: String,
    pub oa_dsn: String,
    pub reso_db_path: String,
    pub reso_minutes_dir: String,
    pub out_dir: String,
    pub model_base_url: String,
    pub model_api_key: String,
    pub model_name: String,
    pub model_timeout_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_util::ENV_LOCK;

    fn set(k: &str, v: &str) {
        env::set_var(k, v);
    }

    fn unset_all() {
        for k in REQUIRED_ENV.iter().chain(&["MODEL_TIMEOUT_MS"]) {
            env::remove_var(k);
        }
    }

    fn set_all() {
        set("OA_API_BEARER", "bearer");
        set("OA_QUERY_DSN", "https://oa");
        set("RESO_DB_PATH", "./x.sqlite");
        set("RESO_MINUTES_DIR", "./inbox");
        set("OUT_DIR", "./out");
        set("MODEL_BASE_URL", "https://llm");
        set("MODEL_API_KEY", "key");
        set("MODEL_NAME", "gpt");
    }

    #[test]
    fn missing_env_returns_env_missing_error() {
        let _g = ENV_LOCK.lock().unwrap();
        unset_all();
        let err = validate_env().expect_err("应失败");
        match err {
            CliError::EnvMissing(m) => assert!(m.contains(&"OA_API_BEARER".to_string())),
            other => panic!("应为 EnvMissing，实为 {other:?}"),
        }
    }

    #[test]
    fn all_env_set_passes() {
        let _g = ENV_LOCK.lock().unwrap();
        unset_all();
        set_all();
        let v = validate_env().expect("应通过");
        assert_eq!(v.oa_bearer, "bearer");
        assert_eq!(v.model_timeout_ms, 15_000);
    }

    /// 退出码 1 必答：env 缺一项 → 退出码 1。
    #[test]
    fn env_missing_returns_exit_code_1() {
        let _g = ENV_LOCK.lock().unwrap();
        unset_all();
        set("OA_API_BEARER", "bearer");
        set("OA_QUERY_DSN", "https://oa");
        // 其余 6 项故意不设
        let err = validate_env().expect_err("应失败");
        assert_eq!(err.exit_code(), 1, "env 缺失必须映射退出码 1");
    }
}