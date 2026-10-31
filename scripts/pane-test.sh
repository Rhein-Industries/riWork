#!/bin/sh
# Runs the app's tests whose names hold each NAME (`nsh test apps/riwork --filter NAME`) in a
# private RiWork home (scripts/pane-env.sh), so they never change the person's own data. Some
# tests open the whole app and save settings, layouts or an API key: never run them otherwise.
# Each NAME gets a fresh home, seeded as phase1_integration.py seeds the suite's: the p1-layouts
# projects and layouts (fixtures.json) the named workspace fixtures open, and fake sessions for
# their made-up shells, and the invented Codex accounts of the settings tests (in an Orca profile of
# its own, never the person's). A /tmp home the caller brings is used as it is, for every NAME.
#
#   scripts/pane-test.sh NAME [NAME]...
set -eu
[ $# -ge 1 ] || { echo "Usage: scripts/pane-test.sh NAME [NAME]..." >&2; exit 64; }
cd "$(dirname "$0")/.."
. scripts/pane-env.sh
log=$(mktemp)
trap 'rm -f "$log"; [ -z "$private_home" ] || rm -rf "$private_home"' EXIT INT TERM
# Replaces the private home, RIWORK_HOME (and the repos, runtime and tmux folders beside it),
# with a newly seeded one; the generator writes only a new home.
seed() {
	rm -rf "$private_home/home" "$private_home/repos" "$private_home/damaged" "$private_home/run" "$private_home/tmux"
	mkdir "$private_home/run" "$private_home/tmux"
	if ! cargo run -q -p riwork-nsh-native --features test-support --example p1_workspace_fixture_home \
		-- --scenario p1-layouts --out "$RIWORK_HOME" >"$log" 2>&1; then
		tail -30 "$log" >&2
		echo "pane-test.sh: could not seed the private home $RIWORK_HOME" >&2
		exit 1
	fi
	# The invented Codex accounts phase6a_settings.py gives the settings tests: one saved, one
	# whose home is gone. No credentials.
	rm -rf "$private_home/orca"
	mkdir -p "$ORCA_USER_DATA_PATH/codex-accounts/fixture-saved/home"
	printf '{"version":1,"user_data":"%s","source_active_id":"fixture-saved","accounts":[%s,%s]}\n' "$ORCA_USER_DATA_PATH" \
		'{"id":"fixture-saved","email":"fixture@example.invalid","workspaceLabel":"Fixture saved","managedHomeRuntime":"host"}' \
		'{"id":"fixture-unavailable","email":"unavailable@example.invalid","workspaceLabel":"Fixture unavailable","managedHomeRuntime":"host"}' \
		>"$RIWORK_HOME/codex-accounts.json"
	chmod 600 "$RIWORK_HOME/codex-accounts.json"
}
[ -z "$private_home" ] || export RIWORK_NSH_FAKE_SESSIONS="$RIWORK_HOME/fake-sessions.json"
status=0
for name in "$@"; do
	echo "== $name"
	[ -z "$private_home" ] || seed
	if "$nsh" test apps/riwork --filter "$name" >"$log" 2>&1; then
		grep -v '^warn riwork/native exports' "$log" | tail -4
	else
		grep -v '^warn riwork/native exports' "$log" | tail -30
		status=1
	fi
done
exit $status
