//! Check family #2: the version matrix.
//!
//! Same architecture as [`crate::schema`]: a plain data [`VersionSnapshot`]
//! describes what we observed about each component, and [`evaluate_version`] is a
//! *pure* function from `(Manifest, VersionSnapshot)` to [`Check`]s — no IO,
//! exhaustively unit-testable.
//!
//! What we observe per component is two-sided:
//!
//! * `declared` — the version the operator says they're deploying, from
//!   `[doctor.versions]`. This is the source of truth in the preflight/gate case,
//!   because the components being gated aren't running yet.
//! * `scraped` — the version a *running* component reports via its Prometheus
//!   metrics endpoint, when one is configured (typically the already-running
//!   `indexer-agent`). Used to cross-check the declared value and flag drift.
//!
//! IO lives behind [`VersionSource`]; the production [`HttpVersionSource`] does a
//! read-only `GET` of the metrics endpoint and parses the `version` label.

use std::collections::BTreeMap;

use semver::Version;

use crate::check::{Check, Family};
use crate::manifest::{ComponentInvariant, Manifest};

/// The outcome of trying to read a running component's version from its metrics.
#[derive(Debug, Clone)]
pub enum Scraped {
    /// The metric was found and carried a `version` label.
    Version(String),
    /// The endpoint was reached but the expected metric/label was absent.
    MetricAbsent,
    /// The endpoint could not be reached or read (carries a short reason).
    Unreachable(String),
}

/// What we know about one component at evaluation time.
#[derive(Debug, Clone, Default)]
pub struct Observed {
    /// The operator-declared deployed version, if any.
    pub declared: Option<String>,
    /// The live cross-check result, if a metrics endpoint was probed.
    pub scraped: Option<Scraped>,
}

impl Observed {
    pub fn declared(version: impl Into<String>) -> Self {
        Observed {
            declared: Some(version.into()),
            scraped: None,
        }
    }
}

/// A read-only picture of the deployed versions, keyed by component name.
#[derive(Debug, Clone, Default)]
pub struct VersionSnapshot {
    pub components: BTreeMap<String, Observed>,
}

impl VersionSnapshot {
    pub fn with_component(mut self, name: impl Into<String>, observed: Observed) -> Self {
        self.components.insert(name.into(), observed);
        self
    }
}

/// Strip a leading `v`/`V` so `v2.0.0` and `2.0.0` parse alike.
fn parse_semver(raw: &str) -> Option<Version> {
    let trimmed = raw.trim();
    let stripped = trimmed.strip_prefix(['v', 'V']).unwrap_or(trimmed);
    Version::parse(stripped).ok()
}

/// Default remediation when a component's deployed version can't be determined.
fn unknown_remediation(name: &str) -> String {
    format!(
        "Declare the deployed version of `{name}` under [doctor.versions] (it may be an \
         env: reference), or configure its metrics endpoint so horizon-doctor can read it."
    )
}

/// Remediation for a component below the Horizon hard floor.
fn below_hard_remediation(name: &str, min: &str) -> String {
    format!(
        "`{name}` must be >= {min} for Horizon (V2): older builds speak only the legacy TAP \
         protocol and will fail against the V2 schema. Upgrade `{name}` to {min} or newer."
    )
}

/// The label horizon-doctor uses to describe where a version came from.
fn source_label(declared: bool) -> &'static str {
    if declared {
        "declared"
    } else {
        "metrics"
    }
}

/// Evaluate one component invariant against what we observed.
fn evaluate_component(inv: &ComponentInvariant, observed: &Observed) -> Vec<Check> {
    let name = format!("version.{}", inv.name);
    let mut checks = Vec::new();

    // The declared value is authoritative; fall back to a live scrape.
    let scraped_version = match &observed.scraped {
        Some(Scraped::Version(v)) => Some(v.clone()),
        _ => None,
    };
    let (effective, from_declared) = match (&observed.declared, &scraped_version) {
        (Some(d), _) => (Some(d.clone()), true),
        (None, Some(s)) => (Some(s.clone()), false),
        (None, None) => (None, false),
    };

    match effective {
        None => {
            // Nothing to compare. A hard floor we can't verify is a WARN, not a
            // FAIL: the gate shouldn't block purely for want of a declared version.
            let detail = match &observed.scraped {
                Some(Scraped::Unreachable(why)) => {
                    format!("deployed version unknown; metrics endpoint unreachable ({why})")
                }
                Some(Scraped::MetricAbsent) => {
                    "deployed version unknown; metrics endpoint exposed no version".to_string()
                }
                _ => "deployed version unknown".to_string(),
            };
            let remediation = inv
                .remediation
                .clone()
                .unwrap_or_else(|| unknown_remediation(&inv.name));
            checks.push(Check::warn(
                Family::Version,
                name.clone(),
                detail,
                remediation,
            ));
        }
        Some(ref ver_str) => {
            let src = source_label(from_declared);
            match parse_semver(ver_str) {
                None => {
                    checks.push(Check::warn(
                        Family::Version,
                        name.clone(),
                        format!("reported version `{ver_str}` ({src}) is not valid semver"),
                        "Ensure the declared/reported version is a semantic version, e.g. 2.0.1.",
                    ));
                }
                Some(ver) => match &inv.min_version {
                    // Report-only component: no floor to enforce.
                    None => {
                        checks.push(Check::pass(
                            Family::Version,
                            name.clone(),
                            format!("`{}` {ver} ({src})", inv.name),
                        ));
                    }
                    Some(min_str) => match parse_semver(min_str) {
                        None => {
                            checks.push(Check::warn(
                                Family::Version,
                                name.clone(),
                                format!("manifest min_version `{min_str}` is not valid semver"),
                                "Fix the manifest's min_version; this is a horizon-doctor manifest bug.",
                            ));
                        }
                        Some(min) if ver >= min => {
                            checks.push(Check::pass(
                                Family::Version,
                                name.clone(),
                                format!("`{}` {ver} ({src}) satisfies >= {min}", inv.name),
                            ));
                        }
                        Some(min) if inv.hard => {
                            checks.push(Check::fail(
                                Family::Version,
                                name.clone(),
                                format!(
                                    "`{}` {ver} ({src}) is below the Horizon floor {min}",
                                    inv.name
                                ),
                                inv.remediation
                                    .clone()
                                    .unwrap_or_else(|| below_hard_remediation(&inv.name, min_str)),
                            ));
                        }
                        Some(min) => {
                            checks.push(Check::warn(
                                Family::Version,
                                name.clone(),
                                format!(
                                    "`{}` {ver} ({src}) is below the recommended {min}",
                                    inv.name
                                ),
                                inv.remediation.clone().unwrap_or_else(|| {
                                    format!("Consider upgrading `{}` to {min} or newer.", inv.name)
                                }),
                            ));
                        }
                    },
                },
            }
        }
    }

    // Drift cross-check: declared and live disagree. Independent of the floor
    // verdict, because a passing-but-mislabelled deployment is its own hazard.
    if let (Some(declared), Some(scraped)) = (&observed.declared, &scraped_version) {
        let differ = match (parse_semver(declared), parse_semver(scraped)) {
            (Some(a), Some(b)) => a != b,
            // Unparseable on either side: fall back to a string compare.
            _ => declared.trim() != scraped.trim(),
        };
        if differ {
            checks.push(Check::warn(
                Family::Version,
                format!("{name}.drift"),
                format!(
                    "declared version `{declared}` does not match the running `{scraped}` reported by metrics"
                ),
                "Reconcile [doctor.versions] with what is actually deployed before relying on the gate.",
            ));
        }
    }

    checks
}

/// Evaluate the version family for a manifest against a snapshot. Pure.
pub fn evaluate_version(manifest: &Manifest, snapshot: &VersionSnapshot) -> Vec<Check> {
    let default = Observed::default();
    manifest
        .version
        .components
        .iter()
        .flat_map(|inv| {
            let observed = snapshot.components.get(&inv.name).unwrap_or(&default);
            evaluate_component(inv, observed)
        })
        .collect()
}

// ----------------------------------------------------------------------------
// Prometheus text-format parsing.
// ----------------------------------------------------------------------------

/// Extract a label value from the first matching sample of a Prometheus metric in
/// text-exposition format, e.g. the `2.1.0` from:
///
/// ```text
/// tap_agent_build_info{version="2.1.0",commit="abc"} 1
/// ```
///
/// Comment lines (`# HELP`/`# TYPE`) are ignored. Returns `None` if the metric or
/// label is absent.
pub fn extract_metric_label(text: &str, metric: &str, label: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The metric name is everything up to the first `{` or whitespace.
        let name_end = line
            .find(|c: char| c == '{' || c.is_whitespace())
            .unwrap_or(line.len());
        if &line[..name_end] != metric {
            continue;
        }
        let Some(open) = line.find('{') else {
            continue; // sample has no labels; nothing to extract.
        };
        let Some(close) = line[open..].find('}') else {
            continue;
        };
        let labels = &line[open + 1..open + close];
        if let Some(value) = find_label(labels, label) {
            return Some(value);
        }
    }
    None
}

/// Pull `value` out of a `key="value"` pair within a Prometheus label set.
fn find_label(labels: &str, key: &str) -> Option<String> {
    for pair in labels.split(',') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?.trim();
        let v = it.next()?.trim();
        if k == key {
            return Some(v.trim_matches('"').to_string());
        }
    }
    None
}

// ----------------------------------------------------------------------------
// Live version source (read-only HTTP).
// ----------------------------------------------------------------------------

/// A single component's live-version probe: where to look and what to look for.
#[derive(Debug, Clone)]
pub struct Probe {
    pub component: String,
    pub url: String,
    pub metric: String,
}

/// A source of live component versions. Implemented by [`HttpVersionSource`];
/// tests build [`VersionSnapshot`]s directly and need no source at all.
pub trait VersionSource {
    fn scrape(
        &self,
        probes: &[Probe],
    ) -> impl std::future::Future<Output = BTreeMap<String, Scraped>> + Send;
}

/// Reads versions from running components' Prometheus metrics endpoints. Strictly
/// read-only: a single `GET` per probe, with a short timeout.
pub struct HttpVersionSource {
    client: reqwest::Client,
}

impl HttpVersionSource {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .expect("reqwest client should build with default config");
        HttpVersionSource { client }
    }

    async fn scrape_one(&self, probe: &Probe) -> Scraped {
        let resp = match self.client.get(&probe.url).send().await {
            Ok(r) => r,
            Err(e) => return Scraped::Unreachable(short_err(&e)),
        };
        if let Err(e) = resp.error_for_status_ref() {
            return Scraped::Unreachable(short_err(&e));
        }
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return Scraped::Unreachable(short_err(&e)),
        };
        match extract_metric_label(&body, &probe.metric, "version") {
            Some(v) => Scraped::Version(v),
            None => Scraped::MetricAbsent,
        }
    }
}

impl Default for HttpVersionSource {
    fn default() -> Self {
        Self::new()
    }
}

/// A compact, secret-free reason string for a request error.
fn short_err(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "connection refused".to_string()
    } else if let Some(status) = e.status() {
        format!("HTTP {status}")
    } else {
        "request failed".to_string()
    }
}

impl VersionSource for HttpVersionSource {
    async fn scrape(&self, probes: &[Probe]) -> BTreeMap<String, Scraped> {
        let mut out = BTreeMap::new();
        for probe in probes {
            out.insert(probe.component.clone(), self.scrape_one(probe).await);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::Status;

    fn manifest() -> Manifest {
        Manifest::load("horizon-2.0").unwrap()
    }

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn declared_at_or_above_floor_passes() {
        let m = manifest();
        let snap = VersionSnapshot::default()
            .with_component("indexer-service-rs", Observed::declared("2.1.0"))
            .with_component("indexer-tap-agent", Observed::declared("2.0.0"));
        let checks = evaluate_version(&m, &snap);
        assert_eq!(
            find(&checks, "version.indexer-service-rs").status,
            Status::Pass
        );
        assert_eq!(
            find(&checks, "version.indexer-tap-agent").status,
            Status::Pass
        );
    }

    #[test]
    fn declared_below_hard_floor_fails_with_horizon_remediation() {
        let m = manifest();
        let snap = VersionSnapshot::default()
            .with_component("indexer-tap-agent", Observed::declared("1.9.0"));
        let checks = evaluate_version(&m, &snap);
        let c = find(&checks, "version.indexer-tap-agent");
        assert_eq!(c.status, Status::Fail);
        assert!(c.message.contains("below the Horizon floor"));
        let rem = c.remediation.as_ref().unwrap();
        assert!(rem.contains("2.0.0"));
        assert!(rem.contains("Horizon"));
    }

    #[test]
    fn v_prefix_is_tolerated() {
        let m = manifest();
        let snap = VersionSnapshot::default()
            .with_component("indexer-service-rs", Observed::declared("v2.0.1"));
        let checks = evaluate_version(&m, &snap);
        assert_eq!(
            find(&checks, "version.indexer-service-rs").status,
            Status::Pass
        );
    }

    #[test]
    fn unknown_version_warns_not_fails() {
        let m = manifest();
        // Nothing declared, nothing scraped: every component is unknown.
        let snap = VersionSnapshot::default();
        let checks = evaluate_version(&m, &snap);
        assert!(!checks.is_empty());
        assert!(checks.iter().all(|c| c.status == Status::Warn));
        let c = find(&checks, "version.indexer-tap-agent");
        assert!(c.message.contains("unknown"));
    }

    #[test]
    fn report_only_component_passes_with_any_version() {
        let m = manifest();
        // indexer-agent has no min_version: any valid semver is a PASS.
        let snap = VersionSnapshot::default()
            .with_component("indexer-agent", Observed::declared("0.21.3"));
        let checks = evaluate_version(&m, &snap);
        let c = find(&checks, "version.indexer-agent");
        assert_eq!(c.status, Status::Pass);
        assert!(c.message.contains("0.21.3"));
    }

    #[test]
    fn invalid_semver_warns() {
        let m = manifest();
        let snap = VersionSnapshot::default()
            .with_component("indexer-tap-agent", Observed::declared("not-a-version"));
        let checks = evaluate_version(&m, &snap);
        let c = find(&checks, "version.indexer-tap-agent");
        assert_eq!(c.status, Status::Warn);
        assert!(c.message.contains("not valid semver"));
    }

    #[test]
    fn declared_and_scraped_disagree_emits_drift_warning() {
        let m = manifest();
        let observed = Observed {
            declared: Some("2.1.0".to_string()),
            scraped: Some(Scraped::Version("2.0.0".to_string())),
        };
        let snap = VersionSnapshot::default().with_component("indexer-tap-agent", observed);
        let checks = evaluate_version(&m, &snap);
        // Primary check still passes (2.1.0 >= 2.0.0)...
        assert_eq!(
            find(&checks, "version.indexer-tap-agent").status,
            Status::Pass
        );
        // ...but drift is surfaced separately.
        let drift = find(&checks, "version.indexer-tap-agent.drift");
        assert_eq!(drift.status, Status::Warn);
        assert!(drift.message.contains("does not match"));
    }

    #[test]
    fn matching_declared_and_scraped_emits_no_drift() {
        let m = manifest();
        let observed = Observed {
            declared: Some("2.0.0".to_string()),
            scraped: Some(Scraped::Version("v2.0.0".to_string())),
        };
        let snap = VersionSnapshot::default().with_component("indexer-tap-agent", observed);
        let checks = evaluate_version(&m, &snap);
        assert!(checks.iter().all(|c| !c.name.ends_with(".drift")));
    }

    #[test]
    fn scraped_only_falls_back_to_metrics_source() {
        let m = manifest();
        let observed = Observed {
            declared: None,
            scraped: Some(Scraped::Version("2.0.0".to_string())),
        };
        let snap = VersionSnapshot::default().with_component("indexer-service-rs", observed);
        let checks = evaluate_version(&m, &snap);
        let c = find(&checks, "version.indexer-service-rs");
        assert_eq!(c.status, Status::Pass);
        assert!(c.message.contains("(metrics)"));
    }

    #[test]
    fn unreachable_endpoint_reports_reason() {
        let m = manifest();
        let observed = Observed {
            declared: None,
            scraped: Some(Scraped::Unreachable("connection refused".to_string())),
        };
        let snap = VersionSnapshot::default().with_component("indexer-agent", observed);
        let checks = evaluate_version(&m, &snap);
        let c = find(&checks, "version.indexer-agent");
        assert_eq!(c.status, Status::Warn);
        assert!(c.message.contains("connection refused"));
    }

    #[test]
    fn prometheus_extract_reads_version_label() {
        let body = "# HELP tap_agent_build_info Build info\n\
                    # TYPE tap_agent_build_info gauge\n\
                    tap_agent_build_info{version=\"2.1.0\",commit=\"deadbeef\"} 1\n";
        assert_eq!(
            extract_metric_label(body, "tap_agent_build_info", "version").as_deref(),
            Some("2.1.0")
        );
    }

    #[test]
    fn prometheus_extract_handles_label_order_and_misses() {
        let body = "other_metric{version=\"9.9.9\"} 1\n\
                    tap_agent_build_info{commit=\"abc\",version=\"2.0.0\"} 1\n";
        // Must match the right metric, regardless of label order.
        assert_eq!(
            extract_metric_label(body, "tap_agent_build_info", "version").as_deref(),
            Some("2.0.0")
        );
        // Absent label -> None.
        assert_eq!(
            extract_metric_label(body, "tap_agent_build_info", "branch"),
            None
        );
        // Absent metric -> None.
        assert_eq!(
            extract_metric_label(body, "nope_build_info", "version"),
            None
        );
    }

    #[test]
    fn prometheus_extract_ignores_metric_without_labels() {
        let body = "tap_agent_build_info 1\n";
        assert_eq!(
            extract_metric_label(body, "tap_agent_build_info", "version"),
            None
        );
    }
}
