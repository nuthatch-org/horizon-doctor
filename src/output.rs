//! Rendering: human-readable table and machine-readable JSON.

use comfy_table::{Cell, Color, ContentArrangement, Table};
use serde::Serialize;

use crate::check::{CheckReport, Status};

/// JSON envelope: a summary plus the individual checks.
#[derive(Debug, Serialize)]
struct JsonReport<'a> {
    worst: Status,
    exit_code: i32,
    summary: Summary,
    checks: &'a [crate::check::Check],
}

#[derive(Debug, Serialize)]
struct Summary {
    pass: usize,
    warn: usize,
    fail: usize,
    total: usize,
}

/// Render the report as pretty JSON for `--json` / CI consumption.
pub fn render_json(report: &CheckReport, strict: bool) -> serde_json::Result<String> {
    let envelope = JsonReport {
        worst: report.worst(),
        exit_code: report.exit_code(strict),
        summary: Summary {
            pass: report.count(Status::Pass),
            warn: report.count(Status::Warn),
            fail: report.count(Status::Fail),
            total: report.checks.len(),
        },
        checks: &report.checks,
    };
    serde_json::to_string_pretty(&envelope)
}

fn status_cell(status: Status) -> Cell {
    let color = match status {
        Status::Pass => Color::Green,
        Status::Warn => Color::Yellow,
        Status::Fail => Color::Red,
    };
    Cell::new(status.label()).fg(color)
}

/// Render the report as a human-readable table.
pub fn render_table(report: &CheckReport) -> String {
    let mut table = Table::new();
    table
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["STATUS", "CHECK", "DETAIL", "REMEDIATION"]);

    for check in &report.checks {
        table.add_row(vec![
            status_cell(check.status),
            Cell::new(&check.name),
            Cell::new(&check.message),
            Cell::new(check.remediation.as_deref().unwrap_or("—")),
        ]);
    }

    let summary = format!(
        "\n{} passed, {} warning(s), {} failure(s)",
        report.count(Status::Pass),
        report.count(Status::Warn),
        report.count(Status::Fail),
    );

    format!("{table}{summary}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{Check, Family};

    fn sample() -> CheckReport {
        let mut r = CheckReport::new();
        r.push(Check::pass(
            Family::Schema,
            "schema.tap_horizon_receipts",
            "table `tap_horizon_receipts` present with all 10 required column(s)",
        ));
        r.push(Check::fail(
            Family::Schema,
            "schema.tap_horizon_ravs",
            "required table `tap_horizon_ravs` is missing",
            "Restart indexer-agent to run migrations.",
        ));
        r
    }

    #[test]
    fn json_has_summary_and_checks() {
        let r = sample();
        let json = render_json(&r, false).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["worst"], "fail");
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["summary"]["pass"], 1);
        assert_eq!(v["summary"]["fail"], 1);
        assert_eq!(v["summary"]["total"], 2);
        assert_eq!(v["checks"].as_array().unwrap().len(), 2);
        assert_eq!(v["checks"][0]["status"], "pass");
    }

    #[test]
    fn table_mentions_checks_and_summary() {
        let r = sample();
        let out = render_table(&r);
        assert!(out.contains("schema.tap_horizon_receipts"));
        assert!(out.contains("schema.tap_horizon_ravs"));
        assert!(out.contains("1 passed, 0 warning(s), 1 failure(s)"));
    }
}
