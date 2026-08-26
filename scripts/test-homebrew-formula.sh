#!/usr/bin/env bash
set -euo pipefail

test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT

checksum=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
scripts/render-homebrew-formula.sh \
  packaging/homebrew/cacheferret.rb.in \
  "$test_dir/cacheferret.rb" \
  v0.1.0 \
  0.1.0 \
  "$checksum" \
  "$checksum" \
  "$checksum" \
  "$checksum"

ruby -c "$test_dir/cacheferret.rb"
grep -Fq 'version "0.1.0"' "$test_dir/cacheferret.rb"
grep -Fq 'cacheferret-v0.1.0-aarch64-apple-darwin.tar.gz' "$test_dir/cacheferret.rb"
grep -Fq 'on_arm do' "$test_dir/cacheferret.rb"
grep -Fq 'on_intel do' "$test_dir/cacheferret.rb"
if grep -Fq 'Hardware::CPU' "$test_dir/cacheferret.rb"; then
  echo "rendered formula bypasses Homebrew's architecture simulation" >&2
  exit 1
fi
if grep -Eq '@[A-Z0-9_]+@' "$test_dir/cacheferret.rb"; then
  echo "rendered formula contains an unresolved placeholder" >&2
  exit 1
fi
