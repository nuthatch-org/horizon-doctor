//! Configuration.
//!
//! `horizon-doctor` reuses the stack's existing `config.toml` shape where it can:
//! a `[doctor]` table selecting the manifest and network, the Postgres URL, and a
//! `[doctor.endpoints]` table for the various status/metrics/RPC endpoints. Any
//! secret-bearing value may be given as `env:VAR_NAME` and is resolved from the
//! process environment at load time — secrets never live in the file.

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub doctor: DoctorConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DoctorConfig {
    /// Selects the invariant set / version matrix, e.g. `horizon-2.0`.
    pub stack_manifest: String,
    /// The network the indexer is configured for, e.g. `arbitrum-one`.
    #[serde(default)]
    pub network: Option<String>,
    /// Promote WARN to failure.
    #[serde(default)]
    pub strict: bool,
    /// Postgres connection string for read-only schema introspection. May be an
    /// `env:VAR` reference.
    #[serde(default)]
    pub database_url: Option<String>,
    #[serde(default)]
    pub endpoints: Endpoints,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Endpoints {
    #[serde(default)]
    pub graph_node_status: Option<String>,
    #[serde(default)]
    pub agent_metrics: Option<String>,
    #[serde(default)]
    pub network_subgraph: Option<String>,
    #[serde(default)]
    pub rpc_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file '{path}': {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("environment variable '{0}' (referenced as env:{0}) is not set")]
    MissingEnv(String),
}

impl Config {
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(text)?)
    }

    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.display().to_string(),
            source,
        })?;
        Config::from_toml(&text)
    }

    /// Resolve the Postgres URL, following an `env:` reference if present.
    pub fn resolved_database_url(&self) -> Result<Option<String>, ConfigError> {
        match &self.doctor.database_url {
            Some(raw) => resolve_env(raw).map(Some),
            None => Ok(None),
        }
    }
}

/// Resolve a possibly-`env:`-prefixed value. A bare value is returned as-is; an
/// `env:VAR` value is read from the environment (error if unset).
pub fn resolve_env(raw: &str) -> Result<String, ConfigError> {
    match raw.strip_prefix("env:") {
        Some(var) => std::env::var(var).map_err(|_| ConfigError::MissingEnv(var.to_string())),
        None => Ok(raw.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let toml = r#"
            [doctor]
            stack_manifest = "horizon-2.0"
            network        = "arbitrum-one"
            strict         = true
            database_url   = "postgres://localhost/indexer"

            [doctor.endpoints]
            graph_node_status = "http://127.0.0.1:8030"
            agent_metrics     = "http://127.0.0.1:7300"
            rpc_url           = "env:DOCTOR_RPC_URL"
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.doctor.stack_manifest, "horizon-2.0");
        assert_eq!(c.doctor.network.as_deref(), Some("arbitrum-one"));
        assert!(c.doctor.strict);
        assert_eq!(
            c.doctor.endpoints.graph_node_status.as_deref(),
            Some("http://127.0.0.1:8030")
        );
    }

    #[test]
    fn minimal_config_defaults() {
        let toml = r#"
            [doctor]
            stack_manifest = "horizon-2.0"
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert!(!c.doctor.strict);
        assert!(c.doctor.network.is_none());
        assert!(c.doctor.database_url.is_none());
        assert!(c.doctor.endpoints.rpc_url.is_none());
    }

    #[test]
    fn resolve_env_passes_through_bare_value() {
        assert_eq!(resolve_env("postgres://x").unwrap(), "postgres://x");
    }

    #[test]
    fn resolve_env_reads_variable() {
        // Use a uniquely-named var to avoid clashing with the environment.
        std::env::set_var("HORIZON_DOCTOR_TEST_DB", "postgres://from-env");
        assert_eq!(
            resolve_env("env:HORIZON_DOCTOR_TEST_DB").unwrap(),
            "postgres://from-env"
        );
        std::env::remove_var("HORIZON_DOCTOR_TEST_DB");
    }

    #[test]
    fn resolve_env_missing_var_errors() {
        let err = resolve_env("env:HORIZON_DOCTOR_DEFINITELY_UNSET").unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnv(_)));
    }

    #[test]
    fn resolved_database_url_follows_env() {
        std::env::set_var("HORIZON_DOCTOR_TEST_DB2", "postgres://resolved");
        let c = Config::from_toml(
            r#"
            [doctor]
            stack_manifest = "horizon-2.0"
            database_url = "env:HORIZON_DOCTOR_TEST_DB2"
        "#,
        )
        .unwrap();
        assert_eq!(
            c.resolved_database_url().unwrap().as_deref(),
            Some("postgres://resolved")
        );
        std::env::remove_var("HORIZON_DOCTOR_TEST_DB2");
    }
}
