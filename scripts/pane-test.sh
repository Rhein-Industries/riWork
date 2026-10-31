#!/bin/sh
# Runs the app's tests whose names hold each NAME (`nsh test apps/riwork --filter NAME`) in a
# private RiWork home (scripts/pane-env.sh), so they never change the person's own data. Some
# tests open the whole app and save settings, layouts or an API key: never run them otherwise.
#
#   scripts/pane-test.sh NAME [NAME]...
set -eu
[ $# -ge 1 ] || { echo "Usage: scripts/pane-test.sh NAME [NAME]..." >&2; exit 64; }
cd "$(dirname "$0")/.."
. scripts/pane-env.sh
log=$(mktemp)
trap 'rm -f "$log"; [ -z "$private_home" ] || rm -rf "$private_home"' EXIT INT TERM
status=0
for name in "$@"; do
	echo "== $name"
	if "$nsh" test apps/riwork --filter "$name" >"$log" 2>&1; then
		grep -v '^warn riwork/native exports' "$log" | tail -4
	else
		grep -v '^warn riwork/native exports' "$log" | tail -30
		status=1
	fi
done
exit $status
