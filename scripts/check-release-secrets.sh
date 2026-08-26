#!/usr/bin/env bash
set -euo pipefail

repo=${CACHEFERRET_RELEASE_REPO:-rvben/cacheferret}
required=(CARGO_REGISTRY_TOKEN HOMEBREW_TAP_DEPLOY_KEY PYPI_API_TOKEN)

if ! command -v gh >/dev/null; then
  echo "gh is required to verify release readiness" >&2
  exit 2
fi

configured=$(gh secret list --repo "$repo" --json name --jq '.[].name')
missing=()
for secret in "${required[@]}"; do
  if ! grep -Fxq "$secret" <<< "$configured"; then
    missing+=("$secret")
  fi
done

if (( ${#missing[@]} )); then
  printf 'Missing required GitHub release secrets for %s: %s\n' \
    "$repo" "${missing[*]}" >&2
  echo "Run 'clihatch secrets $repo --dry-run' before configuring them." >&2
  exit 1
fi

echo "All required GitHub release secrets are configured for $repo"
