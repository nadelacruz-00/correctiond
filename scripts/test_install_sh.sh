#!/bin/bash
# Tests for scripts/install.sh on a temporary DESTDIR. No root needed.
set -u
repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'chmod -R u+rwx "$tmp" 2>/dev/null; rm -rf "$tmp"' EXIT
pay=$tmp/payload; mkdir -p "$pay"
printf '#!/bin/sh\nexit 0\n' >"$pay/engine"
cp "$repo/data/correctiond.xml" "$repo/data/correctiond-wrapper.sh" "$pay/"
cp "$repo/scripts/install.sh" "$pay/install.sh"
pass=0 failn=0
ok() { echo "ok   $1"; pass=$((pass+1)); }
bad() { echo "FAIL $1: $2"; failn=$((failn+1)); }
skip() { echo "skip $1 ($2)"; }

# Stub dirs: "none" reports no owner; others report one owner for one manager.
mkstubs() { # name, owning-manager-or-empty
  d=$tmp/stubs-$1; mkdir -p "$d"
  for m in dpkg rpm pacman; do printf '#!/bin/sh\nexit 1\n' >"$d/$m"; done
  case $2 in
    dpkg) printf '#!/bin/sh\necho "correctiond: $2"\n' >"$d/dpkg" ;;
    rpm) printf '#!/bin/sh\necho "correctiond-0.1.0-1.x86_64"\n' >"$d/rpm" ;;
    pacman) printf '#!/bin/sh\necho "$2 is owned by correctiond 0.1.0-1"\n' >"$d/pacman" ;;
  esac
  chmod +x "$d"/*
}
mkstubs none ''; mkstubs dpkg dpkg; mkstubs rpm rpm; mkstubs pacman pacman

# run STUBS ARGS...: sets rc, out, err. Fresh DESTDIR unless $D is preset.
run() {
  s=$1; shift
  PATH=$tmp/stubs-$s:$PATH INSTALL_SH_OSTREE_FILE=${OSTREE:-$tmp/no-such-file} \
    DESTDIR=${D:-$tmp/dest} bash "$pay/install.sh" "$@" >"$tmp/out" 2>"$tmp/err"
  rc=$?; out=$(cat "$tmp/out"); err=$(cat "$tmp/err")
}
fresh() { chmod -R u+rwx "$tmp/dest" 2>/dev/null; rm -rf "$tmp/dest"; D=; OSTREE=; }
# expect NAME RC [out|err SUBSTRING]...
expect() {
  n=$1; want=$2; shift 2
  [ "$rc" = "$want" ] || { bad "$n" "exit $rc, want $want; err: $err"; return; }
  while [ $# -ge 2 ]; do
    case $1 in out) t=$out ;; *) t=$err ;; esac
    case $t in *"$2"*) ;; *) bad "$n" "$1 lacks: $2"; return ;; esac
    shift 2
  done
  ok "$n"
}
files() { for f in usr/lib/correctiond/engine usr/share/ibus/component/correctiond.xml usr/bin/correctiond; do [ -f "$tmp/dest/$f" ] || return 1; done; }

fresh; run none
if [ $rc = 0 ] && files && [ "$(stat -c %a "$tmp/dest/usr/lib/correctiond/engine")" = 755 ] \
   && [ "$(stat -c %a "$tmp/dest/usr/share/ibus/component/correctiond.xml")" = 644 ]; then
  expect fresh_install 0 out 'Now run `ibus restart` as your user (not with sudo)'
else bad fresh_install "rc=$rc err=$err"; fi

echo old >"$tmp/dest/usr/lib/correctiond/engine"
run none
if [ $rc = 0 ] && files && cmp -s "$pay/engine" "$tmp/dest/usr/lib/correctiond/engine"; then ok rerun_is_idempotent
else bad rerun_is_idempotent "rc=$rc err=$err"; fi

fresh; run none --bogus; expect unknown_flag 64 err 'usage: install.sh'

if [ "$(id -u)" -eq 0 ]; then skip root_required_without_destdir "running as root"
else
  PATH=$tmp/stubs-none:$PATH INSTALL_SH_OSTREE_FILE=$tmp/none DESTDIR= bash "$pay/install.sh" >/dev/null 2>"$tmp/err"
  rc=$?; err=$(cat "$tmp/err"); expect root_required_without_destdir 64 err E_USAGE
fi

fresh; OSTREE=$pay/engine; run none; expect ostree 5 err '[E_OSTREE]'

for m in dpkg rpm pacman; do
  fresh; run $m; expect owned_by_$m 3 err 'is owned by package' err '[E_PKG_OWNED]'
  fresh; run $m --force; expect owned_by_${m}_force 0
done

fresh; mkdir -p "$tmp/dest/usr/share/ibus/component"; chmod 555 "$tmp/dest/usr/share/ibus/component"
if [ "$(id -u)" -eq 0 ]; then skip copy_failure "running as root"
else D=$tmp/dest; run none; expect copy_failure 4 err '[E_COPY]' err '--uninstall'; fi
chmod 755 "$tmp/dest/usr/share/ibus/component"

fresh; run none; D=$tmp/dest; run none --uninstall
if [ $rc = 0 ] && [ ! -e "$tmp/dest/usr/lib/correctiond/engine" ] && [ ! -e "$tmp/dest/usr/share/ibus/component/correctiond.xml" ] \
   && [ ! -e "$tmp/dest/usr/bin/correctiond" ]; then
  expect uninstall_removes_marked_wrapper 0 out 'Now run `ibus restart` as your user and remove the autocorrect input source'
else bad uninstall_removes_marked_wrapper "rc=$rc err=$err"; fi

fresh; run none; D=$tmp/dest; echo '#!/bin/sh' >"$tmp/dest/usr/bin/correctiond"; run none --uninstall
if [ $rc = 0 ] && [ -f "$tmp/dest/usr/bin/correctiond" ] && [ ! -e "$tmp/dest/usr/lib/correctiond/engine" ]; then ok uninstall_keeps_foreign_wrapper
else bad uninstall_keeps_foreign_wrapper "rc=$rc"; fi

fresh; run dpkg --uninstall; expect uninstall_owner_check 3 err '[E_PKG_OWNED]'

fresh; run none --dry-run
n=$(printf '%s\n' "$out" | grep -c '^would install ')
if [ $rc = 0 ] && [ "$n" = 3 ] && [ ! -e "$tmp/dest" ]; then ok dry_run_writes_nothing
else bad dry_run_writes_nothing "rc=$rc lines=$n dest_exists=$([ -e "$tmp/dest" ] && echo y || echo n)"; fi

# install-dev.sh refuses when a package owns the component; sudo must never run.
fakebin=$tmp/fake-engine; printf '#!/bin/sh\nexit 0\n' >"$fakebin"; chmod +x "$fakebin"
guard() { # manager-stub-dir
  rm -f "$tmp/sudo-called"
  d=$tmp/gstubs-$1; mkdir -p "$d"; cp "$tmp/stubs-$1"/* "$d/"
  printf '#!/bin/sh\ntouch "%s"\nexit 1\n' "$tmp/sudo-called" >"$d/sudo"
  printf '#!/bin/sh\ntouch "%s"\nexit 1\n' "$tmp/sudo-called" >"$d/cargo"
  chmod +x "$d"/*
  PATH=$d:$PATH INSTALL_DEV_BIN=$fakebin bash "$repo/scripts/install-dev.sh" >"$tmp/out" 2>"$tmp/err"
  rc=$?; out=$(cat "$tmp/out"); err=$(cat "$tmp/err")
}
for m in dpkg rpm; do
  guard $m
  if [ -e "$tmp/sudo-called" ]; then bad install_dev_guard_$m "sudo or cargo was called"
  else expect install_dev_guard_$m 1 err 'component path is owned by package correctiond' err 'sudo apt remove correctiond' err 'IBUS_COMPONENT_PATH'; fi
done

echo "$pass passed, $failn failed"
[ "$failn" -eq 0 ]
