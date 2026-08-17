#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 8 ]]; then
  echo "usage: $0 TEMPLATE OUTPUT TAG VERSION SHA_ARM_MAC SHA_INTEL_MAC SHA_ARM_LINUX SHA_INTEL_LINUX" >&2
  exit 2
fi

template=$1
output=$2
tag=$3
version=$4
sha_arm_mac=$5
sha_intel_mac=$6
sha_arm_linux=$7
sha_intel_linux=$8

if [[ ! -f "$template" ]]; then
  echo "template does not exist: $template" >&2
  exit 2
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || [[ "$tag" != "v${version}" ]]; then
  echo "tag must be vVERSION and VERSION must be semantic: tag=$tag version=$version" >&2
  exit 2
fi
for checksum in "$sha_arm_mac" "$sha_intel_mac" "$sha_arm_linux" "$sha_intel_linux"; do
  if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]]; then
    echo "invalid SHA-256 checksum: $checksum" >&2
    exit 2
  fi
done

sed \
  -e "s/@TAG@/${tag}/g" \
  -e "s/@VERSION@/${version}/g" \
  -e "s/@SHA_AARCH64_APPLE_DARWIN@/${sha_arm_mac}/g" \
  -e "s/@SHA_X86_64_APPLE_DARWIN@/${sha_intel_mac}/g" \
  -e "s/@SHA_AARCH64_UNKNOWN_LINUX_GNU@/${sha_arm_linux}/g" \
  -e "s/@SHA_X86_64_UNKNOWN_LINUX_GNU@/${sha_intel_linux}/g" \
  "$template" > "$output"

if grep -Eq '@[A-Z0-9_]+@' "$output"; then
  echo "formula contains an unresolved placeholder" >&2
  exit 1
fi
