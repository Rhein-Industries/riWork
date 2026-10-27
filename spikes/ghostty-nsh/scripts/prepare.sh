#!/bin/sh
# Throwaway fixture: this script never connects to the default tmux socket.
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SPIKE_PYTHON=$(command -v python3)
SPIKE_TMUX=$(command -v tmux)
RIWORK_HOME=$(mktemp -d "${TMPDIR:-/tmp}/riwork-ghostty-spike.XXXXXX")
export RIWORK_HOME SPIKE_PYTHON SPIKE_TMUX SCRIPT_DIR
chmod 700 "$RIWORK_HOME"
"$SPIKE_PYTHON" - <<'PY'
import json, os, pathlib, shlex, subprocess

home = pathlib.Path(os.environ['RIWORK_HOME']).resolve()
python = os.environ['SPIKE_PYTHON']
tmux = os.environ['SPIKE_TMUX']
scripts = pathlib.Path(os.environ['SCRIPT_DIR'])
socket = str(home / 'tmux.sock')
session = 'ghostty-spike'
(home / 'GHOSTTY_NSH_SPIKE_FIXTURE').write_text('isolated fixture only\n')
base = [tmux, '-S', socket, '-f', '/dev/null']
recorder = shlex.join([python, '-u', str(scripts / 'input_recorder.py'), str(home)])
env = os.environ.copy()
env.pop('TMUX', None)
env.pop('TMUX_PANE', None)
subprocess.run(base + ['new-session', '-d', '-x', '96', '-y', '26', '-c', str(home), '-s', session, recorder], env=env, check=True)
subprocess.run(base + ['set-option', '-g', 'status', 'off'], env=env, check=True)
subprocess.run(base + ['set-option', '-g', 'mouse', 'on'], env=env, check=True)
subprocess.run(base + ['set-option', '-g', 'allow-rename', 'off'], env=env, check=True)
subprocess.run(base + ['set-option', '-sg', 'escape-time', '0'], env=env, check=True)
# The pinned macOS embedding runtime adds its own `exec -l` shell wrapper.
# An additional `exec` token here becomes a bogus executable name.
command = shlex.join([tmux, '-S', socket, 'attach-session', '-t', session])
dark = home / 'ghostty-dark.conf'
light = home / 'ghostty-light.conf'
for path, background, foreground, selection in [
    (dark, '#101820', '#dce6ee', '#35516a'),
    (light, '#f8f5ec', '#182837', '#c5d9ea'),
]:
    path.write_text(f'''# Throwaway fixture config; generated under isolated RIWORK_HOME.
font-family = Menlo
font-size = 15
background = {background}
foreground = {foreground}
selection-background = {selection}
shell-integration = none
''')
values = {
    'RIWORK_HOME': str(home),
    'SPIKE_TMUX': tmux,
    'SPIKE_TMUX_SOCKET': socket,
    'SPIKE_TMUX_SESSION': session,
    'SPIKE_COMMAND': command,
    'SPIKE_CONFIG': str(dark),
    'SPIKE_LIGHT_CONFIG': str(light),
    'SPIKE_INPUT_LOG': str(home / 'input-events.jsonl'),
}
env_path = home / 'fixture.env'
env_path.write_text(''.join(f'export {key}={shlex.quote(value)}\n' for key, value in values.items()))
identity = subprocess.check_output(base + ['display-message', '-p', '-t', session, '#{session_id} #{session_created} #{pane_id} #{pane_pid}'], env=env, text=True).strip()
(home / 'session-initial.txt').write_text(identity + '\n')
(home / 'verification_environment.json').write_text(json.dumps({
    'kind': 'isolated fixture app only',
    'RIWORK_HOME': str(home),
    'tmux_socket': socket,
    'tmux_session': session,
    'initial_session_identity': identity,
    'default_tmux_socket_used': False,
    'live_riwork_windows_used': False,
}, indent=2) + '\n')
print(f'Prepared isolated fixture: {home}')
print(f'Initial identity: {identity}')
print(f'Source: . {shlex.quote(str(env_path))}')
print(env_path)
PY
