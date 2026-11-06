#!/usr/bin/env bash
# Build the companion extension's zip (pack only; installs nothing). The zip
# carries both license files, since metadata.json has no license field.
# A plain `zip -j` of these files: the extension has no schemas or
# translations, so this is what `gnome-extensions pack` would produce,
# without needing gnome-shell installed.
#
# Usage: bash scripts/pack-extension.sh [OUT_DIR]   (default: target/extension)
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="${1:-$repo/target/extension}"
uuid=correctiond-companion@nadelacruz-00.github.io
files=(metadata.json extension.js lib.js config.js prefs.js prefsWindow.js wordsPage.js LICENSE-MIT LICENSE-APACHE)

grep -q "\"uuid\": \"$uuid\"" "$repo/extension/metadata.json"
mkdir -p "$out"
zip="$(cd "$out" && pwd)/$uuid.shell-extension.zip"
rm -f "$zip" # zip adds to an existing archive
zip -q -j -X "$zip" \
    "$repo/extension/metadata.json" \
    "$repo/extension/extension.js" \
    "$repo/extension/lib.js" \
    "$repo/extension/config.js" \
    "$repo/extension/prefs.js" \
    "$repo/extension/prefsWindow.js" \
    "$repo/extension/wordsPage.js" \
    "$repo/LICENSE-MIT" \
    "$repo/LICENSE-APACHE"
listing="$(unzip -Z1 "$zip")"
for f in "${files[@]}"; do
    grep -qx "$f" <<<"$listing" || { echo "missing $f in $zip" >&2; exit 1; }
done
if [[ "$(wc -l <<<"$listing")" != "${#files[@]}" ]]; then
    echo "unexpected files in $zip:" >&2; echo "$listing" >&2; exit 1
fi
echo "packed $zip"
