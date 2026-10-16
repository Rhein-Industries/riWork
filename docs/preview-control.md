# Development previews from coding shells

An agent can start a preview tab in an already running RiWork project window and
continue directly to testing. These commands use RiWork's local preview control
mailbox. They do not launch another GUI or activate an OS window.

```sh
# Build, install, launch, and stream on a simulator owned by this preview.
riwork preview open --ios --scheme PhoneApp --wait --json

# Select a workspace, device type, and runtime when discovery is ambiguous.
riwork preview open --ios --scheme PhoneApp \
  --xcode-project PhoneApp.xcworkspace --device-type 'iPhone 17' \
  --runtime 'iOS 26.0' --wait --timeout-seconds 300 --json

# Start a cooperative, windowless GPUI development app.
riwork preview open --command 'cargo run -p my-app --features preview' --wait --json
```

`--project ID` selects a RiWork project. `--xcode-project PATH` selects an Xcode
project or workspace relative to `--cwd`. Without `--project`, RiWork uses the
calling chat or shell's recorded project, then the closest registered worktree or
project containing the working folder. It does not change the global active
project. A project must already be open in an updated RiWork GUI.

Opening an identical command/configuration in the same project reuses its tab.
The default selects the preview tab; `--background` preserves tab selection.
`--wait` succeeds only after an actual rendered frame, with a default timeout of
120 seconds. Its JSON includes `preview_id`, `reused`, and `status`. A readiness
timeout leaves the preview open so the agent can inspect its build logs. Layout
restoration always leaves a preview stopped; running a command is explicit.

```sh
riwork preview status PREVIEW_UUID --json
riwork preview tap PREVIEW_UUID 120 300 --json
riwork preview text PREVIEW_UUID --text 'Hello from the coding agent' --json
riwork preview key PREVIEW_UUID --key enter --json
riwork preview screenshot PREVIEW_UUID --output /tmp/phone-preview.png --json
riwork preview close PREVIEW_UUID --json
```

Input coordinates use logical pixels in the original preview frame, before the
UI scales it to fit. `input` accepts one typed protocol event as JSON, including
pointer down/move/up for drags and long presses:

```sh
riwork preview input PREVIEW_UUID --input-json \
  '{"kind":"pointer_down","x":120,"y":300,"button":"left"}' --json
riwork preview input PREVIEW_UUID --input-json \
  '{"kind":"pointer_move","x":120,"y":180}' --json
riwork preview input PREVIEW_UUID --input-json \
  '{"kind":"pointer_up","x":120,"y":180,"button":"left"}' --json
```

An input response confirms queuing. Inspect a fresh screenshot to verify the
application's resulting state. Screenshots wake a background preview briefly and
wait for a new frame sequence instead of returning its previous cached image.
Status includes the current phase, readiness, dimensions, simulator identity when
available, and bounded build/application logs. `preview list --json` lists known
previews. Errors use `{"error":{"code","message"}}` with exit status 2; a missing
or stale GUI owner reports `host_unavailable`.

The same operations are available from `riwork mcp` as
`riwork_preview_open`, `riwork_preview_list`, `riwork_preview_status`,
`riwork_preview_input`, `riwork_preview_screenshot`, and `riwork_preview_close`.
The input tool accepts the event under its `input` property; the screenshot tool
takes an explicit local `output` path. The lifecycle only affects the app process
and simulator owned by that preview.
