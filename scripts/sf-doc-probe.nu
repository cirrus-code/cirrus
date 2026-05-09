#!/usr/bin/env nu

# Run scripts/sf-doc-probe.js to identify which network response carries
# the article body for a given Salesforce help URL.
#
# Caches puppeteer-core under ~/.cache/cloudburst-sdk/probe-deps/ to avoid
# re-installing it on every run. Drops into nix's nodejs to avoid the
# host's potentially-stale node.

def main [url: string] {
    let cache = $"($env.HOME)/.cache/cloudburst-sdk/probe-deps"
    mkdir $cache

    # Pin puppeteer-core via a minimal package.json. CommonJS so the
    # probe's `require(...)` resolves via NODE_PATH below.
    let pkg = $"($cache)/package.json"
    if not ($pkg | path exists) {
        '{"name":"sf-doc-probe","private":true,"dependencies":{"puppeteer-core":"^22.0.0"}}'
        | save -f $pkg
    }
    if not ($"($cache)/node_modules/puppeteer-core" | path exists) {
        print "→ installing puppeteer-core into probe cache (one-time)"
        ^nix shell nixpkgs#nodejs --command bash -c $"cd ($cache); npm install --silent --no-audit --no-fund"
    }

    let probe = ($env.FILE_PWD | path join "sf-doc-probe.js")
    with-env { NODE_PATH: $"($cache)/node_modules" } {
        ^nix shell nixpkgs#nodejs --command node $probe $url
    }
}
