# nix-rust-template

My personal project template for Rust projects using Nix.

## Setup

``` nu
mkdir .direnv
"use flake" | save -f .envrc
nix flake lock
cargo generate-lockfile
```

