#!/bin/sh
# Phase 1's data-refresh contract is push-only (P1-POLL-03).
set -eu
if rg -n 'setInterval\(|requestAnimationFrame\(|\.refetch\(' "$(dirname "$0")" \
  -g '*.ts' -g '*.tsx' -g '!**/*.test.ts*' -g '!**/.nsh/**' -g '!**/target/**'; then
  echo 'Unexpected TSX polling; use a native process subscription.' >&2
  exit 1
else
  status=$?
  test "$status" -eq 1
fi
