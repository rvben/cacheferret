#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux ]]; then
  echo "test-linux-e2e.sh must run on Linux" >&2
  exit 2
fi

cacheferret_bin=${CACHEFERRET_BIN:-target/debug/cacheferret}
cacheferret_bin=$(realpath "$cacheferret_bin")
if [[ ! -x "$cacheferret_bin" ]]; then
  echo "cacheferret binary is not executable: $cacheferret_bin" >&2
  exit 2
fi
if ! command -v jq >/dev/null; then
  echo "jq is required for Linux end-to-end assertions" >&2
  exit 2
fi

test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT
projects=$test_root/projects
mkdir -p "$projects"

# Four independently identified project caches.
mkdir -p "$projects/rust/target/debug"
printf '[package]\nname = "linux-fixture"\nversion = "0.1.0"\n' > "$projects/rust/Cargo.toml"
printf 'object' > "$projects/rust/target/debug/app"

mkdir -p "$projects/node/node_modules/example"
printf '{"name":"linux-fixture"}\n' > "$projects/node/package.json"
printf 'module' > "$projects/node/node_modules/example/index.js"

mkdir -p "$projects/python/.venv/bin"
printf 'home = /usr/bin\n' > "$projects/python/.venv/pyvenv.cfg"
printf 'python' > "$projects/python/.venv/bin/python"

mkdir -p "$projects/tagged"
printf 'Signature: 8a477f597d28d172789f06886806bc55\n' > "$projects/tagged/CACHEDIR.TAG"
printf 'cache' > "$projects/tagged/data"

# A cache-looking symlink must never be discovered or traversed.
mkdir -p "$test_root/outside" "$projects/symlinked"
printf '[package]\nname = "symlink-fixture"\nversion = "0.1.0"\n' > "$projects/symlinked/Cargo.toml"
printf 'keep' > "$test_root/outside/sentinel"
ln -s "$test_root/outside" "$projects/symlinked/target"

# Only the Rust cache is old enough for the default seven-day policy.
find "$projects/rust/target" -exec touch -d '10 days ago' {} +

scan_json=$($cacheferret_bin scan --root "$projects" --scope project --limit 100 --output json)
jq -e '
  .total == 4 and
  ([.items[].kind] | sort) == ["cachedir-tag", "cargo-target", "node-modules", "python-venv"] and
  ([.items[].path] | all(contains("symlinked/target") | not))
' <<< "$scan_json" >/dev/null

dry_run_json=$($cacheferret_bin clean --root "$projects" --scope project --dry-run --output json)
jq -e '
  .changed == false and
  .dry_run == true and
  .selected == 1 and
  .protected_skipped == 3 and
  .selected_targets[0].kind == "cargo-target"
' <<< "$dry_run_json" >/dev/null
test -d "$projects/rust/target"

dry_run_text=$($cacheferret_bin clean --root "$projects" --scope project --dry-run --output text)
grep -Fq $'SIZE\tRESTORE\tKIND\tPATH' <<< "$dry_run_text"
grep -Fq "$projects/rust/target" <<< "$dry_run_text"

# A piped cleanup without --yes must refuse with the declared stable error.
set +e
$cacheferret_bin clean --root "$projects" --scope project --protect-days 0 --output json \
  > "$test_root/refusal.stdout" 2> "$test_root/refusal.stderr"
refusal_code=$?
set -e
test "$refusal_code" -eq 6
jq -e '.error.kind == "confirmation_required" and .error.exit_code == 6' \
  < "$test_root/refusal.stderr" >/dev/null
test -d "$projects/rust/target"

clean_json=$($cacheferret_bin clean --root "$projects" --scope project --protect-days 0 --yes --output json)
jq -e '.changed == true and .selected == 4 and .cleaned == 4 and .skipped == 0' \
  <<< "$clean_json" >/dev/null
test ! -e "$projects/rust/target"
test ! -e "$projects/node/node_modules"
test ! -e "$projects/python/.venv"
test ! -e "$projects/tagged"
test -L "$projects/symlinked/target"
test "$(cat "$test_root/outside/sentinel")" = keep

# Linux XDG discovery and cleanup must use the configured cache root.
xdg_cache=$test_root/xdg-cache
mkdir -p "$xdg_cache/pip/wheels"
printf 'wheel' > "$xdg_cache/pip/wheels/example.whl"
global_scan=$(XDG_CACHE_HOME="$xdg_cache" \
  $cacheferret_bin scan --scope global --kind pip-cache --limit 10 --output json)
jq -e --arg path "$xdg_cache/pip" \
  '.total == 1 and .items[0].path == $path and .items[0].scope == "global"' \
  <<< "$global_scan" >/dev/null

global_dry_run=$(XDG_CACHE_HOME="$xdg_cache" \
  $cacheferret_bin clean --scope global --kind pip-cache --protect-days 0 --dry-run --output json)
jq -e --arg path "$xdg_cache/pip" \
  '.selected == 1 and .selected_targets[0].path == $path and .changed == false' \
  <<< "$global_dry_run" >/dev/null
test -d "$xdg_cache/pip"

global_clean=$(XDG_CACHE_HOME="$xdg_cache" \
  $cacheferret_bin clean --scope global --kind pip-cache --protect-days 0 --yes --output json)
jq -e '.changed == true and .cleaned == 1 and .skipped == 0' <<< "$global_clean" >/dev/null
test ! -e "$xdg_cache/pip"
test -d "$xdg_cache"

# Broad environment-provided cache roots are rejected.
mkdir -p "$test_root/empty-xdg"
broad_scan=$(XDG_CACHE_HOME="$test_root/empty-xdg" GOCACHE=/tmp \
  $cacheferret_bin scan --scope global --kind go-build-cache --limit 10 --output json)
jq -e '.total == 0 and .items == []' <<< "$broad_scan" >/dev/null

# Release-facing Linux output remains usable.
test "$($cacheferret_bin --version)" = 'cacheferret 0.1.0'
$cacheferret_bin schema | jq -e '.clispec == "0.3"' >/dev/null
test -n "$($cacheferret_bin completions bash)"

printf 'Linux end-to-end checks passed\n'
