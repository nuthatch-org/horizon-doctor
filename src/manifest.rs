//! Versioned invariant sets.
//!
//! A [`Manifest`] is the set of invariants `horizon-doctor` checks for a given
//! stack release. The canonical source of truth for the schema invariants is the
//! TypeScript `graphprotocol/indexer` migrations (mirrored by the `sqlx` queries
//! in `indexer-rs`); the manifest captures the (table, columns) mapping those
//! queries depend on so we can detect a missing migration *before* the panic.
//!
//! Manifests are vendored in `manifests/` and selected by name via config.

use serde::Deserialize;

/// The default discriminator column that distinguishes a Horizon (V2) table from
/// its legacy (V1) shape. Its absence is the `42703` failure made famous.
fn default_key_column() -> String {
    "collection_id".to_string()
}

/// A complete invariant set for one stack release.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Selector, e.g. `horizon-2.0`. Matched against `[doctor].stack_manifest`.
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub schema: SchemaManifest,
    #[serde(default)]
    pub version: VersionManifest,
}

/// Version-matrix invariants: the minimum component versions a stack release
/// requires (notably the `v2.0.0` Horizon hard floor for the Rust components).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct VersionManifest {
    #[serde(default, rename = "components")]
    pub components: Vec<ComponentInvariant>,
}

/// A single component's version requirement.
#[derive(Debug, Clone, Deserialize)]
pub struct ComponentInvariant {
    /// Component name, e.g. `indexer-tap-agent`. Also the key the operator uses in
    /// `[doctor.versions]` to declare the deployed version.
    pub name: String,
    /// The minimum acceptable semver. `None` means "report only, no floor".
    #[serde(default)]
    pub min_version: Option<String>,
    /// When `true`, a deployed version below `min_version` is a hard `FAIL` (the
    /// Horizon v2 requirement); otherwise it is a `WARN`.
    #[serde(default)]
    pub hard: bool,
    /// Optional endpoint key (into `[doctor.endpoints]`) whose Prometheus metrics
    /// expose this component's running version, for a live cross-check.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// The Prometheus metric carrying the `version` label, e.g. `tap_agent_build_info`.
    #[serde(default)]
    pub metric: Option<String>,
    /// Optional remediation override; otherwise a sensible default is composed.
    #[serde(default)]
    pub remediation: Option<String>,
}

/// Schema-coherence invariants: the tables and columns the Rust components read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SchemaManifest {
    #[serde(default, rename = "tables")]
    pub tables: Vec<TableInvariant>,
}

/// A single required table and the columns the components' queries reference.
#[derive(Debug, Clone, Deserialize)]
pub struct TableInvariant {
    /// Postgres table name (assumed in the `public` schema).
    pub name: String,
    /// Columns the Rust `sqlx` queries reference. Missing any of these is a hard
    /// fail (`42703` at runtime).
    #[serde(default)]
    pub columns: Vec<String>,
    /// The V2 discriminator column. If the table exists but lacks this column,
    /// it has the legacy shape and (if empty) can be dropped for `indexer-agent`
    /// to recreate.
    #[serde(default = "default_key_column")]
    pub key_column: String,
    /// Optional remediation override; otherwise a sensible default is composed.
    #[serde(default)]
    pub remediation: Option<String>,
}

/// The vendored manifests, embedded at compile time so the binary is fully
/// self-contained (the RFC's "vendored, with CI drift detection" choice).
const HORIZON_2_0: &str = include_str!("../manifests/horizon-2.0.toml");

impl Manifest {
    /// Parse a manifest from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Load a vendored manifest by its selector name.
    pub fn load(name: &str) -> Result<Self, ManifestError> {
        let text = match name {
            "horizon-2.0" => HORIZON_2_0,
            other => return Err(ManifestError::Unknown(other.to_string())),
        };
        Manifest::from_toml(text).map_err(ManifestError::Parse)
    }

    /// The selector names of all vendored manifests.
    pub fn vendored_names() -> &'static [&'static str] {
        &["horizon-2.0"]
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("unknown manifest '{name}'; vendored manifests: {vendored}", name = .0, vendored = Manifest::vendored_names().join(", "))]
    Unknown(String),
    #[error("failed to parse manifest: {0}")]
    Parse(#[from] toml::de::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_manifest() {
        let toml = r#"
            name = "test-1.0"
            description = "a test manifest"

            [[schema.tables]]
            name = "tap_horizon_receipts"
            columns = ["collection_id", "value"]
        "#;
        let m = Manifest::from_toml(toml).unwrap();
        assert_eq!(m.name, "test-1.0");
        assert_eq!(m.schema.tables.len(), 1);
        let t = &m.schema.tables[0];
        assert_eq!(t.name, "tap_horizon_receipts");
        assert_eq!(t.columns, vec!["collection_id", "value"]);
        // key_column defaults
        assert_eq!(t.key_column, "collection_id");
        assert!(t.remediation.is_none());
    }

    #[test]
    fn vendored_horizon_manifest_loads_and_covers_v2_tables() {
        let m = Manifest::load("horizon-2.0").unwrap();
        assert_eq!(m.name, "horizon-2.0");

        let names: Vec<&str> = m.schema.tables.iter().map(|t| t.name.as_str()).collect();
        // The V2/Horizon tables whose absence triggers the startup panic.
        for required in [
            "tap_horizon_receipts",
            "tap_horizon_ravs",
            "tap_horizon_denylist",
        ] {
            assert!(
                names.contains(&required),
                "manifest must require {required}"
            );
        }

        // The receipts table must require the famous discriminator column.
        let receipts = m
            .schema
            .tables
            .iter()
            .find(|t| t.name == "tap_horizon_receipts")
            .unwrap();
        assert!(receipts.columns.iter().any(|c| c == "collection_id"));
    }

    #[test]
    fn unknown_manifest_errors() {
        let err = Manifest::load("does-not-exist").unwrap_err();
        assert!(matches!(err, ManifestError::Unknown(_)));
    }
}
