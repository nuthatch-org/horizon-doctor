//! Command-line interface.

use clap::{Parser, Subcommand, ValueEnum};

use crate::check::Family;

#[derive(Debug, Parser)]
#[command(
    name = "horizon-doctor",
    version,
    about = "Preflight & migration validation for the Graph Protocol indexer stack (Horizon)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the preflight checks and exit with a gate-friendly status code.
    Check(CheckArgs),
    /// List the vendored invariant manifests.
    Manifests,
}

#[derive(Debug, clap::Args)]
pub struct CheckArgs {
    /// Path to the config file (TOML). Defaults to ./config.toml.
    #[arg(
        short,
        long,
        default_value = "config.toml",
        env = "HORIZON_DOCTOR_CONFIG"
    )]
    pub config: std::path::PathBuf,

    /// Emit machine-readable JSON instead of the human table.
    #[arg(long)]
    pub json: bool,

    /// Promote warnings to failures (exit 1 instead of 2).
    #[arg(long)]
    pub strict: bool,

    /// Restrict to specific check families (comma-separated), e.g. --only schema,version.
    #[arg(long, value_delimiter = ',')]
    pub only: Vec<FamilyArg>,

    /// Override the manifest selector from config (e.g. horizon-2.0).
    #[arg(long)]
    pub manifest: Option<String>,

    /// Override the Postgres connection string from config. May be an env: reference.
    #[arg(long)]
    pub database_url: Option<String>,
}

/// CLI-facing mirror of [`Family`] so we control the value strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FamilyArg {
    Schema,
    Version,
    Horizon,
    Provision,
    Startup,
}

impl From<FamilyArg> for Family {
    fn from(f: FamilyArg) -> Self {
        match f {
            FamilyArg::Schema => Family::Schema,
            FamilyArg::Version => Family::Version,
            FamilyArg::Horizon => Family::Horizon,
            FamilyArg::Provision => Family::Provision,
            FamilyArg::Startup => Family::Startup,
        }
    }
}

impl CheckArgs {
    /// The set of families to run: those named in `--only`, or all families when
    /// `--only` is empty.
    pub fn selected_families(&self) -> Vec<Family> {
        if self.only.is_empty() {
            vec![
                Family::Schema,
                Family::Version,
                Family::Horizon,
                Family::Provision,
                Family::Startup,
            ]
        } else {
            self.only.iter().copied().map(Family::from).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_check_with_flags() {
        let cli = Cli::parse_from([
            "horizon-doctor",
            "check",
            "--json",
            "--strict",
            "--only",
            "schema,version",
            "--manifest",
            "horizon-2.0",
        ]);
        let Command::Check(args) = cli.command else {
            panic!("expected check");
        };
        assert!(args.json);
        assert!(args.strict);
        assert_eq!(args.only, vec![FamilyArg::Schema, FamilyArg::Version]);
        assert_eq!(args.manifest.as_deref(), Some("horizon-2.0"));
    }

    #[test]
    fn default_families_is_all() {
        let cli = Cli::parse_from(["horizon-doctor", "check"]);
        let Command::Check(args) = cli.command else {
            panic!("expected check");
        };
        assert_eq!(args.selected_families().len(), 5);
        assert!(!args.json);
    }

    #[test]
    fn only_schema_selects_one() {
        let cli = Cli::parse_from(["horizon-doctor", "check", "--only", "schema"]);
        let Command::Check(args) = cli.command else {
            panic!("expected check");
        };
        assert_eq!(args.selected_families(), vec![Family::Schema]);
    }
}
