//! Check results: the common currency every diagnostic family speaks.
//!
//! A [`Check`] is a single named diagnostic with a [`Status`], a human message,
//! and optional remediation text. A [`CheckReport`] is the collected verdict for
//! a run, and knows how to render an exit code that an `initContainer` or
//! `ExecStartPre` can gate on.

use serde::Serialize;

/// The outcome of a single [`Check`].
///
/// Ordering matters: `Pass < Warn < Fail`, so the worst status in a report is
/// simply the maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    /// The label used in the human-readable table.
    pub fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

/// Which check family produced a result. Used for grouping and `--only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    Schema,
    Version,
    Horizon,
    Provision,
    Startup,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Schema => "schema",
            Family::Version => "version",
            Family::Horizon => "horizon",
            Family::Provision => "provision",
            Family::Startup => "startup",
        }
    }
}

/// A single diagnostic result.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// Stable, machine-friendly identifier, e.g. `schema.tap_horizon_receipts`.
    pub name: String,
    pub family: Family,
    pub status: Status,
    /// Short human description of what was observed.
    pub message: String,
    /// What the operator should do about it, when not a `Pass`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Check {
    pub fn pass(family: Family, name: impl Into<String>, message: impl Into<String>) -> Self {
        Check {
            name: name.into(),
            family,
            status: Status::Pass,
            message: message.into(),
            remediation: None,
        }
    }

    pub fn warn(
        family: Family,
        name: impl Into<String>,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Check {
            name: name.into(),
            family,
            status: Status::Warn,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }

    pub fn fail(
        family: Family,
        name: impl Into<String>,
        message: impl Into<String>,
        remediation: impl Into<String>,
    ) -> Self {
        Check {
            name: name.into(),
            family,
            status: Status::Fail,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }
}

/// The collected verdict of a run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CheckReport {
    pub checks: Vec<Check>,
}

impl CheckReport {
    pub fn new() -> Self {
        CheckReport::default()
    }

    pub fn push(&mut self, check: Check) {
        self.checks.push(check);
    }

    pub fn extend(&mut self, checks: impl IntoIterator<Item = Check>) {
        self.checks.extend(checks);
    }

    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    /// The worst status across all checks, or `Pass` for an empty report.
    pub fn worst(&self) -> Status {
        self.checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(Status::Pass)
    }

    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    /// Process exit code for use as an `initContainer`/`ExecStartPre` gate.
    ///
    /// - `0` — every check passed.
    /// - `1` — at least one check failed (or, under `strict`, at least one warned).
    /// - `2` — warnings only, and `strict` is off.
    pub fn exit_code(&self, strict: bool) -> i32 {
        match self.worst() {
            Status::Pass => 0,
            Status::Fail => 1,
            Status::Warn => {
                if strict {
                    1
                } else {
                    2
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_orders_pass_warn_fail() {
        assert!(Status::Pass < Status::Warn);
        assert!(Status::Warn < Status::Fail);
        assert_eq!(
            [Status::Pass, Status::Fail, Status::Warn].into_iter().max(),
            Some(Status::Fail)
        );
    }

    #[test]
    fn empty_report_passes() {
        let report = CheckReport::new();
        assert_eq!(report.worst(), Status::Pass);
        assert_eq!(report.exit_code(false), 0);
        assert_eq!(report.exit_code(true), 0);
    }

    #[test]
    fn any_fail_yields_exit_1() {
        let mut report = CheckReport::new();
        report.push(Check::pass(Family::Schema, "a", "ok"));
        report.push(Check::fail(Family::Schema, "b", "missing", "do the thing"));
        assert_eq!(report.worst(), Status::Fail);
        assert_eq!(report.exit_code(false), 1);
        assert_eq!(report.exit_code(true), 1);
    }

    #[test]
    fn warn_only_is_exit_2_unless_strict() {
        let mut report = CheckReport::new();
        report.push(Check::pass(Family::Schema, "a", "ok"));
        report.push(Check::warn(Family::Version, "b", "skew", "upgrade"));
        assert_eq!(report.worst(), Status::Warn);
        assert_eq!(report.exit_code(false), 2);
        assert_eq!(report.exit_code(true), 1);
    }

    #[test]
    fn counts_by_status() {
        let mut report = CheckReport::new();
        report.push(Check::pass(Family::Schema, "a", "ok"));
        report.push(Check::pass(Family::Schema, "b", "ok"));
        report.push(Check::warn(Family::Version, "c", "skew", "upgrade"));
        assert_eq!(report.count(Status::Pass), 2);
        assert_eq!(report.count(Status::Warn), 1);
        assert_eq!(report.count(Status::Fail), 0);
    }

    #[test]
    fn pass_check_serializes_without_remediation() {
        let check = Check::pass(Family::Schema, "schema.x", "present");
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["status"], "pass");
        assert_eq!(json["family"], "schema");
        assert!(json.get("remediation").is_none());
    }
}
