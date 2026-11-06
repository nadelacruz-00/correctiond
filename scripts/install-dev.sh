#!/usr/bin/env bash
# Install the dev build as an IBus component, then restart ibus-daemon.
# Needs sudo (writes /usr/share/ibus/component/). Afterwards select the
# engine with `ibus engine correctiond:colemak` or Super+Space.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="$repo/target/release/engine"
xml="$repo/data/correctiond.xml"
dest=/usr/share/ibus/component/correctiond.xml

if [[ ! -x "$bin" ]]; then
    echo "missing $bin: run 'cargo build --release' first" >&2
    exit 1
fi
case "$bin" in
    *'|'* | *'&'* | *'\'*)
        echo "binary path contains characters sed cannot substitute: $bin" >&2
        exit 1
        ;;
esac

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
sed "s|<exec>[^<]*</exec>|<exec>$bin</exec>|" "$xml" > "$tmp"
grep -q "<exec>$bin</exec>" "$tmp"

sudo install -m 0644 "$tmp" "$dest"
echo "installed $dest (exec: $bin)"
ibus restart
