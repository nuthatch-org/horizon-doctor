# horizon-doctor

> Preflight & migration validation CLI for the Graph Protocol indexer stack (Horizon).
> **TOOL-RFC-002** — community tool, not a protocol GRC.

A single read-only binary that validates the coherence of an indexer stack
**before** the Rust components (`indexer-service-rs`, `indexer-tap-agent`) start —
and on demand thereafter. It turns the infamous Horizon-era startup panic into a
controlled, explained gate.

## The problem it solves

`indexer-agent` is the sole owner of database migrations; the Rust components only
*read* the resulting schema. If the migrations haven't run, `indexer-tap-agent`
panics at startup fetching V2 receipts against a schema missing the `collection_id`
column — Postgres error **`42703` (undefined_column)**. The operator sees a raw
panic instead of a diagnosis, receipt aggregation silently stops, and query fees go
unredeemed.

`horizon-doctor` catches that condition first and tells you exactly what to do.
Against a database where the migrations haven't run (remediation column trimmed
here for width):

```text
STATUS  CHECK                         DETAIL                                              REMEDIATION
FAIL    schema.tap_horizon_receipts   required table `tap_horizon_receipts` is missing    indexer-agent migrations have not applied this
                                                                                          schema. Restart indexer-agent to run migrations,
                                                                                          then start indexer-tap-agent.
FAIL    schema.tap_horizon_ravs       required table `tap_horizon_ravs` is missing        …
FAIL    schema.tap_horizon_denylist   required table `tap_horizon_denylist` is missing    …

0 passed, 0 warning(s), 4 failure(s)        # exit code 1
```

After `indexer-agent` migrates, the same command goes green and exits `0` — ready
to gate the dependent component:

```text
STATUS  CHECK                         DETAIL
PASS    schema.tap_horizon_receipts   table `tap_horizon_receipts` present with all 10 required column(s)
PASS    schema.tap_horizon_ravs       table `tap_horizon_ravs` present with all 13 required column(s)
PASS    schema.tap_horizon_denylist   table `tap_horizon_denylist` present with all 1 required column(s)

4 passed, 0 warning(s), 0 failure(s)        # exit code 0
```

It also handles the nastier middle ground: a V2 table left in the *legacy* shape
(missing `collection_id`). If it's empty, `horizon-doctor` tells you it's safe to
`DROP` so `indexer-agent` recreates it with the V2 columns; if it still holds data,
it explicitly warns you **not** to drop it.

## Check families

| # | Family | Status | What it does |
|---|--------|--------|--------------|
| 1 | **schema** | ✅ implemented | Introspects `information_schema` for the V2 TAP tables/columns the Rust components require; detects missing tables, the empty-legacy-table case (suggests a safe `DROP`/recreate), and populated legacy tables (warns against dropping). |
| 2 | **version** | ✅ implemented | Component version matrix against the manifest's Horizon floors (the `v2.0.0` hard requirement for the Rust components). Compares operator-declared versions and, where a metrics endpoint is reachable, cross-checks the running version and warns on drift. |
| 3 | horizon | ⏳ planned | `[horizon].enabled` and required contract addresses present & non-zero. |
| 4 | provision | ⏳ planned | On-chain stake provisioned to the SubgraphService + operator registered. |
| 5 | startup | ⏳ planned | Startup-order guard (agent migrates, then tap-agent reads). |

## Install

```sh
cargo build --release        # binary at target/release/horizon-doctor
```

## Usage

```sh
# Human table
horizon-doctor check --config config.toml

# Machine-readable, for CI / orchestration
horizon-doctor check --config config.toml --json

# Just one family; promote warnings to failures
horizon-doctor check --only schema --strict

# List vendored manifests
horizon-doctor manifests
```

### Exit codes (gate-friendly)

| Code | Meaning |
|------|---------|
| `0` | all checks passed |
| `1` | at least one **FAIL** (or any **WARN** under `--strict`) |
| `2` | warnings only, `--strict` off |
| `3` | operational error (bad config, unknown manifest) |

Use it as a Kubernetes `initContainer` for `indexer-tap-agent`, a systemd
`ExecStartPre`, or an ad-hoc go/no-go during an upgrade window.

### As a Kubernetes initContainer

A non-zero exit blocks the dependent component from starting until the agent has
migrated, converting the silent panic into a controlled, explained wait:

```yaml
initContainers:
  - name: horizon-doctor
    image: ghcr.io/lodestar-team/horizon-doctor:latest
    args: ["check", "--config", "/etc/horizon-doctor/config.toml", "--only", "schema"]
    env:
      - name: DATABASE_URL
        valueFrom:
          secretKeyRef: { name: indexer-db, key: url }
```

### As a systemd ExecStartPre

```ini
[Service]
ExecStartPre=/usr/local/bin/horizon-doctor check --config /etc/horizon-doctor/config.toml
ExecStart=/usr/local/bin/indexer-tap-agent ...
```

## Configuration

See [`config.example.toml`](./config.example.toml). Any secret-bearing value may be
given as `env:VAR_NAME` and is resolved from the environment at load time.

```toml
[doctor]
stack_manifest = "horizon-2.0"
network        = "arbitrum-one"
strict         = false
database_url   = "env:DATABASE_URL"   # read-only introspection
```

### The version matrix

Horizon is a hard cut-over: the Rust components only speak the V2 TAP protocol from
`v2.0.0`. The `version` family enforces that floor. Because the components being
gated usually aren't running yet, you **declare** the versions you're deploying in
`[doctor.versions]`; horizon-doctor compares each against the manifest's floor and
fails hard on anything below `v2.0.0`. Where a Prometheus metrics endpoint is also
configured (typically the already-running `indexer-agent`), it cross-checks the
running `*_build_info{version=...}` label and **warns on drift** between what you
declared and what's actually live. A `v` prefix is tolerated; values may be
`env:VAR` references.

```toml
[doctor.versions]
"indexer-service-rs" = "2.0.0"
"indexer-tap-agent"  = "2.0.0"
"indexer-agent"      = "env:INDEXER_AGENT_VERSION"
```

## Manifests & drift control

Invariants are **vendored** per stack release in [`manifests/`](./manifests) and
embedded in the binary. The schema invariants are derived from the canonical
`tap_horizon_*` migrations in `graphprotocol/indexer-rs` (the source of truth being
the TypeScript `graphprotocol/indexer` repo); the version floors track the Horizon
release line of the same components. CI is intended to diff the manifest against
those migrations and releases to catch drift.

## Safety

Strictly **read-only**: against Postgres it only ever issues `SELECT`s; against a
component's metrics endpoint it only ever issues a single `GET`. It never writes to
the database, never submits transactions, and never logs secrets. Remediation is
always the operator's action.

## Development

```sh
cargo test           # unit + CLI integration tests (no DB needed)

# Opt-in: exercise the real Postgres path against a throwaway instance
docker run --rm -d --name hd-pg -e POSTGRES_PASSWORD=pw -p 5433:5432 postgres:16
HORIZON_DOCTOR_TEST_DATABASE_URL=postgres://postgres:pw@localhost:5433/postgres \
  cargo test --test schema_pg
docker rm -f hd-pg

# Opt-in: exercise the live version cross-check against a real metrics endpoint
HORIZON_DOCTOR_TEST_METRICS_URL=http://127.0.0.1:7300/metrics \
  cargo test --test version_http
```

## License

Apache-2.0.
