//! End-to-end CLI tests that don't need a database: they exercise config/manifest
//! loading, output rendering, and exit codes via the public library surface used
//! by `main`.

use horizon_doctor::check::{Check, CheckReport, Family};
use horizon_doctor::config::Config;
use horizon_doctor::manifest::Manifest;
use horizon_doctor::output;
use horizon_doctor::runner;

#[tokio::test]
async fn missing_db_url_reports_actionable_fail() {
    let config = Config::from_toml(
        r#"
        [doctor]
        stack_manifest = "horizon-2.0"
    "#,
    )
    .unwrap();
    let manifest = Manifest::load("horizon-2.0").unwrap();

    let report = runner::run(&config, &manifest, &[Family::Schema], true, None).await;

    assert_eq!(report.exit_code(false), 1);
    let json = output::render_json(&report, false).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["worst"], "fail");
    assert_eq!(v["checks"][0]["name"], "schema.config");
    assert!(v["checks"][0]["remediation"].is_string());
}

/// Golden-ish shape test for the `--json` envelope, so consumers can rely on it.
#[test]
fn json_envelope_shape_is_stable() {
    let mut report = CheckReport::new();
    report.push(Check::pass(
        Family::Schema,
        "schema.tap_horizon_receipts",
        "table `tap_horizon_receipts` present with all 10 required column(s)",
    ));
    report.push(Check::fail(
        Family::Schema,
        "schema.tap_horizon_ravs",
        "required table `tap_horizon_ravs` is missing",
        "Restart indexer-agent.",
    ));

    let v: serde_json::Value =
        serde_json::from_str(&output::render_json(&report, false).unwrap()).unwrap();

    // Top-level keys.
    for key in ["worst", "exit_code", "summary", "checks"] {
        assert!(v.get(key).is_some(), "missing top-level key {key}");
    }
    assert_eq!(v["exit_code"], 1);
    assert_eq!(v["summary"]["pass"], 1);
    assert_eq!(v["summary"]["fail"], 1);
    assert_eq!(v["summary"]["warn"], 0);
    assert_eq!(v["summary"]["total"], 2);

    // Per-check keys.
    let first = &v["checks"][0];
    assert_eq!(first["family"], "schema");
    assert_eq!(first["status"], "pass");
    assert!(first.get("remediation").is_none()); // omitted on pass
}
