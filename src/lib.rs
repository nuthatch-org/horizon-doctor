//! `horizon-doctor` — preflight & migration validation for the Graph Protocol
//! indexer stack under Horizon.
//!
//! The library is split into small, testable pieces:
//!
//! * [`check`] — the result vocabulary ([`check::Check`], [`check::CheckReport`]).
//! * [`manifest`] — versioned invariant sets, vendored in `manifests/`.
//! * [`schema`] — check family #1 (schema coherence), the panic-prevention core.
//! * [`config`] — the `[doctor]` config table with `env:` resolution.
//! * [`runner`] — orchestrates families into a report.
//! * [`output`] — human table and `--json` rendering.
//! * [`cli`] — the command-line surface.
//!
//! It is strictly read-only against Postgres, endpoints, and chain.

pub mod check;
pub mod cli;
pub mod config;
pub mod manifest;
pub mod output;
pub mod runner;
pub mod schema;
