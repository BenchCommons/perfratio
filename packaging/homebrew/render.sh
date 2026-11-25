#!/usr/bin/env bash
# Usage: render.sh <version> <path-to-SHA256SUMS> > perfratio.rb
set -euo pipefail
version=$1 sums=$2
out=$(sed "s/@VERSION@/${version}/g" "$(dirname "$0")/perfratio.rb.template")
for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  sum=$(awk -v f="perfratio-v${version}-${t}.tar.gz" '$2==f {print $1}' "$sums")
  [ -n "$sum" ] || { echo "no checksum for $t" >&2; exit 1; }
  key=$(echo "$t" | tr 'a-z-' 'A-Z_')
  out=${out//@SHA256_${key}@/$sum}
done
printf '%s\n' "$out"
