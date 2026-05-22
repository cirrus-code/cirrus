# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`cirrus` is a Rust HTTP client for the Salesforce REST API (unaffiliated with Salesforce). Pre-1.0; not yet on crates.io.

Shipped surface:
- All five priority OAuth flows (JWT, Refresh, Client Credentials, Web Server PKCE, Token Exchange) + Static.
- Phase 1: versions, limits, describe (global + per-object), sObject CRUD, query/queryAll/queryMore, search/parameterizedSearch.
- Phase 2: composite/batch, composite/tree, composite/sobjects (incl. `retrieve_with_body`), generic `/composite`, Bulk 2.0 (ingest + query), Apex REST passthrough, Tooling API, Event Monitoring.
- Cross-cutting: open-ended client escape hatch, pagination stream (`futures::Stream`), retry + backoff policy, `Sforce-Limit-Info` surfacing, auto-refresh on 401, multipart blob uploads.

Tests: 279 unit + 17 doctest, all wiremock-backed, fast (<5s wall). No SOAP, no legacy auth (username-password OAuth, etc.).

## Architecture

### Open-ended client (escape hatch)

Every typed handler layers over a small set of public verb methods on `Cirrus`: `get`, `get_with_query`, `post`, `put`, `patch`, `delete`, plus `request_builder` (auth-injected) and `execute` (hands-off bypass). Path resolution is three-mode:

- **Relative** (`limits`) → versioned: `{instance}/services/data/{version}/limits`
- **Leading slash** (`/services/apexrest/foo`) → instance-rooted
- **Fully-qualified** (`https://...`) → passthrough

When adding a new typed handler, **layer over the public verbs** — don't introduce parallel transport code. Use `versioned_segments` + `send_at` only if a path segment needs percent-encoding (e.g., upsert by external ID with `/` in the value).

### Send-method family

Four internal send paths cover every wire shape we've needed. Pick by request/response shape, not by handler name. All four go through the same retry policy, 401 auto-refresh, and `Sforce-Limit-Info` capture.

| Helper | Request body | Response | Used by |
|---|---|---|---|
| `Cirrus::send` (and public verbs) | `Serialize` JSON or none | typed JSON → `R` | most REST |
| `send_with_body` | raw `bytes::Bytes` + Content-Type | typed JSON → `R` | Bulk 2.0 CSV ingest upload |
| `fetch_raw` | query params only | `(HeaderMap, bytes::Bytes)` | Bulk 2.0 query results, Event Monitoring downloads |
| `send_multipart` | JSON metadata part + binary part | typed JSON → `R` | sObject blob inserts/updates (ContentVersion / Document / Attachment) |

If you find yourself wanting a fifth, first check whether the existing four would work with caller-side adaptation.

### Handler module conventions

- One module per platform-level surface: `crates/cirrus/src/handlers/{sobjects, query, search, composite, bulk, tooling, apex, event_monitoring, limits, versions}.rs`.
- Handler struct holds `&'a Cirrus`; constructed via top-level methods on `Cirrus` (e.g., `sf.tooling()`, `sf.bulk().query()`, `sf.sobject("Account")`).
- Methods that return records expose two variants: the default (returns `serde_json::Value`) and `_as::<R>` for typed deserialization.
- **Never** model org-specific types. Platform envelopes (response shapes Salesforce defines) live in `crates/cirrus/src/response.rs` and are re-exported at the crate root; record types are caller-supplied via the generic `R: DeserializeOwned`.
- Pagination support: handlers with paginated GETs add `_stream` / `_stream_as` variants returning `pagination::Records<R>` (a `futures::Stream`). See `query`/`tooling.query` for the pattern.

## Test conventions

- Wiremock for handler tests; full suite runs in ~0.5s wall time. **No live network in tests.**
- Mock JSON fixtures should cite specific doc pages. When an audit found `BulkQueryJob.query` was a bogus field (Salesforce never returns it), the fix was to stop matching the mock to our wrong assumption and start matching the doc's actual example. Doc-driven > prior-knowledge.
- Each new handler ships with wiremock coverage of: happy path, error array, edge cases documented in the wire shape (partial-success semantics, header cursors, etc.).
- If you can't verify a wire-shape claim against docs, flag it explicitly in code — see `ExecuteAnonymousResult` in `crates/cirrus/src/response.rs` for the established pattern ("Wire-shape provenance" docstring).

### Integration tests

Live tests against a real Salesforce sandbox / dev / scratch org live under `crates/cirrus/tests/integration.rs` (one binary, submodules under `crates/cirrus/tests/integration/`). All `#[ignore]`-gated so they don't run by default.

```bash
# Configure once: copy .env.example to .env, fill in the values
cp .env.example .env

# Run all integration tests (sequential — they share org state)
cargo nextest run --test integration --run-ignored only -- --test-threads=1
```

The harness (`crates/cirrus/tests/integration/common.rs`) refuses to run unless `INSTANCE_URL` matches a known sandbox/dev/scratch My Domain pattern: `.sandbox.`, `.develop.`, `.scratch.`, or `.trailblaze.` infix before `.my.salesforce.com`. The `.trailblaze.` partition is used by free Developer Edition orgs from developer.salesforce.com signup (subdomain ends in `-dev-ed`). Override with `CIRRUS_INTEGRATION_FORCE=1` only after verifying the target org is safe for destructive writes — the safe-list catches Enhanced Domains URLs but not legacy pre-Spring-'23 sandbox URLs, and Salesforce occasionally introduces new partition infixes (audit when adding orgs in unfamiliar shapes).

Auth supports two paths: paste a static token from `sf org display`, or configure JWT bearer flow with a connected app + private key. Static-token mode is the easy bootstrap; JWT exercises the full auth flow.

Don't add network-touching tests to the default (`cargo test`) suite — those should always be wiremock-backed and offline.

## Memory and cross-session notes

Persistent notes the user has flagged for future sessions live in `~/.claude/projects/-home-ryan-Projects-cirrus/memory/`. Notable entries:

- **No legacy or deprecated Salesforce APIs.** Skip anything Salesforce marks legacy (Bulk 1.0, SOAP login, username-password OAuth, etc.).
- **Doc cache goes in `~/.cache/cirrus/sf-docs/`**, not `/tmp` (NixOS tmpfs wipes /tmp on reboot).
- **The Salesforce docs content API** — see `reference_sf_doc_api.md` for endpoint details and the help.salesforce.com gap.

## Repository Layout

This is a **Cargo workspace**. The repo root holds workspace-level config (`Cargo.toml` workspace manifest, `clippy.toml`, `deny.toml`, `flake.nix`, `rust-toolchain.toml`, cross-crate `docs/` and `scripts/`). Each member crate lives under `crates/<name>/` with the standard `src/lib.rs` layout.

```
cirrus/
├── Cargo.toml                  # [workspace] manifest, [workspace.dependencies], [workspace.lints]
├── clippy.toml, deny.toml      # apply to all workspace members
├── docs/, scripts/             # cross-crate
└── crates/
    └── cirrus/                 # the REST client crate
        ├── Cargo.toml          # package manifest, inherits via *.workspace = true
        ├── src/
        ├── tests/
        └── examples/
```

Shared dependency versions are declared in `[workspace.dependencies]` and inherited per-crate via `dep.workspace = true`. Lint denials live in `[workspace.lints.clippy]` and are activated per-crate via `[lints] workspace = true`. New sibling crates inherit both automatically.

## Development Environment

The project uses a Nix flake with `direnv` (`.envrc` is `use flake`). The dev shell provides `rustc`/`cargo` (stable), `clippy`, `rust-analyzer`, `cargo-nextest`, and `cargo-release`. Outside Nix, the `rust-toolchain.toml` pins channel `stable` with `clippy` and `rustfmt`.

Edition is **2024** — code may use features unavailable in older editions.

## Common Commands

All commands run from the workspace root unless noted.

```bash
cargo build --workspace                    # Build all member crates
cargo nextest run --workspace              # Run all tests (preferred — flake provides nextest)
cargo test --workspace                     # Fallback test runner
cargo nextest run -p cirrus <pattern>      # Run a single test by name substring in a specific crate
cargo clippy --all-targets --workspace     # Lint (CI-equivalent — many rules are `deny`)
cargo fmt --all                            # Format every crate
nix build                                  # Reproducible package build via the flake
cargo release -p cirrus <level>            # Release a specific crate; signs commits/tags, pushes to origin, only from main
```

## Coding Constraints (enforced by lints)

The workspace `Cargo.toml` sets these clippy lints to `deny` via `[workspace.lints.clippy]` — every member crate inherits them. Code that trips them will fail `cargo clippy`:

- `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented` — no panicking constructs; propagate errors with `Result`.
- `dbg_macro`, `print_stdout`, `print_stderr` — no ad-hoc stdout/stderr printing. Use a logging facade (e.g. `tracing`/`log`) when one is added.
- `disallowed_types`, `disallowed_methods` — see `clippy.toml`.

`clippy.toml` bans the following in favor of replacements:

- `std::path::Path` / `std::path::PathBuf` → use `camino::Utf8Path` / `camino::Utf8PathBuf`.
- `std::fs::*` (read, write, open, create, remove, copy, rename, metadata, canonicalize, etc.) → use the `fs_err` equivalents so error messages include the offending path.
- `std::fs::OpenOptions` → `fs_err::OpenOptions`.

When adding dependencies, prefer `camino` and `fs_err` for any path/IO work — they're not yet in `[dependencies]` but will be needed as soon as filesystem code is introduced.

## Release Process

`[package.metadata.release]` in `crates/cirrus/Cargo.toml` is configured for `cargo-release`:

- Releases only from `main`.
- Commits and tags are GPG-signed.
- Tags use the `v{{version}}` format and are pushed to `origin`.
- `publish = true` — releases push to crates.io.
