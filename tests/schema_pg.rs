//! Opt-in integration test against a real Postgres.
//!
//! Skipped unless `HORIZON_DOCTOR_TEST_DATABASE_URL` is set. Point it at a
//! throwaway database; the test creates and drops its own tables in a unique
//! schema-free namespace by name, so use a disposable instance.
//!
//! Run the real path with, e.g.:
//!   docker run --rm -d -e POSTGRES_PASSWORD=pw -p 5433:5432 postgres:16
//!   HORIZON_DOCTOR_TEST_DATABASE_URL=postgres://postgres:pw@localhost:5433/postgres \
//!     cargo test --test schema_pg -- --nocapture

use horizon_doctor::check::Status;
use horizon_doctor::manifest::Manifest;
use horizon_doctor::schema::{self, PgSchemaSource, SchemaSource};

fn test_db_url() -> Option<String> {
    std::env::var("HORIZON_DOCTOR_TEST_DATABASE_URL").ok()
}

async fn exec(pool: &sqlx::PgPool, sql: &str) {
    sqlx::query(sql).execute(pool).await.unwrap();
}

/// Drop any of our tables left behind, so the test is idempotent.
async fn cleanup(pool: &sqlx::PgPool) {
    for t in [
        "tap_horizon_receipts",
        "tap_horizon_receipts_invalid",
        "tap_horizon_ravs",
        "tap_horizon_denylist",
    ] {
        exec(pool, &format!("DROP TABLE IF EXISTS {t} CASCADE")).await;
    }
}

#[tokio::test]
async fn real_postgres_pre_and_post_migration() {
    let Some(url) = test_db_url() else {
        eprintln!("skipping: set HORIZON_DOCTOR_TEST_DATABASE_URL to run");
        return;
    };

    let source = PgSchemaSource::connect(&url).await.unwrap();
    let manifest = Manifest::load("horizon-2.0").unwrap();
    let tables = schema::required_tables(&manifest);

    // Use a raw pool for fixture setup.
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    cleanup(&pool).await;

    // --- Pre-migration: nothing exists -> all FAIL.
    let snapshot = source.snapshot(&tables).await.unwrap();
    let checks = schema::evaluate_schema(&manifest, &snapshot);
    assert!(
        checks.iter().all(|c| c.status == Status::Fail),
        "empty DB should fail every schema check"
    );

    // --- Empty legacy table: tap_horizon_receipts without collection_id, 0 rows.
    exec(
        &pool,
        "CREATE TABLE tap_horizon_receipts (id BIGSERIAL PRIMARY KEY, allocation_id CHAR(40), \
         signer_address CHAR(40), value NUMERIC(39))",
    )
    .await;
    let snapshot = source.snapshot(&tables).await.unwrap();
    let checks = schema::evaluate_schema(&manifest, &snapshot);
    let receipts = checks
        .iter()
        .find(|c| c.name == "schema.tap_horizon_receipts")
        .unwrap();
    assert_eq!(receipts.status, Status::Fail);
    assert!(receipts.message.contains("legacy shape"));
    assert!(receipts.message.contains("empty"));
    assert!(receipts
        .remediation
        .as_ref()
        .unwrap()
        .contains("DROP TABLE tap_horizon_receipts"));

    // --- Post-migration: create all four tables with the V2 shape -> all PASS.
    cleanup(&pool).await;
    exec(
        &pool,
        "CREATE TABLE tap_horizon_receipts (id BIGSERIAL PRIMARY KEY, signer_address CHAR(40), \
         signature BYTEA, collection_id CHAR(64), payer CHAR(40), data_service CHAR(40), \
         service_provider CHAR(40), timestamp_ns NUMERIC(20), nonce NUMERIC(20), value NUMERIC(39))",
    )
    .await;
    exec(
        &pool,
        "CREATE TABLE tap_horizon_receipts_invalid (id BIGSERIAL PRIMARY KEY, signer_address CHAR(40), \
         signature BYTEA, collection_id CHAR(64), payer CHAR(40), data_service CHAR(40), \
         service_provider CHAR(40), timestamp_ns NUMERIC(20), nonce NUMERIC(20), value NUMERIC(39), \
         error_log TEXT)",
    )
    .await;
    exec(
        &pool,
        "CREATE TABLE tap_horizon_ravs (signature BYTEA, collection_id CHAR(64), payer CHAR(40), \
         data_service CHAR(40), service_provider CHAR(40), timestamp_ns NUMERIC(20), \
         value_aggregate NUMERIC(39), metadata BYTEA, last BOOLEAN, final BOOLEAN, \
         redeemed_at TIMESTAMPTZ, created_at TIMESTAMPTZ, updated_at TIMESTAMPTZ)",
    )
    .await;
    exec(
        &pool,
        "CREATE TABLE tap_horizon_denylist (sender_address CHAR(40) PRIMARY KEY)",
    )
    .await;

    let snapshot = source.snapshot(&tables).await.unwrap();
    let checks = schema::evaluate_schema(&manifest, &snapshot);
    assert!(
        checks.iter().all(|c| c.status == Status::Pass),
        "post-migration schema should pass: {:?}",
        checks
            .iter()
            .filter(|c| c.status != Status::Pass)
            .collect::<Vec<_>>()
    );

    cleanup(&pool).await;
}
