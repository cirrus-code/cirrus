# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`cloudburst-sdk` is a Rust HTTP client for the Salesforce REST API (unaffiliated with Salesforce). Pre-1.0; not yet on crates.io.

Shipped surface (live snapshot in `TODO.org`):
- All five priority OAuth flows (JWT, Refresh, Client Credentials, Web Server PKCE, Token Exchange) + Static.
- Phase 1: versions, limits, describe (global + per-object), sObject CRUD, query/queryAll/queryMore, search/parameterizedSearch.
- Phase 2: composite/batch, composite/tree, composite/sobjects (incl. `retrieve_with_body`), generic `/composite`, Bulk 2.0 (ingest + query), Apex REST passthrough, Tooling API, Event Monitoring.
- Cross-cutting: open-ended client escape hatch, pagination stream (`futures::Stream`), retry + backoff policy, `Sforce-Limit-Info` surfacing, auto-refresh on 401, multipart blob uploads.

Tests: ~256 unit + 2 doctest, all wiremock-backed, fast (<1s wall). No SOAP, no legacy auth (username-password OAuth, etc.) — see `TODO.org` for the SKIP list.

## Architecture

### Open-ended client (escape hatch)

Every typed handler layers over a small set of public verb methods on `Cloudburst`: `get`, `get_with_query`, `post`, `put`, `patch`, `delete`, plus `request_builder` (auth-injected) and `execute` (hands-off bypass). Path resolution is three-mode:

- **Relative** (`limits`) → versioned: `{instance}/services/data/{version}/limits`
- **Leading slash** (`/services/apexrest/foo`) → instance-rooted
- **Fully-qualified** (`https://...`) → passthrough

When adding a new typed handler, **layer over the public verbs** — don't introduce parallel transport code. Use `versioned_segments` + `send_at` only if a path segment needs percent-encoding (e.g., upsert by external ID with `/` in the value).

### Send-method family

Four internal send paths cover every wire shape we've needed. Pick by request/response shape, not by handler name. All four go through the same retry policy, 401 auto-refresh, and `Sforce-Limit-Info` capture.

| Helper | Request body | Response | Used by |
|---|---|---|---|
| `Cloudburst::send` (and public verbs) | `Serialize` JSON or none | typed JSON → `R` | most REST |
| `send_with_body` | raw `bytes::Bytes` + Content-Type | typed JSON → `R` | Bulk 2.0 CSV ingest upload |
| `fetch_raw` | query params only | `(HeaderMap, bytes::Bytes)` | Bulk 2.0 query results, Event Monitoring downloads |
| `send_multipart` | JSON metadata part + binary part | typed JSON → `R` | sObject blob inserts/updates (ContentVersion / Document / Attachment) |

If you find yourself wanting a fifth, first check whether the existing four would work with caller-side adaptation.

### Handler module conventions

- One module per platform-level surface: `handlers/{sobjects, query, search, composite, bulk, tooling, apex, event_monitoring, limits, versions}.rs`.
- Handler struct holds `&'a Cloudburst`; constructed via top-level methods on `Cloudburst` (e.g., `sf.tooling()`, `sf.bulk().query()`, `sf.sobject("Account")`).
- Methods that return records expose two variants: the default (returns `serde_json::Value`) and `_as::<R>` for typed deserialization.
- **Never** model org-specific types. Platform envelopes (response shapes Salesforce defines) live in `response.rs` and are re-exported at the crate root; record types are caller-supplied via the generic `R: DeserializeOwned`.
- Pagination support: handlers with paginated GETs add `_stream` / `_stream_as` variants returning `pagination::Records<R>` (a `futures::Stream`). See `query`/`tooling.query` for the pattern.

## Documentation fetching

`scripts/sf-doc.nu` is the canonical doc-audit tool. It hits `developer.salesforce.com`'s internal JSON content API directly (not the SPA — that path is slower and racy). Cache lands in `~/.cache/cloudburst-sdk/sf-docs/`.

```bash
# Discover canonical page IDs for a guide (writes manifest-{guide}.json to cache).
nu scripts/sf-doc.nu --guide api_rest --manifest

# Fetch a specific page by id (preferred — see caveat below).
nu scripts/sf-doc.nu --guide api_rest --page resources_query

# Fetch by full URL (works, but URL slugs aren't always page ids).
nu scripts/sf-doc.nu 'https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/dome_query.htm'
```

**Caveats:**

- **URL slug ≠ manifest page id.** A doc-site URL like `tooling_api_rest_query.htm` may not exist as a real page id; the canonical id from the manifest's TOC is `intro_rest_resources`. **Always use `--manifest` first** to find real page ids before fetching individual pages.
- **`xcloud.*` / `help.salesforce.com` pages are out of reach.** OAuth flow specifics live there but the Visualforce/Aura SPA can't be defeated with curl. For auth-flow audits, work from RFC specs (RFC 6749, 7523, 7636, 8693) and cross-references in fetchable guides.
- **The script shells out to `curl`** — its default User-Agent is whitelisted by `developer.salesforce.com`'s docs site, while browser UAs (Mozilla/*) get 403'd. Don't try to "improve" by spoofing browser headers.

The probe scripts (`scripts/sf-doc-probe.js` / `.nu`) are kept for future investigations if Salesforce changes the content endpoint pattern.

## Test conventions

- Wiremock for handler tests; full suite runs in ~0.5s wall time. **No live network in tests.**
- Mock JSON fixtures should cite specific doc pages. When an audit found `BulkQueryJob.query` was a bogus field (Salesforce never returns it), the fix was to stop matching the mock to our wrong assumption and start matching the doc's actual example. Doc-driven > prior-knowledge.
- Each new handler ships with wiremock coverage of: happy path, error array, edge cases documented in the wire shape (partial-success semantics, header cursors, etc.).
- If you can't verify a wire-shape claim against docs, flag it explicitly in code — see `ExecuteAnonymousResult` in `response.rs` for the established pattern ("Wire-shape provenance" docstring).

### Integration tests

Live tests against a real Salesforce sandbox / dev / scratch org live under `tests/integration.rs` (one binary, submodules under `tests/integration/`). All `#[ignore]`-gated so they don't run by default.

```bash
# Configure once: copy .env.example to .env, fill in the values
cp .env.example .env

# Run all integration tests (sequential — they share org state)
cargo nextest run --test integration --run-ignored only -- --test-threads=1
```

The harness (`tests/integration/common.rs`) refuses to run unless `INSTANCE_URL` matches a known sandbox/dev/scratch My Domain pattern (`.sandbox.`, `.develop.`, or `.scratch.` infix before `.my.salesforce.com`). Override with `CLOUDBURST_INTEGRATION_FORCE=1` only after verifying the target org is safe for destructive writes — the safe-list catches Enhanced Domains URLs but not legacy pre-Spring-'23 sandbox URLs.

Auth supports two paths: paste a static token from `sf org display`, or configure JWT bearer flow with a connected app + private key. Static-token mode is the easy bootstrap; JWT exercises the full auth flow.

Don't add network-touching tests to the default (`cargo test`) suite — those should always be wiremock-backed and offline.

## Memory and cross-session notes

Persistent notes the user has flagged for future sessions live in `~/.claude/projects/-home-ryan-Projects-cloudburst-sdk/memory/`. Notable entries:

- **No legacy or deprecated Salesforce APIs.** Skip anything Salesforce marks legacy (Bulk 1.0, SOAP login, username-password OAuth, etc.).
- **Doc cache goes in `~/.cache/cloudburst-sdk/sf-docs/`**, not `/tmp` (NixOS tmpfs wipes /tmp on reboot).
- **The Salesforce docs content API** — see `reference_sf_doc_api.md` for endpoint details and the help.salesforce.com gap.

## Repository Layout

This crate uses a **non-standard layout** — `lib.rs` and `main.rs` live at the repository root, not under `src/`. `Cargo.toml` wires this up explicitly:

```toml
[[bin]]
name = "main"
path = "main.rs"

[lib]
path = "lib.rs"
```

Don't create a `src/` directory unless you intentionally restructure the crate.

## Development Environment

The project uses a Nix flake with `direnv` (`.envrc` is `use flake`). The dev shell provides `rustc`/`cargo` (stable), `clippy`, `rust-analyzer`, `cargo-nextest`, and `cargo-release`. Outside Nix, the `rust-toolchain.toml` pins channel `stable` with `clippy` and `rustfmt`.

Edition is **2024** — code may use features unavailable in older editions.

## Common Commands

```bash
cargo build                  # Build the workspace
cargo run --bin main         # Run the binary
cargo nextest run            # Run tests (preferred — flake provides nextest)
cargo test                   # Fallback test runner
cargo nextest run <pattern>  # Run a single test by name substring
cargo clippy --all-targets   # Lint (CI-equivalent — many rules are `deny`)
cargo fmt                    # Format
nix build                    # Reproducible package build via the flake
cargo release <level>        # Release helper; signs commits/tags, pushes to origin, only from main
```

## Coding Constraints (enforced by lints)

`Cargo.toml` sets these clippy lints to `deny` — code that trips them will fail `cargo clippy`:

- `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented` — no panicking constructs; propagate errors with `Result`.
- `dbg_macro`, `print_stdout`, `print_stderr` — no ad-hoc stdout/stderr printing. Use a logging facade (e.g. `tracing`/`log`) when one is added.
- `disallowed_types`, `disallowed_methods` — see `clippy.toml`.

`clippy.toml` bans the following in favor of replacements:

- `std::path::Path` / `std::path::PathBuf` → use `camino::Utf8Path` / `camino::Utf8PathBuf`.
- `std::fs::*` (read, write, open, create, remove, copy, rename, metadata, canonicalize, etc.) → use the `fs_err` equivalents so error messages include the offending path.
- `std::fs::OpenOptions` → `fs_err::OpenOptions`.

When adding dependencies, prefer `camino` and `fs_err` for any path/IO work — they're not yet in `[dependencies]` but will be needed as soon as filesystem code is introduced.

## Release Process

`[package.metadata.release]` is configured for `cargo-release`:

- Releases only from `main`.
- Commits and tags are GPG-signed.
- Tags use the `v{{version}}` format and are pushed to `origin`.
- `publish = true` — releases push to crates.io.
