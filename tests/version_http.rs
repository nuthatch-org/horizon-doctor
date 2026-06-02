//! Opt-in integration test for the live version cross-check.
//!
//! Skipped unless `HORIZON_DOCTOR_TEST_METRICS_URL` is set. Point it at a running
//! component's Prometheus metrics endpoint; set `HORIZON_DOCTOR_TEST_METRIC` to the
//! build-info metric name (defaults to `tap_agent_build_info`). The test only ever
//! issues a read-only GET.
//!
//! Run with, e.g.:
//!   HORIZON_DOCTOR_TEST_METRICS_URL=http://127.0.0.1:7300/metrics \
//!     cargo test --test version_http -- --nocapture

use horizon_doctor::version::{HttpVersionSource, Probe, Scraped, VersionSource};

#[tokio::test]
async fn live_metrics_scrape_reads_a_version() {
    let Ok(url) = std::env::var("HORIZON_DOCTOR_TEST_METRICS_URL") else {
        eprintln!("skipping: set HORIZON_DOCTOR_TEST_METRICS_URL to run");
        return;
    };
    let metric = std::env::var("HORIZON_DOCTOR_TEST_METRIC")
        .unwrap_or_else(|_| "tap_agent_build_info".into());

    let source = HttpVersionSource::new();
    let probes = vec![Probe {
        component: "indexer-tap-agent".to_string(),
        url,
        metric,
    }];
    let scraped = source.scrape(&probes).await;

    match scraped.get("indexer-tap-agent") {
        Some(Scraped::Version(v)) => eprintln!("scraped version: {v}"),
        Some(Scraped::MetricAbsent) => {
            panic!("endpoint reached but the build-info metric/version label was absent")
        }
        Some(Scraped::Unreachable(why)) => panic!("endpoint unreachable: {why}"),
        None => panic!("no result for the probed component"),
    }
}
