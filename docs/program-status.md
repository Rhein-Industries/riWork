# Program status in terminals

Somtus reads explicit OSC 7501 Program Status Protocol reports from raw terminal
output. It does not infer status from prompts, screen contents, or silence.
Native chat lifecycle events and existing harness hooks/rollouts continue to work.

Programs can report without a handshake:

```sh
printf '\033]7501;state=working:app=my-agent:progress=40\033\\'
printf '\033]7501;state=blocked:kind=permission:msg=QXBwbHk/\007'
printf '\033]7501;state=done\033\\'
printf '\033]7501;state=clear\033\\'
```

The `msg` above is base64 for `Apply?`. States are `idle`, `working`, `done`,
`blocked`, and `error`. `kind` is permission, question, or auth, only for blocked.
Progress is 0–100 for working/blocked. Optional title/message text uses base64;
app names are ASCII identifiers. Reports replace the entire addressed record.
An optional `id=task/child` names a subordinate record. Clear removes that record
and descendants; clear without an id removes every record.

`ESC ] 7501 ; ? ST` receives an OSC reply listing supported states and kinds;
the reply uses the query's BEL or ST terminator. Reports may span any number of
PTY reads. Invalid state, ID, encoded text, or size limits discard the report.
Limits: 4096 sequence bytes, 2048 decoded message bytes, 192 title bytes,
32 app bytes, 128 ID bytes/eight ID segments, and 256 records including root.
Old subordinate records are evicted by last report order at the limit.

The collector uses a tmux output pipe that continues while windows are detached.
New panes install it before launching the command; attaching older panes installs
it for future output if they have no existing pipe. Somtus never replaces a user's
existing tmux output pipe. The `program-status-collect` CLI command is an internal
collector entry point, not an alternative API for inventing agent events.

Snapshots under `RIWORK_HOME/program-status/<shell UUID>.json` contain only the
bounded reports and metadata, never ordinary terminal output. They identify the
source as `osc7501`, keep the exact reported state, and distinguish observation time
from collector heartbeat time. A restarted pane gets a new generation; a retained
collector cannot publish old reports into it. Connection loss or an expired
collector lease preserves the last report as historical and marks it disconnected;
neither is a completion. Programs must clear or replace their reports when their
activity changes. A live pipe alone does not prove a program's health.

The legacy activity badges map idle/blocked/error to Waiting; the host overview
retains their distinct reported states. While the current collector is connected,
its root report takes precedence. Clear restores the existing hook/rollout source.
After collector loss, only newer hook/rollout facts may replace the disconnected
report; older completion facts cannot make the interrupted activity look done.
A subordinate protocol record is a reported task,
not proof of a real child process. Reports are untrusted facts from the program,
not authorization or evidence that a task succeeded.

Protocol reference: https://www.superlogical.com/rex/docs/build/program-status
