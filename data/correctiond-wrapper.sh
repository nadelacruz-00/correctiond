#!/bin/sh
# correctiond wrapper
[ $# -eq 0 ] && { echo "usage: correctiond doctor [--json] | correctiond import FILE" >&2; exit 64; }
exec /usr/lib/correctiond/engine "$@"
