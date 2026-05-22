# cirrus

A family of Rust crates for the Salesforce platform. Not affiliated with Salesforce.

## Crates

| Crate                      | Status  | Description                                                                                                                   |
|----------------------------|---------|-------------------------------------------------------------------------------------------------------------------------------|
| [`cirrus`](crates/cirrus/) | 0.1.0   | HTTP client for the Salesforce REST API — sObject CRUD, SOQL/SOSL, Bulk 2.0, composite, Tooling, Apex REST, Event Monitoring. |
| `cirrus-auth`              | planned | Shared OAuth flows and token management. Extracted from `cirrus`'s `auth/` module.                                            |
| `cirrus-metadata`          | planned | Client for the Salesforce Metadata API (SOAP).                                                                                |

Each crate ships and versions independently. They share a workspace so dependency versions, lint config, and tooling stay consistent.

## Development

The repo is a Nix flake. With [direnv](https://direnv.net) installed, `cd`ing in loads the dev shell automatically; otherwise run `nix develop`. The shell provides stable `rustc`/`cargo`, `clippy`, `rust-analyzer`, `cargo-nextest`, and `cargo-release`.

You do not need to use Nix to contribute to this repository. The Nix developer environment is provided for convenience.

```bash
cargo build --workspace                          # build all crates
cargo nextest run --workspace                    # run all tests (no network)
cargo clippy --all-targets --workspace           # workspace-wide lints
cargo fmt --all                                  # format every crate
```

Per-crate READMEs and crate-specific commands live alongside each crate.

## License

MIT. See [`LICENSE`](LICENSE).
