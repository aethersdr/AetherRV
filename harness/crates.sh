#!/bin/bash
# Regenerate docs/CRATES.md: the licence of every third-party Rust crate the binaries link and every npm package the
# UI bundle ships (the project is GPL-3.0; every licence listed must be compatible). Workstation dev tool, needs jq:
#   harness/crates.sh
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
META=$(mktemp)
trap 'rm -f "$META"' EXIT
cargo metadata --offline --format-version 1 > "$META"
# shellcheck disable=SC2016  # jq's $vars, not the shell's
jq -r --slurpfile pkg ui/package.json --slurpfile lock ui/package-lock.json '
  def shipped: any(.dep_kinds[]; .kind == null or .kind == "build");     # what ships, not dev-only
  def permissive: ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0", "Unlicense",
                   "0BSD", "CC0-1.0", "MIT-0", "BSL-1.0", "CDLA-Permissive-2.0", "LGPL-2.1-or-later", "LLVM-exception"];
  def ok: gsub("[()]"; " ") | gsub("/"; " OR ") | [splits("\\s+") | select(. != "" and . != "OR" and . != "AND" and . != "WITH")]
          | length > 0 and all(.[]; . as $t | permissive | index([$t]) != null);
  (.workspace_members) as $ws
  | (reduce .packages[] as $p ({}; .[$p.id] = $p)) as $pk
  | (reduce .resolve.nodes[] as $n ({}; .[$n.id] = [$n.deps[] | select(shipped) | .pkg])) as $deps
  | ({seen: {}, stack: $ws}
     | until(.stack | length == 0; .stack[-1] as $i | .stack |= .[:-1]
             | if .seen[$i] then . else .seen[$i] = true | .stack += $deps[$i] end)
     | .seen | keys) as $seen
  | ([$seen[] | select(. as $i | $ws | index([$i]) | not) | $pk[.]] | sort_by([.name, .version])) as $third
  | ([$ws[] as $w | $deps[$w][] | select(. as $i | $ws | index([$i]) | not) | $pk[.].name] | unique) as $direct
  | ([$third[] | select(.license // "" | ok | not) | .name]) as $bad
  # licences by count, ties in first-seen order (as Python Counter.most_common)
  | ([$third | to_entries[] | {i: .key, l: (.value.license // "NONE")}] | group_by(.l)
     | map({l: .[0].l, n: length, first: .[0].i}) | sort_by([-.n, .first])) as $counts
  | ($pkg[0].dependencies // {} | keys) as $npm
  | "# Third-party licences",
    "",
    "AetherRV is GPL-3.0. Everything below is linked into the binaries or shipped in the UI bundle, and every",
    "licence is GPL-3.0-compatible (permissive). Regenerate with `harness/crates.sh`; dev-only tools",
    "(test frameworks, the bundler) are not listed.",
    "",
    "## Rust: \($third | length) crates",
    "",
    "| Licence | Crates |",
    "|---|---|",
    ($counts[] | "| \(.l) | \(.n) |"),
    "",
    "Not on the permissive list: \(if $bad == [] then "none" else $bad | join(", ") end).",
    "",
    "### Direct dependencies",
    "",
    "| Crate | Version | Licence |",
    "|---|---|---|",
    ($third[] | select(.name as $n | $direct | index([$n])) | "| \(.name) | \(.version) | \(.license // "None") |"),
    "",
    "## UI (npm, shipped in the bundle)",
    "",
    "| Package | Version | Licence |",
    "|---|---|---|",
    ($npm[] | . as $n | ($lock[0].packages["node_modules/" + $n] // {}) | "| \($n) | \(.version // "?") | \(.license // "?") |"),
    "",
    "## Not vendored",
    "",
    "Run-time system packages from Arch Linux ARM (Mosquitto EPL-2.0/EDL-1.0, the UI runtime, BlueZ GPL-2.0) are",
    "separate programs the system talks to, not linked or redistributed by this repository.",
    (["crates: \($third | length), npm packages: \($npm | length), not permissive: \($bad)"] | join("") | stderr | empty)
' "$META" > docs/CRATES.md.new
mv docs/CRATES.md.new docs/CRATES.md
echo
