#!/bin/sh
# Stops only the prepared isolated server. Evidence files are retained.
set -eu
: "${RIWORK_HOME:?Source fixture.env first}"
: "${SPIKE_TMUX:?Source fixture.env first}"
: "${SPIKE_TMUX_SOCKET:?Source fixture.env first}"
: "${SPIKE_TMUX_SESSION:?Source fixture.env first}"
test -f "$RIWORK_HOME/GHOSTTY_NSH_SPIKE_FIXTURE"
test "$SPIKE_TMUX_SOCKET" = "$RIWORK_HOME/tmux.sock"
test "$SPIKE_TMUX_SESSION" = "ghostty-spike"
# On this host kill-server returned success while leaving the fixture alive.
# Kill its exact session and verify the postcondition instead of trusting exit 0.
if "$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" has-session -t "$SPIKE_TMUX_SESSION" 2>/dev/null; then
    "$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" kill-session -t "$SPIKE_TMUX_SESSION"
fi
if "$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" has-session -t "$SPIKE_TMUX_SESSION" 2>/dev/null; then
    echo "Fixture session is still present; cleanup failed" >&2
    exit 1
fi
