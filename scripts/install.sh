#!/bin/bash
# Installer for the correctiond portable tarball. Run as root (or set DESTDIR).
# Never calls sudo, never prompts, never runs ibus.
set -eu

usage() { echo "usage: install.sh [--uninstall] [--dry-run] [--force] [--help]" >&2; }

here=$(cd "$(dirname "$0")" && pwd)
dest=${DESTDIR:-}
uninstall=0 dry=0 force=0
for a in "$@"; do
  case $a in
    --uninstall) uninstall=1 ;;
    --dry-run) dry=1 ;;
    --force) force=1 ;;
    --help) usage; exit 0 ;;
    *) usage; exit 64 ;;
  esac
done
if [ "$(id -u)" -ne 0 ] && [ -z "$dest" ]; then
  usage
  echo "install.sh: run as root, or set DESTDIR [E_USAGE]" >&2
  exit 64
fi

engine=/usr/lib/correctiond/engine
comp=/usr/share/ibus/component/correctiond.xml
wrap=/usr/bin/correctiond
marker='# correctiond wrapper'

if [ -e "${INSTALL_SH_OSTREE_FILE:-/run/ostree-booted}" ]; then
  echo "Use \`rpm-ostree install ./correctiond-*.rpm\` on this system [E_OSTREE]" >&2
  exit 5
fi

# Owner check: package managers know only the real paths, so query the bare
# system path (not the DESTDIR-prefixed one), and only for managers on PATH.
if [ "$force" -eq 0 ]; then
  for p in "$engine" "$comp" "$wrap"; do
    pkg='' mgr='' rm_cmd=''
    if command -v dpkg >/dev/null 2>&1 && out=$(dpkg -S "$p" 2>/dev/null); then
      pkg=${out%%:*}; mgr=dpkg; rm_cmd="sudo apt remove $pkg"
    elif command -v rpm >/dev/null 2>&1 && out=$(rpm -qf "$p" 2>/dev/null); then
      pkg=${out%%
*}; mgr=rpm; rm_cmd="sudo dnf remove $pkg"
    elif command -v pacman >/dev/null 2>&1 && out=$(pacman -Qo "$p" 2>/dev/null); then
      pkg=$(printf '%s\n' "$out" | sed -n '1s/.* is owned by \([^ ]*\).*/\1/p'); mgr=pacman
      rm_cmd="sudo pacman -R $pkg"
    fi
    if [ -n "$pkg" ]; then
      echo "$p is owned by package $pkg ($mgr). Upgrade with your package manager, or remove it first: $rm_cmd. [E_PKG_OWNED]" >&2
      exit 3
    fi
  done
fi

if [ "$uninstall" -eq 1 ]; then
  for p in "$engine" "$comp" "$wrap"; do
    f=$dest$p
    [ -e "$f" ] || [ -L "$f" ] || continue
    if [ "$p" = "$wrap" ] && ! grep -qF "$marker" "$f" 2>/dev/null; then
      echo "keeping $f (not a correctiond wrapper)"
      continue
    fi
    if [ "$dry" -eq 1 ]; then echo "would remove $f"; else rm -f "$f"; echo "removed $f"; fi
  done
  [ "$dry" -eq 1 ] || echo "Now run \`ibus restart\` as your user and remove the autocorrect input source"
  exit 0
fi

written=''
put() { # src dest mode
  d=$(dirname "$2")
  mkdir -p "$d" || return 1
  t=$(mktemp -p "$d" .correctiond.XXXXXX) || return 1
  if cp "$1" "$t" && chmod "$3" "$t" && mv -f "$t" "$2"; then return 0; fi
  rm -f "$t"
  return 1
}
fail_copy() {
  echo "install.sh: could not write $1" >&2
  echo "Written so far:${written:- nothing}" >&2
  echo "Re-running install.sh is safe; \`install.sh --uninstall\` rolls back. [E_COPY]" >&2
  exit 4
}
for spec in "engine:$engine:755" "correctiond.xml:$comp:644" "correctiond-wrapper.sh:$wrap:755"; do
  src=${spec%%:*}; rest=${spec#*:}; p=${rest%%:*}; mode=${rest##*:}
  f=$dest$p
  if [ "$dry" -eq 1 ]; then echo "would install $f"; continue; fi
  put "$here/$src" "$f" "$mode" || fail_copy "$f"
  written="$written $f"
  echo "installed $f"
done
[ "$dry" -eq 1 ] || echo "Now run \`ibus restart\` as your user (not with sudo)"
