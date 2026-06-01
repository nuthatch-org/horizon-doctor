//! `horizon-doctor` binary entry point.

use std::process::ExitCode;

use clap::Parser;

use horizon_doctor::cli::{CheckArgs, Cli, Command};
use horizon_doctor::config::Config;
use horizon_doctor::manifest::Manifest;
use horizon_doctor::output;
use horizon_doctor::runner;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Manifests => {
            for name in Manifest::vendored_names() {
                println!("{name}");
            }
            ExitCode::SUCCESS
        }
        Command::Check(args) => match run_check(args).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("horizon-doctor: {e:#}");
                // Distinct from a normal FAIL (1) / WARN (2): an operational error.
                ExitCode::from(3)
            }
        },
    }
}

async fn run_check(args: CheckArgs) -> anyhow::Result<ExitCode> {
    let config = Config::from_path(&args.config)?;

    let manifest_name = args
        .manifest
        .clone()
        .unwrap_or_else(|| config.doctor.stack_manifest.clone());
    let manifest = Manifest::load(&manifest_name)?;

    // strict is the OR of config and the flag.
    let strict = config.doctor.strict || args.strict;
    let explicit = !args.only.is_empty();
    let families = args.selected_families();

    let report = runner::run(
        &config,
        &manifest,
        &families,
        explicit,
        args.database_url.as_deref(),
    )
    .await;

    if args.json {
        println!("{}", output::render_json(&report, strict)?);
    } else {
        println!("{}", output::render_table(&report));
    }

    Ok(ExitCode::from(report.exit_code(strict) as u8))
}
