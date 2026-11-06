#!/usr/bin/env bash
# Install the companion GNOME Shell extension for the current user.
# Copies extension/ (without its tests) plus LICENSE-MIT and LICENSE-APACHE to
# ~/.local/share/gnome-shell/extensions/<uuid>/. GNOME Shell on Wayland only
# picks up a new or changed extension at login, so afterwards: log out and
# back in, then `gnome-extensions enable <uuid>` (once; it stays enabled).
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
uuid=correctiond-companion@nadelacruz-00.github.io
src="$repo/extension"
dest="${XDG_DATA_HOME:-$HOME/.local/share}/gnome-shell/extensions/$uuid"

grep -q "\"uuid\": \"$uuid\"" "$src/metadata.json"

mkdir -p "$dest"
# Both license files too: metadata.json has no license field.
install -m 0644 "$src/metadata.json" "$src/extension.js" "$src/lib.js" \
    "$src/config.js" "$src/prefs.js" "$src/prefsWindow.js" "$src/wordsPage.js" \
    "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$dest/"
echo "installed $dest"

if gnome-extensions info "$uuid" >/dev/null 2>&1; then
    echo "GNOME Shell already knows $uuid: log out and back in to load the new code."
else
    echo "Log out and back in, then run: gnome-extensions enable $uuid"
fi
