#!/usr/bin/env nu

# Fetch a Salesforce help-doc page and convert it to plain text.
#
# Salesforce's developer.salesforce.com Lightning SPA loads each article
# body via an internal JSON content API that this script hits directly:
#
#   https://developer.salesforce.com/docs/get_document_content/{guide}/{page}.htm/{lang}/{version}
#
# The response is JSON `{id, title, content}` where `content` is HTML.
# We extract `content`, hand it to pandoc, and write the result out.
#
# This replaces the previous puppeteer/single-file-cli flow, which was
# (a) ~50× slower and (b) racy — pages with delayed XHRs would render as
# the navigation/intro shell instead of the article body. A historical
# probe (scripts/sf-doc-probe.js) identified the content endpoint by
# watching the SPA's network traffic.
#
# Usage:
#
#   nu scripts/sf-doc.nu '<URL>'
#   nu scripts/sf-doc.nu '<URL>' --output ~/.cache/cloudburst-sdk/sf-docs/foo.md --format markdown
#   nu scripts/sf-doc.nu --guide api_tooling --page intro_rest_resources
#   nu scripts/sf-doc.nu --guide api_tooling --manifest    # dump TOC instead of an article
#
# The two-flag form is preferred when you've discovered the canonical
# page ID from the manifest — URL-derived page slugs sometimes differ
# from the manifest's actual `id` (e.g. `tooling_api_rest_query.htm`
# does not exist; the real id is `intro_rest_resources`).

const CONTENT_BASE = "https://developer.salesforce.com/docs/get_document_content"
const MANIFEST_BASE = "https://developer.salesforce.com/docs/get_document"
const HELP_CONTENT_BASE = "https://help.salesforce.com/apex/HCVFPropertiesAccessor"
const DEFAULT_LANG = "en-us"

# Guide namespaces that live on help.salesforce.com instead of
# developer.salesforce.com. The OAuth flow docs (xcloud.*), feature
# release notes (sfdo.*), and admin-facing how-to articles all use the
# Visualforce-rendered help portal. There's no JSON content API for
# these — only an HTML fragment served by HCVFPropertiesAccessor.
const HELP_GUIDE_NAMESPACES = ["xcloud" "sfdo" "sf"]

# We shell out to `curl` rather than using nu's `http get`. Reason:
# developer.salesforce.com's docs site ironically rejects browser-like
# User-Agents (Mozilla/5.0 → 403 or connection drop) but accepts curl's
# default `curl/X.Y.Z`. nushell's default UA is also rejected. Trying to
# spoof curl's UA from nu is doable but fragile — easier to just use
# curl. This keeps the script's network behavior identical to what we
# verified manually.

# Parse a Salesforce help URL into (guide, page).
def parse-doc-url [url: string] {
    let m = (
        $url
        | parse --regex 'atlas\.[a-z-]+\.(?<guide>[A-Za-z0-9_]+)\.meta/[A-Za-z0-9_]+/(?<page>[A-Za-z0-9_]+)\.htm'
        | get --optional 0
    )
    if $m != null {
        return { guide: $m.guide, page: $m.page }
    }
    let h = (
        $url
        | parse --regex 'id=(?<guide>[A-Za-z0-9_]+)\.(?<page>[A-Za-z0-9_]+)\.htm'
        | get --optional 0
    )
    if $h != null {
        return { guide: $h.guide, page: $h.page }
    }
    error make { msg: $"could not parse guide/page from URL: ($url)" }
}

# Fetch the guide manifest. Used to (a) discover the current docs-site
# version so the content endpoint URL stays valid as Salesforce ships
# docs releases, and (b) dump the TOC when --manifest is set.
def fetch-manifest [guide: string, lang: string] {
    let url = $"($MANIFEST_BASE)/atlas.($lang).($guide).meta"
    ^curl --silent --show-error --fail --max-time 30 $url | from json
}

# Fetch the article content as JSON {id, title, content (HTML)} from
# the developer.salesforce.com docs API.
def fetch-content [guide: string, page: string, lang: string, version: string] {
    let url = $"($CONTENT_BASE)/($guide)/($page).htm/($lang)/($version)"
    let raw = (^curl --silent --show-error --fail --max-time 30 $url)
    if ($raw | str length) == 0 {
        error make { msg: $"content endpoint returned empty body — page id '($page)' may not exist in guide '($guide)'. Try `--guide ($guide) --manifest` to list valid IDs." }
    }
    $raw | from json
}

# Fetch help.salesforce.com article content as raw HTML. Returns a
# pseudo-record matching fetch-content's output so the caller doesn't
# branch.
def fetch-help-content [guide: string, page: string, lang: string] {
    # help.salesforce.com expects underscore-locale (en_US), not
    # hyphen-locale (en-us). Translate.
    let help_lang = ($lang | str replace -r '^([a-z]+)-([a-z]+)$' '$1_$2' | str upcase)
    let url = $"($HELP_CONTENT_BASE)?id=($guide).($page).htm&language=($help_lang)&type=5"
    let raw = (^curl --silent --show-error --fail --max-time 30 $url)
    if ($raw | str length) == 0 {
        error make { msg: $"help endpoint returned empty body for ($guide).($page)" }
    }
    { id: $page, title: $page, content: $raw }
}

# Returns true if the guide identifier should route through the
# help.salesforce.com Visualforce backend instead of the docs API.
def is-help-guide [guide: string]: nothing -> bool {
    $HELP_GUIDE_NAMESPACES | any { |ns| $guide == $ns }
}

def main [
    url?: string,                       # full Salesforce help URL
    --guide: string,                    # alternative to URL: guide identifier (e.g. api_tooling)
    --page: string,                     # alternative to URL: page identifier (e.g. intro_rest_resources)
    --output (-o): path,                # output file path (default: ~/.cache/cloudburst-sdk/sf-docs/{page}.{format})
    --format (-f): string = "plain",    # pandoc target format (plain, markdown, gfm, ...)
    --lang: string = $DEFAULT_LANG,     # docs language code
    --version: string,                  # override docs-site version (default: discover from manifest)
    --manifest,                         # dump the guide's TOC manifest instead of an article
] {
    let parsed = if $url != null {
        parse-doc-url $url
    } else if $guide != null and $page != null {
        { guide: $guide, page: $page }
    } else if $guide != null and $manifest {
        { guide: $guide, page: null }
    } else {
        error make { msg: "must supply <URL>, or --guide and --page, or --guide and --manifest" }
    }

    if $manifest {
        if (is-help-guide $parsed.guide) {
            error make { msg: $"--manifest unsupported for help.salesforce.com guides (($parsed.guide)) — they have no JSON TOC backend" }
        }
        let mf = (fetch-manifest $parsed.guide $lang)
        let out = ($output | default $"($env.HOME)/.cache/cloudburst-sdk/sf-docs/manifest-($parsed.guide).json")
        mkdir ($out | path dirname)
        $mf | to json --indent 2 | save -f $out
        print $"✓ wrote manifest to ($out)"
        print $out
        return
    }

    let doc = if (is-help-guide $parsed.guide) {
        # help.salesforce.com guides skip version resolution — the
        # Visualforce endpoint always serves the current article.
        fetch-help-content $parsed.guide $parsed.page $lang
    } else {
        let resolved_version = if $version != null {
            $version
        } else {
            let mf = (fetch-manifest $parsed.guide $lang)
            let v = ($mf.version | get --optional doc_version)
            if $v != null { $v } else {
                $mf.available_versions | get 0.doc_version
            }
        }
        fetch-content $parsed.guide $parsed.page $lang $resolved_version
    }
    let out = ($output | default $"($env.HOME)/.cache/cloudburst-sdk/sf-docs/($parsed.page).($format)")
    mkdir ($out | path dirname)

    # The API returns content as a fragment (no <html><body>). pandoc's
    # html reader handles fragments fine, but wrapping makes pandoc emit
    # a title and lets the rendered output stand alone.
    let html_path = $"($env.HOME)/.cache/cloudburst-sdk/sf-docs/.tmp-($parsed.page).html"
    let wrapped = $"<!DOCTYPE html><html><head><title>($doc.title)</title></head><body><h1>($doc.title)</h1>($doc.content)</body></html>"
    $wrapped | save -f $html_path

    ^pandoc -f html -t $format $html_path -o $out

    rm $html_path
    let line_count = (^wc -l $out | split row ' ' | get 0 | into int)
    print $"✓ wrote ($out) \(($line_count) lines\)"
    print $out
}
