#!/bin/sh
# RIWORK_HOME is overridden only for the child test process, never the caller.
set -eu
schedule_test_home=$(mktemp -d "${TMPDIR:-/tmp}/riwork-schedule-check.XXXXXX")
trap 'rm -rf "$schedule_test_home"' EXIT
RIWORK_HOME="$schedule_test_home" RIWORK_RUNTIME_DIR="$schedule_test_home/runtime" cargo test --offline "$@" -- --test-threads=1
