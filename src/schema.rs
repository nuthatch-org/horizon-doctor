//! Check family #1: schema coherence.
//!
//! The architecture deliberately splits two concerns:
//!
//! * [`SchemaSnapshot`] — a plain description of what the database actually
//!   contains (tables, their columns, row counts). Cheap to build by hand.
//! * [`evaluate_schema`] — a *pure* function from `(Manifest, SchemaSnapshot)`
//!   to a list of [`Check`]s. No IO, exhaustively unit-testable.
//!
//! IO lives behind [`SchemaSource`]; the production [`PgSchemaSource`] reads
//! `information_schema` (strictly read-only). Tests construct snapshots directly,
//! and an opt-in integration test (gated on `DATABASE_URL`) exercises the real
//! Postgres path.

use std::collections::{BTreeMap, BTreeSet};

use crate::check::{Check, Family};
use crate::manifest::{Manifest, TableInvariant};

/// What one table looks like in the live database.
#[derive(Debug, Clone)]
pub struct TableSnapshot {
    pub columns: BTreeSet<String>,
    pub row_count: i64,
}

impl TableSnapshot {
    /// Convenience constructor for tests and fixtures.
    pub fn new<I, S>(columns: I, row_count: i64) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        TableSnapshot {
            columns: columns.into_iter().map(Into::into).collect(),
            row_count,
        }
    }
}

/// A read-only picture of the relevant slice of the database schema.
#[derive(Debug, Clone, Default)]
pub struct SchemaSnapshot {
    /// Present tables, keyed by name. Absent tables are simply not in the map.
    pub tables: BTreeMap<String, TableSnapshot>,
}

impl SchemaSnapshot {
    pub fn with_table(mut self, name: impl Into<String>, table: TableSnapshot) -> Self {
        self.tables.insert(name.into(), table);
        self
    }
}

/// A source of schema snapshots. Implemented by [`PgSchemaSource`] for real
/// Postgres; tests build [`SchemaSnapshot`]s directly.
pub trait SchemaSource {
    /// Introspect the given tables. Tables that do not exist are omitted from the
    /// returned snapshot.
    fn snapshot(
        &self,
        tables: &[String],
    ) -> impl std::future::Future<Output = anyhow::Result<SchemaSnapshot>> + Send;
}

/// Default remediation when a required table is entirely absent.
fn missing_table_remediation(table: &str) -> String {
    format!(
        "Table `{table}` does not exist: indexer-agent migrations have not applied this schema. \
         Restart indexer-agent to run migrations, then start indexer-tap-agent."
    )
}

/// Default remediation for the known empty-legacy-table case.
fn empty_legacy_remediation(table: &str, key_column: &str) -> String {
    format!(
        "Table `{table}` exists with the legacy shape (missing `{key_column}`) and is empty. \
         Dropping it lets indexer-agent recreate it with the V2 columns: \
         `DROP TABLE {table};` then restart indexer-agent."
    )
}

/// Remediation for a legacy-shaped table that still holds data.
fn populated_legacy_remediation(table: &str, key_column: &str) -> String {
    format!(
        "Table `{table}` has the legacy shape (missing `{key_column}`) but contains data; \
         do NOT drop it. Ensure indexer-agent has applied the Horizon migration for this \
         release, then re-check. Consult the TAP/GraphTally migration guide before proceeding."
    )
}

/// Evaluate one table invariant against the snapshot.
fn evaluate_table(invariant: &TableInvariant, snapshot: &SchemaSnapshot) -> Check {
    let name = format!("schema.{}", invariant.name);

    let Some(table) = snapshot.tables.get(&invariant.name) else {
        let remediation = invariant
            .remediation
            .clone()
            .unwrap_or_else(|| missing_table_remediation(&invariant.name));
        return Check::fail(
            Family::Schema,
            name,
            format!("required table `{}` is missing", invariant.name),
            remediation,
        );
    };

    let missing: Vec<&String> = invariant
        .columns
        .iter()
        .filter(|c| !table.columns.contains(*c))
        .collect();

    if missing.is_empty() {
        return Check::pass(
            Family::Schema,
            name,
            format!(
                "table `{}` present with all {} required column(s)",
                invariant.name,
                invariant.columns.len()
            ),
        );
    }

    // The table exists but is incomplete. If it lacks the V2 discriminator
    // column, it has the legacy shape — and emptiness decides whether a drop is
    // the safe remedy.
    let lacks_key = missing.contains(&&invariant.key_column);
    if lacks_key {
        if table.row_count == 0 {
            return Check::fail(
                Family::Schema,
                name,
                format!(
                    "table `{}` has the legacy shape (missing `{}`) and is empty",
                    invariant.name, invariant.key_column
                ),
                empty_legacy_remediation(&invariant.name, &invariant.key_column),
            );
        }
        return Check::fail(
            Family::Schema,
            name,
            format!(
                "table `{}` has the legacy shape (missing `{}`) and contains {} row(s)",
                invariant.name, invariant.key_column, table.row_count
            ),
            populated_legacy_remediation(&invariant.name, &invariant.key_column),
        );
    }

    // Present, has the key column, but missing other expected columns.
    let cols: Vec<&str> = missing.iter().map(|c| c.as_str()).collect();
    let remediation = invariant
        .remediation
        .clone()
        .unwrap_or_else(|| missing_table_remediation(&invariant.name));
    Check::fail(
        Family::Schema,
        name,
        format!(
            "table `{}` is missing column(s): {}",
            invariant.name,
            cols.join(", ")
        ),
        remediation,
    )
}

/// Evaluate the schema family for a manifest against a snapshot. Pure.
pub fn evaluate_schema(manifest: &Manifest, snapshot: &SchemaSnapshot) -> Vec<Check> {
    manifest
        .schema
        .tables
        .iter()
        .map(|inv| evaluate_table(inv, snapshot))
        .collect()
}

/// The set of table names a manifest's schema family needs introspected.
pub fn required_tables(manifest: &Manifest) -> Vec<String> {
    manifest
        .schema
        .tables
        .iter()
        .map(|t| t.name.clone())
        .collect()
}

// ----------------------------------------------------------------------------
// Real Postgres source (read-only).
// ----------------------------------------------------------------------------

/// Reads schema state from a live Postgres via `information_schema`. Strictly
/// read-only: it never writes, and only ever issues `SELECT`s.
pub struct PgSchemaSource {
    pool: sqlx::PgPool,
}

impl PgSchemaSource {
    pub fn new(pool: sqlx::PgPool) -> Self {
        PgSchemaSource { pool }
    }

    /// Connect to Postgres read-only. The caller supplies the connection string
    /// (typically the stack's existing `DATABASE_URL` / config value).
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(database_url)
            .await?;
        Ok(PgSchemaSource::new(pool))
    }
}

/// Guard against identifier injection: we interpolate table names into a
/// `count(*)` query (identifiers can't be bound as parameters), so only allow the
/// conservative `[A-Za-z_][A-Za-z0-9_]*` shape.
fn is_safe_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl SchemaSource for PgSchemaSource {
    async fn snapshot(&self, tables: &[String]) -> anyhow::Result<SchemaSnapshot> {
        // 1. Columns for all requested tables, in one query.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name, column_name \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = ANY($1)",
        )
        .bind(tables)
        .fetch_all(&self.pool)
        .await?;

        let mut columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (table, column) in rows {
            columns.entry(table).or_default().insert(column);
        }

        // 2. Row count for each present table (decides the empty-legacy case).
        let mut snapshot = SchemaSnapshot::default();
        for (table, cols) in columns {
            if !is_safe_identifier(&table) {
                anyhow::bail!("refusing to introspect unsafe table identifier: {table:?}");
            }
            let count: (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM \"{table}\""))
                .fetch_one(&self.pool)
                .await?;
            snapshot
                .tables
                .insert(table, TableSnapshot::new(cols, count.0));
        }

        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::load("horizon-2.0").unwrap()
    }

    /// A snapshot where every required table is fully present, post-migration.
    fn healthy_snapshot(m: &Manifest) -> SchemaSnapshot {
        let mut snap = SchemaSnapshot::default();
        for t in &m.schema.tables {
            snap.tables
                .insert(t.name.clone(), TableSnapshot::new(t.columns.clone(), 0));
        }
        snap
    }

    fn find<'a>(checks: &'a [Check], table: &str) -> &'a Check {
        let name = format!("schema.{table}");
        checks.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn post_migration_all_pass() {
        let m = manifest();
        let snap = healthy_snapshot(&m);
        let checks = evaluate_schema(&m, &snap);
        assert_eq!(checks.len(), m.schema.tables.len());
        assert!(checks
            .iter()
            .all(|c| c.status == crate::check::Status::Pass));
    }

    #[test]
    fn pre_migration_missing_table_fails_with_restart_agent() {
        let m = manifest();
        // Empty database: nothing exists.
        let snap = SchemaSnapshot::default();
        let checks = evaluate_schema(&m, &snap);
        assert!(checks
            .iter()
            .all(|c| c.status == crate::check::Status::Fail));
        let receipts = find(&checks, "tap_horizon_receipts");
        assert!(receipts.message.contains("missing"));
        let rem = receipts.remediation.as_ref().unwrap();
        assert!(rem.contains("indexer-agent"));
        assert!(rem.contains("Restart") || rem.contains("restart"));
    }

    #[test]
    fn empty_legacy_table_suggests_drop_and_recreate() {
        let m = manifest();
        let mut snap = healthy_snapshot(&m);
        // tap_horizon_receipts exists with the legacy shape: no collection_id, empty.
        let legacy_cols = ["id", "allocation_id", "signer_address", "value"];
        snap.tables.insert(
            "tap_horizon_receipts".to_string(),
            TableSnapshot::new(legacy_cols, 0),
        );
        let checks = evaluate_schema(&m, &snap);
        let receipts = find(&checks, "tap_horizon_receipts");
        assert_eq!(receipts.status, crate::check::Status::Fail);
        assert!(receipts.message.contains("legacy shape"));
        assert!(receipts.message.contains("empty"));
        let rem = receipts.remediation.as_ref().unwrap();
        assert!(rem.contains("DROP TABLE tap_horizon_receipts"));
    }

    #[test]
    fn populated_legacy_table_warns_against_drop() {
        let m = manifest();
        let mut snap = healthy_snapshot(&m);
        let legacy_cols = ["id", "allocation_id", "signer_address", "value"];
        snap.tables.insert(
            "tap_horizon_receipts".to_string(),
            TableSnapshot::new(legacy_cols, 42),
        );
        let checks = evaluate_schema(&m, &snap);
        let receipts = find(&checks, "tap_horizon_receipts");
        assert_eq!(receipts.status, crate::check::Status::Fail);
        assert!(receipts.message.contains("42 row"));
        let rem = receipts.remediation.as_ref().unwrap();
        assert!(rem.contains("do NOT drop"));
        assert!(!rem.contains("DROP TABLE"));
    }

    #[test]
    fn missing_non_key_column_fails() {
        let m = manifest();
        let mut snap = healthy_snapshot(&m);
        // Has collection_id but lost `value`.
        let receipts_inv = m
            .schema
            .tables
            .iter()
            .find(|t| t.name == "tap_horizon_receipts")
            .unwrap();
        let mut cols: Vec<String> = receipts_inv.columns.clone();
        cols.retain(|c| c != "value");
        snap.tables.insert(
            "tap_horizon_receipts".to_string(),
            TableSnapshot::new(cols, 0),
        );
        let checks = evaluate_schema(&m, &snap);
        let receipts = find(&checks, "tap_horizon_receipts");
        assert_eq!(receipts.status, crate::check::Status::Fail);
        assert!(receipts.message.contains("value"));
        // Not the legacy-shape path, since collection_id is present.
        assert!(!receipts.message.contains("legacy shape"));
    }

    #[test]
    fn safe_identifier_rejects_injection() {
        assert!(is_safe_identifier("tap_horizon_receipts"));
        assert!(is_safe_identifier("_x1"));
        assert!(!is_safe_identifier("tap; DROP TABLE x"));
        assert!(!is_safe_identifier("1bad"));
        assert!(!is_safe_identifier("has space"));
        assert!(!is_safe_identifier(""));
    }

    #[test]
    fn required_tables_lists_manifest_tables() {
        let m = manifest();
        let tables = required_tables(&m);
        assert!(tables.contains(&"tap_horizon_receipts".to_string()));
        assert_eq!(tables.len(), m.schema.tables.len());
    }
}
