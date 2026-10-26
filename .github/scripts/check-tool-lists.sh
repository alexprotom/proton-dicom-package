#!/usr/bin/env bash
# Every crate under crates/ that builds a tool must also be in the snap:
# in the part's rust-path and as an app. (The Windows installer needs no
# list - pdp-pack finds the tools by itself.)
set -euo pipefail
cd "$(dirname "$0")/../.."
status=0
for manifest in crates/*/Cargo.toml; do
  dir=$(dirname "$manifest")
  # [[bin]] names, or the package name when there is a src/main.rs.
  bins=$(awk '/^\[\[bin\]\]/{b=1;next} /^\[/{b=0} b && /^name *=/{gsub(/.*= *"|".*/,"");print}' "$manifest")
  if [ -z "$bins" ] && [ -f "$dir/src/main.rs" ]; then
    bins=$(awk '/^\[package\]/{p=1;next} /^\[/{p=0} p && /^name *=/{gsub(/.*= *"|".*/,"");print;exit}' "$manifest")
  fi
  [ -z "$bins" ] && continue
  if ! grep -qE "^\s*-\s*$dir\s*$" snap/snapcraft.yaml; then
    echo "::error file=snap/snapcraft.yaml::$dir is not in the rust-path of the snap"
    status=1
  fi
  for bin in $bins; do
    if ! grep -qE "^  $bin:\s*$" snap/snapcraft.yaml || ! grep -qE "command: bin/$bin\s*$" snap/snapcraft.yaml; then
      echo "::error file=snap/snapcraft.yaml::$bin has no app (command: bin/$bin) in the snap"
      status=1
    fi
    echo "tool: $bin ($dir)"
  done
done
exit $status
