#!/usr/bin/env nu

# Fetch a Salesforce help page and convert it to plain text.
#
# Salesforce help articles are Lightning SPAs — `curl` and naive headless
# Chromium dumps return only the loading shell, since the article body is
# fetched by a follow-up XHR after the page mounts. This script drives
# `single-file-cli` (puppeteer + Chromium under the hood, with `networkidle0`
# semantics) to wait for full render, then runs the result through pandoc
# to strip the chrome and asset bloat down to pandoc-plain text.
#
# Examples:
#
#   nu scripts/sf-doc.nu 'https://help.salesforce.com/s/articleView?id=xcloud.remoteaccess_oauth_jwt_flow.htm&language=en_US&type=5'
#   nu scripts/sf-doc.nu --format markdown -o /tmp/jwt.md <URL>
#   nu scripts/sf-doc.nu --keep-html <URL>

def main [
    url: string,                          # Salesforce help / dev-docs URL
    --output (-o): path,                  # Output path; default derived from the URL's `id=` param
    --format (-f): string = "plain",      # pandoc output format (e.g. plain, markdown, gfm)
    --wait-delay: int = 5000,             # ms to wait after networkidle for late XHRs
    --viewport-height: int = 4000,        # tall viewport so all content paints
    --keep-html,                          # keep the intermediate single-file HTML
    --chromium: path,                     # override chromium path (default: $(which chromium))
] {
    let chromium_path = if $chromium == null {
        which chromium | get --optional 0.path
    } else {
        $chromium
    }
    if $chromium_path == null {
        error make { msg: "chromium not found on PATH; pass --chromium /path/to/chromium" }
    }

    let stem = (
        $url
        | parse --regex 'id=(?<id>[^&]+)'
        | get --optional 0.id
        | default (random uuid | str substring 0..8)
    )
    let out = ($output | default $"/tmp/sf-doc-($stem).($format)")
    let html = $"/tmp/sf-doc-($stem).html"

    print $"→ rendering ($url)"
    (
        ^nix run nixpkgs#single-file-cli --
            $url $html
            --browser-executable-path $chromium_path
            --block-scripts=false
            --block-images
            --block-fonts
            --browser-wait-delay $wait_delay
            --browser-height $viewport_height
            --browser-wait-until networkidle0
    )

    print $"→ pandoc → ($out)"
    ^pandoc -f html -t $format $html -o $out

    if not $keep_html {
        rm $html
    }

    let line_count = (^wc -l $out | split row ' ' | get 0 | into int)
    print $"✓ wrote ($out) \(($line_count) lines\)"
    print $out
}
