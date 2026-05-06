# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`cloudburst-sdk` is a Rust HTTP client for the Salesforce API (unaffiliated with Salesforce). It is currently a fresh scaffold — `lib.rs` contains a placeholder `add` function and `main.rs` is "Hello, world!". Treat the constraints below as the design intent for code that hasn't been written yet.

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
