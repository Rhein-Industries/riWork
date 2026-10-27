#!/bin/sh
# Source fixture.env first. Read-only evidence from the explicit fixture socket.
set -eu
: "${RIWORK_HOME:?Source fixture.env first}"
: "${SPIKE_TMUX:?Source fixture.env first}"
: "${SPIKE_TMUX_SOCKET:?Source fixture.env first}"
: "${SPIKE_TMUX_SESSION:?Source fixture.env first}"
test -f "$RIWORK_HOME/GHOSTTY_NSH_SPIKE_FIXTURE"
test "$SPIKE_TMUX_SOCKET" = "$RIWORK_HOME/tmux.sock"
"$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" display-message -p -t "$SPIKE_TMUX_SESSION" 'session=#{session_id} created=#{session_created} pane=#{pane_id} pane_pid=#{pane_pid} clients=#{session_attached} size=#{pane_width}x#{pane_height}'
"$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" list-clients -F 'client=#{client_name} pid=#{client_pid} size=#{client_width}x#{client_height}'
"$SPIKE_TMUX" -S "$SPIKE_TMUX_SOCKET" capture-pane -p -t "$SPIKE_TMUX_SESSION"
