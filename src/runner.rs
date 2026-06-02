//! Orchestration: turn a config + manifest + selected families into a report.
//!
//! Each family is run for effect and contributes [`Check`]s. Connection and
//! configuration problems are themselves reported as `FAIL` checks rather than
//! panics, so an `initContainer` always gets an explained, gate-able result.

use crate::check::{Check, CheckReport, Family};
use crate::config::Config;
use crate::manifest::Manifest;
use crate::schema::{self, PgSchemaSource, SchemaSource};
use crate::version::{self, HttpVersionSource, Observed, Probe, VersionSnapshot, VersionSource};

/// Run the selected check families.
///
/// `explicit` indicates the user passed `--only`; when true, a requested family
/// that isn't implemented yet is surfaced as a WARN, rather than silently skipped.
pub async fn run(
    config: &Config,
    manifest: &Manifest,
    families: &[Family],
    explicit: bool,
    database_url_override: Option<&str>,
) -> CheckReport {
    let mut report = CheckReport::new();

    for &family in families {
        match family {
            Family::Schema => {
                report.extend(run_schema(config, manifest, database_url_override).await);
            }
            Family::Version => {
                report.extend(run_version(config, manifest).await);
            }
            // Not yet implemented in this slice.
            Family::Horizon | Family::Provision | Family::Startup => {
                if explicit {
                    report.push(Check::warn(
                        family,
                        format!("{}.pending", family.as_str()),
                        format!(
                            "the '{}' check family is not implemented in this build",
                            family.as_str()
                        ),
                        "Track progress in TOOL-RFC-002; this slice ships the schema family.",
                    ));
                }
            }
        }
    }

    report
}

/// Resolve the database URL, preferring the CLI override, then config.
fn resolve_db_url(
    config: &Config,
    database_url_override: Option<&str>,
) -> Result<Option<String>, crate::config::ConfigError> {
    if let Some(raw) = database_url_override {
        return crate::config::resolve_env(raw).map(Some);
    }
    config.resolved_database_url()
}

async fn run_schema(
    config: &Config,
    manifest: &Manifest,
    database_url_override: Option<&str>,
) -> Vec<Check> {
    let db_url = match resolve_db_url(config, database_url_override) {
        Ok(Some(url)) => url,
        Ok(None) => {
            return vec![Check::fail(
                Family::Schema,
                "schema.config",
                "no Postgres connection string configured",
                "Set [doctor].database_url in config (or pass --database-url); it may be an env: reference.",
            )];
        }
        Err(e) => {
            return vec![Check::fail(
                Family::Schema,
                "schema.config",
                format!("could not resolve database URL: {e}"),
                "Ensure the referenced environment variable is set.",
            )];
        }
    };

    let source = match PgSchemaSource::connect(&db_url).await {
        Ok(s) => s,
        Err(e) => {
            return vec![Check::fail(
                Family::Schema,
                "schema.connection",
                format!("could not connect to Postgres: {e}"),
                "Verify the database is reachable and the connection string is correct. \
                 horizon-doctor only ever reads.",
            )];
        }
    };

    let tables = schema::required_tables(manifest);
    match source.snapshot(&tables).await {
        Ok(snapshot) => schema::evaluate_schema(manifest, &snapshot),
        Err(e) => vec![Check::fail(
            Family::Schema,
            "schema.introspection",
            format!("failed to introspect schema: {e}"),
            "Check that the connecting role may read information_schema.",
        )],
    }
}

/// Run the version family: assemble what we can observe (operator-declared
/// versions, plus a live scrape where a metrics endpoint is configured) and hand
/// it to the pure evaluator.
async fn run_version(config: &Config, manifest: &Manifest) -> Vec<Check> {
    if manifest.version.components.is_empty() {
        return Vec::new();
    }

    // Declared matrix from [doctor.versions], with env: references resolved.
    let declared = match config.resolved_versions() {
        Ok(d) => d,
        Err(e) => {
            return vec![Check::fail(
                Family::Version,
                "version.config",
                format!("could not resolve declared versions: {e}"),
                "Ensure the referenced environment variable is set, or use a literal version.",
            )];
        }
    };

    // Build live probes for components that name a metrics endpoint we can resolve.
    let mut probes = Vec::new();
    for c in &manifest.version.components {
        let (Some(ep_key), Some(metric)) = (&c.endpoint, &c.metric) else {
            continue;
        };
        let Some(raw) = config.doctor.endpoints.get(ep_key) else {
            continue; // endpoint not configured; declared value (if any) stands alone.
        };
        // A bad env: reference here shouldn't sink the whole family — just skip the
        // live cross-check for this component.
        if let Ok(url) = crate::config::resolve_env(raw) {
            probes.push(Probe {
                component: c.name.clone(),
                url,
                metric: metric.clone(),
            });
        }
    }

    let mut scraped = if probes.is_empty() {
        Default::default()
    } else {
        HttpVersionSource::new().scrape(&probes).await
    };

    // Assemble the snapshot, one Observed per manifest component.
    let mut snapshot = VersionSnapshot::default();
    for c in &manifest.version.components {
        snapshot.components.insert(
            c.name.clone(),
            Observed {
                declared: declared.get(&c.name).cloned(),
                scraped: scraped.remove(&c.name),
            },
        );
    }

    version::evaluate_version(manifest, &snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Status;

    fn config_without_db() -> Config {
        Config::from_toml(
            r#"
            [doctor]
            stack_manifest = "horizon-2.0"
        "#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn schema_without_db_url_fails_with_config_check() {
        let config = config_without_db();
        let manifest = Manifest::load("horizon-2.0").unwrap();
        let report = run(&config, &manifest, &[Family::Schema], true, None).await;
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "schema.config");
        assert_eq!(report.checks[0].status, Status::Fail);
    }

    #[tokio::test]
    async fn unimplemented_family_warns_only_when_explicit() {
        let config = config_without_db();
        let manifest = Manifest::load("horizon-2.0").unwrap();

        // Explicit request -> a pending WARN.
        let report = run(&config, &manifest, &[Family::Horizon], true, None).await;
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].status, Status::Warn);
        assert_eq!(report.checks[0].name, "horizon.pending");

        // Default (non-explicit) -> silently skipped.
        let report = run(&config, &manifest, &[Family::Horizon], false, None).await;
        assert!(report.is_empty());
    }
}
