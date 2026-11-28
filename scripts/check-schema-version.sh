#!/usr/bin/env bash
# Checks that every place that carries the JSON export schemaVersion agrees with
# SCHEMA_VERSION in src/export/json.rs (#366). Run from anywhere; needs only bash,
# grep, sed and cmp. Prints one line per divergent file and exits 1 if any.
#
# To bump the version, change SCHEMA_VERSION first, then fix what this reports.
set -u
cd "$(dirname "$0")/.." || exit 2

fail=0
bad() { # file, message
  echo "schema-version: $1: $2" >&2
  fail=1
}

SRC=src/export/json.rs
version=$(sed -nE 's/^pub const SCHEMA_VERSION: &str = "([0-9]+\.[0-9]+)";.*/\1/p' "$SRC")
if [ -z "$version" ]; then
  echo "schema-version: $SRC: cannot find 'pub const SCHEMA_VERSION: &str = \"X.Y\";'" >&2
  exit 2
fi
echo "SCHEMA_VERSION is $version ($SRC)"

# 1. Schema file and its frozen snapshot.
live=docs/schema/perfratio-export.schema.json
snap=docs/schema/$version/perfratio-export.schema.json
if [ ! -f "$snap" ]; then
  bad "$snap" "missing; copy $live to it (cp $live $snap)"
elif ! cmp -s "$live" "$snap"; then
  bad "$snap" "differs from $live; the snapshot of the current version must be identical"
fi
const=$(grep -m1 -A1 '"schemaVersion": {' "$live" | grep -oE '"const": *"[0-9.]+"' | grep -oE '[0-9]+\.[0-9]+')
[ "$const" = "$version" ] || bad "$live" "properties.schemaVersion.const is '${const:-?}', expected $version"
desc=$(grep -m1 -oE 'schemaVersion [0-9]+\.[0-9]+' "$live" | grep -oE '[0-9]+\.[0-9]+')
[ "$desc" = "$version" ] || bad "$live" "description says schemaVersion '${desc:-?}', expected $version"

# 2. Docs that state the current version ("currently `"X.Y"`", "current version is `"X.Y"`").
current_re='[Cc]urrent(ly)?( (`?schemaVersion`?|version))?( is)? `"?[0-9]+\.[0-9]+"?`'
for f in README.md docs/CROSS_LANGUAGE.md docs/MIGRATION.md doc/understanding-output.md; do
  found=$(grep -oE "$current_re" "$f" | grep -oE '[0-9]+\.[0-9]+')
  if [ -z "$found" ]; then
    bad "$f" "does not state the current schemaVersion, e.g. currently \`\"$version\"\`"
    continue
  fi
  for v in $found; do
    [ "$v" = "$version" ] || bad "$f" "states current schemaVersion $v, expected $version"
  done
done

# 3. Contract spec header ("schemaVersion X.Y;" on the status line).
spec=docs/specs/output-schema-contract.md
v=$(grep -m1 -oE 'schemaVersion [0-9]+\.[0-9]+;' "$spec" | grep -oE '[0-9]+\.[0-9]+')
[ "$v" = "$version" ] || bad "$spec" "status line says schemaVersion '${v:-?}', expected $version"

# 4. Tests that hard-code the current version next to "schemaVersion".
for f in tests/*.rs; do
  [ "$f" = tests/doc_versions_tests.rs ] && continue
  grep -nE 'schemaVersion.{0,16}[0-9]+\.[0-9]+' "$f" | while IFS= read -r line; do
    for v in $(printf '%s\n' "$line" | sed -E 's/^[0-9]+://' | grep -oE 'schemaVersion.{0,16}' | grep -oE '[0-9]+\.[0-9]+'); do
      [ "$v" = "$version" ] || echo "schema-version: $f:${line%%:*}: expects schemaVersion $v, expected $version" >&2
    done
  done
done > /tmp/schema-version-tests.$$ 2>&1
if [ -s /tmp/schema-version-tests.$$ ]; then
  cat /tmp/schema-version-tests.$$ >&2
  fail=1
fi
rm -f /tmp/schema-version-tests.$$

if [ "$fail" -ne 0 ]; then
  echo "schema-version: FAILED. See CONTRIBUTING.md, 'Schema version'." >&2
  exit 1
fi
echo "schema-version: OK"
