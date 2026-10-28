#!/usr/bin/env python3
"""Phase 2b acceptance in fresh /tmp RIWORK_HOME fixtures only.

The extras fixture calls production host routes and native component commands;
raw PTY records plus tmux capture-pane establish the delivered UTF-8 bytes. A
second unmodified Phase 2a fixture runs its 20-cycle detach-before-free regression.
Never connects to the default tmux socket, live windows, or user state.
"""
import argparse
import base64
import json
from pathlib import Path
import shlex
import signal
import time
import unicodedata

from phase2_terminals import Run, write_json

# A real foreground PTY program advertises bracketed paste, records every byte,
# and renders a hex digest and the original payload back into tmux scrollback.
RECORDER = r'''
import os, select, sys, tty
from pathlib import Path
path = Path(sys.argv[1])
tty.setraw(0)
os.write(1, b'\x1b[?2004hRECORDER_READY\r\n')
pending = b''
with path.open('ab', buffering=0) as record:
    while True:
        ready, _, _ = select.select([0], [], [], .1)
        if not ready: continue
        data = os.read(0, 65536)
        if not data: break
        record.write(data)
        pending += data
        while b'\x1b[201~' in pending:
            packet, pending = pending.split(b'\x1b[201~', 1)
            payload = packet.split(b'\x1b[200~', 1)[-1]
            os.write(1, b'INPUT_HEX:' + (packet+b'\x1b[201~').hex().encode() + b'\r\n')
            os.write(1, b'PASTED:' + payload.replace(b'\r', b'\n').replace(b'\n', b'\r\n') + b'\r\n')
'''
ESCAPED = "\\ ()[]{}<>\"'`!#$&;|*?\t"
PNG = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/l9sAAAAASUVORK5CYII=')


def escaped(path):
    return ''.join(('\\' if character in ESCAPED else '') + character for character in str(path))


class Extras(Run):
    def __init__(self, args):
        super().__init__(args)
        self.extra_sessions = []
        self.evidence = {}
        self.manifest['phase'] = '2b terminal extras'
        self.save()

    def seed(self):
        super().seed()
        self.recorder = self.root/'recorder.py'
        self.recorder.write_text(RECORDER)

    def route(self, step):
        return self.control_command('route', windowId=self.windows[self.projects[0]['id']], step=step)

    def select(self, shell):
        self.route({'kind':'showShell', 'shellId':shell['id']})
        self.wait_selected(self.windows[self.projects[0]['id']], shell['id'])
        self.wait(lambda: self.state(shell, active=True, visible=True), f'visible attached {shell["id"]}')

    def state(self, shell, **expected):
        for state in self.snapshot()['terminalStates']['states']:
            if state.get('alive') and state.get('target') == shell['id'] and all(state.get(key) == value for key, value in expected.items()):
                return state
        return None

    def native(self, shell, name, **args):
        return self.control_command('terminalCommand', shellId=shell['id'], name=name, args=args)

    def policy(self, shell, **args):
        return self.control_command('terminalPolicy', shellId=shell['id'], args=args)['policy']

    def capture(self, shell):
        return self.tmux_cmd('capture-pane', '-p', '-J', '-S', '-', '-t', f'={shell["id"]}:0.0').stdout

    def registry(self):
        return json.loads((self.home/'sessions.json').read_text())['sessions']

    def release_plan(self, snapshot, shell):
        window = next(state['windowNumber'] for state in snapshot['states'] if state.get('alive') and state.get('target')==shell['id'])
        return next(row['data'] for row in reversed(snapshot['trace']) if row['event']=='release-policy' and row['data']['windowNumber']==window)

    def create_recorder(self, index):
        path = self.root/f'recorder-{index}.input'
        command = shlex.join([str(self.python), '-u', str(self.recorder), str(path)])
        shell = json.loads(self.cmd([self.binary, 'shell', 'create', '--project', self.projects[0]['id'], '--command', command, '--no-parent', '--background', '--json']).stdout)
        shell['recordPath'] = path
        self.extra_sessions.append(shell)
        self.select(shell)
        self.wait(lambda: b'RECORDER_READY' in self.capture(shell), f'raw recorder {index} ready')
        return shell

    def delivered(self, shell, before, payload):
        # Preserve existing terminal paste semantics: tmux maps LF to CR.
        expected = b'\x1b[200~' + payload.replace(b'\n', b'\r') + b'\x1b[201~'
        path = shell['recordPath']
        self.wait(lambda: path.exists() and len(path.read_bytes()) >= before + len(expected), 'bracketed bytes reach raw PTY')
        actual = path.read_bytes()[before:]
        if actual != expected:
            raise RuntimeError(f'PTY bytes differ: expected {expected.hex()}, received {actual.hex()}')
        capture = self.wait(lambda: (data if (expected.hex().encode() in (data := self.capture(shell))) else None), 'tmux capture includes delivered byte digest')
        # Full byte digest above detects wrapping, escaping, separators and any
        # injected Return; this second check establishes rendered Unicode text.
        if payload.replace(b'\n', b'') not in capture.replace(b'\n', b''):
            raise RuntimeError('tmux capture does not preserve pasted UTF-8 payload')
        return dict(raw_hex=actual.hex(), capture_hex=capture.hex(), bytes=len(actual))

    def paste_drop_image_find(self):
        shell = self.create_recorder(0)
        self.input_shell = shell
        text = "PASTE_ü_日本語 spaced '$dollar'\nsecond line"
        self.native(shell, 'pasteboard', text=text)
        self.evidence['bracketed_paste'] = self.delivered(shell, 0, text.encode())
        paths = [self.root/'repos/Alpha'/'space ü日本語.txt', self.root/'repos/Alpha'/"quote ' $(literal).png"]
        for path in paths: path.write_text('fixture\n')
        offset = shell['recordPath'].stat().st_size
        self.native(shell, 'pasteboard', drop=True, paths=[str(path) for path in paths])
        # Cocoa file URLs expose macOS filesystem spelling (decomposed
        # Unicode). Assert exact bytes after that OS boundary and prove that
        # both spellings still address the original files, not sibling names.
        url_paths = [unicodedata.normalize('NFD', str(path)) for path in paths]
        payload = ' '.join(escaped(path) for path in url_paths).encode()
        self.evidence['file_drop'] = dict(paths=[str(path) for path in paths], cocoa_url_paths=url_paths, **self.delivered(shell, offset, payload))
        # Quoting must produce exactly these two literal shell arguments.
        arguments = self.cmd(['/bin/zsh', '-c', 'printf "%s\\0" ' + payload.decode()]).stdout
        if arguments != b''.join(path.encode()+b'\0' for path in url_paths):
            raise RuntimeError('dropped Unicode/metacharacter paths are not shell-quoted as literal arguments')
        for source, url_path in zip(paths, url_paths):
            original, emitted = source.stat(), Path(url_path).stat()
            if (original.st_dev, original.st_ino) != (emitted.st_dev, emitted.st_ino):
                raise RuntimeError('Cocoa file URL Unicode spelling resolved to a different file')
        self.evidence['file_drop']['same_file_identity_after_cocoa_normalization'] = True
        # Unsafe batches must not deliver the valid prefix either.
        offset = shell['recordPath'].stat().st_size
        trace_start = len(self.snapshot(True)['terminalStates']['trace'])
        try:
            self.native(shell, 'pasteboard', drop=True, paths=[str(paths[0]), '/tmp/unsafe\nname'])
        except RuntimeError as error:
            if 'native pasteboard input refused' not in str(error): raise
        self.wait(lambda: any(record['event']=='inputRefused' for record in self.snapshot(True)['terminalStates']['trace'][trace_start:]), 'unsafe file batch refused')
        if shell['recordPath'].stat().st_size != offset: raise RuntimeError('unsafe file batch delivered partial input')
        self.evidence['file_drop']['unsafe_batch_atomic'] = True
        # Finder-style file pasteboards also carry filename text. Files retain
        # priority over that caption and an accompanying image representation.
        self.native(shell, 'pasteboard', paths=[str(path) for path in paths], text='caption must not replace file paths', pngBase64=base64.b64encode(PNG).decode())
        self.evidence['file_paste'] = self.delivered(shell, offset, payload)
        offset = shell['recordPath'].stat().st_size
        caption = 'caption ü takes priority over picture'
        self.native(shell, 'pasteboard', text=caption, pngBase64=base64.b64encode(PNG).decode())
        self.evidence['captioned_image_paste'] = self.delivered(shell, offset, caption.encode())
        offset = shell['recordPath'].stat().st_size
        self.native(shell, 'pasteboard', pngBase64=base64.b64encode(PNG).decode())
        def staged():
            files = list((self.home/'tmp/terminal-images').glob('*/*.png'))
            return files[0] if len(files)==1 else None
        image = self.wait(staged, 'clipboard image staged into tab-owned file')
        self.evidence['clipboard_image'] = dict(path=str(image), sha256=__import__('hashlib').sha256(PNG).hexdigest(), **self.delivered(shell, offset, escaped(image).encode()))
        if image.read_bytes() != PNG or image.stat().st_mode & 0o777 != 0o600:
            raise RuntimeError('clipboard image contents or permissions differ')
        self.image = image
        offset = shell['recordPath'].stat().st_size
        self.native(shell, 'pasteboard', drop=True, pngBase64=base64.b64encode(PNG).decode())
        dropped = self.wait(lambda: next((path for path in (self.home/'tmp/terminal-images').glob('*/*.png') if path != image), None), 'dropped image staged into tab-owned file')
        self.evidence['image_drop'] = dict(path=str(dropped), **self.delivered(shell, offset, escaped(dropped).encode()))
        if dropped.read_bytes() != PNG: raise RuntimeError('dropped image contents differ')
        self.images = [image, dropped]
        # Use the visible workspace Find command, then the same native component
        # event emitted to its Find UI. Navigation and closing must remain local.
        trace_start = len(self.snapshot(True)['terminalStates']['trace'])
        window = self.windows[self.projects[0]['id']]
        self.control_command('command', windowId=window, id='find', args={'query':'PASTED:'})
        def result():
            rows = [r['data'] for r in self.snapshot(True)['terminalStates']['trace'][trace_start:] if r['event']=='findResult']
            return rows[-1] if rows and rows[-1].get('total', 0) >= 3 else None
        found = self.wait(result, 'Find matches captured terminal output')
        terminal_id = self.state(shell)['id']
        def find_command(command, predicate):
            start = len(self.snapshot(True)['terminalStates']['trace'])
            self.control_command('command', windowId=window, id=command)
            def updated():
                rows = [row['data'] for row in self.snapshot(True)['terminalStates']['trace'][start:] if row['event']=='findResult' and row['terminalId']==terminal_id]
                return next((row for row in reversed(rows) if predicate(row)), None)
            return self.wait(updated, f'{command} updates native Find selection')
        following = find_command('findNext', lambda row: row.get('total')==found['total'] and row.get('current',0)>0)
        previous = find_command('findPrevious', lambda row: row.get('total')==found['total'] and row.get('current',0)>0 and row['current']!=following['current'])
        closed = find_command('findClose', lambda row: row.get('total')==0 and row.get('current')==0)
        self.evidence['find'] = dict(result=found, following=following, previous=previous, closed=closed, commands=['find', 'findNext', 'findPrevious', 'findClose'])
        self.save_extras()

    def new_tab(self, step, description):
        before = {row['id'] for row in self.registry()}
        self.route(step)
        def created():
            rows = [row for row in self.registry() if row['id'] not in before]
            return rows[0] if len(rows)==1 else None
        shell = self.wait(created, description)
        self.extra_sessions.append(shell)
        self.wait_selected(self.windows[self.projects[0]['id']], shell['id'])
        self.wait(lambda: self.state(shell, active=True, visible=True), f'{description} attached')
        return shell

    def subshells(self):
        parent = self.input_shell
        child = self.new_tab({'kind':'newSubshell', 'parentShellId':parent['id']}, 'subshell created through workspace')
        nested = self.new_tab({'kind':'newSubshell', 'parentShellId':child['id']}, 'nested subshell created through workspace')
        if child.get('parent_id') != parent['id'] or nested.get('parent_id') != child['id']:
            raise RuntimeError('subshell registry did not retain explicit parent lineage')
        tokens = [(child, 'CHILD_SCROLLBACK_1246'), (nested, 'NESTED_SCROLLBACK_2738')]
        for shell, token in tokens:
            self.tmux_cmd('send-keys', '-t', f'={shell["id"]}:0.0', '-l', f"printf '{token}\\n'")
            self.tmux_cmd('send-keys', '-t', f'={shell["id"]}:0.0', 'Enter')
            self.wait(lambda: token.encode() in self.capture(shell), f'{token} in independent scrollback')
        if tokens[1][1].encode() in self.capture(child) or tokens[0][1].encode() in self.capture(nested):
            raise RuntimeError('nested shell scrollbacks leaked across sessions')
        identity = {row[0]:row for row in self.identity()}
        self.tmux_cmd('send-keys', '-t', f'={nested["id"]}:0.0', '-l', 'exit')
        self.tmux_cmd('send-keys', '-t', f'={nested["id"]}:0.0', 'Enter')
        self.wait(lambda: self.tmux_cmd('has-session','-t',f'={nested["id"]}',check=False).returncode != 0, 'nested exit ends only nested session')
        after = {row[0]:row for row in self.identity()}
        for shell in [parent, child, *self.sessions]:
            if after.get(shell['id']) != identity[shell['id']]: raise RuntimeError('nested exit changed another pane identity')
        self.evidence['subshells'] = dict(parent=parent['id'], child=child['id'], nested=nested['id'], independent_scrollback=True, nested_exit_isolated=True)
        self.ended_shell = nested
        self.wait(lambda: self.state(nested, active=True, childExited=True, ended=True), 'ended nested session retains last surface')
        self.select(parent)
        hidden = self.state(child, active=True, visible=False)
        if not hidden: raise RuntimeError('prior child must be attached and hidden for stale focus regression')
        trace_start = len(self.snapshot(True)['terminalStates']['trace'])
        self.control_command('terminalProbe', shellId=child['id'], kind='stale-focus')
        self.wait(lambda: any(record['event']=='focusChange' and record['terminalId']==hidden['id'] and record['data'].get('focused') for record in self.snapshot(True)['terminalStates']['trace'][trace_start:]), 'hidden child native focus event delivered')
        observed = []
        deadline = time.monotonic()+.25
        while time.monotonic()<deadline:
            snapshot = self.snapshot()
            window = next(window for window in snapshot['windows'] if window['windowId']==self.windows[self.projects[0]['id']])
            layout = window['layout']
            selected = layout['panes'][str(layout['active_pane'])]['active_tab_key']
            observed.append(selected)
            if selected != f'shell:{parent["id"]}': raise RuntimeError('queued hidden child focus stole active parent selection')
            time.sleep(.025)
        self.evidence['stale_hidden_focus_ignored'] = dict(hidden_shell=child['id'], selected_shell=parent['id'], observation_ms=250, readbacks=observed)
        trace_start = len(self.snapshot(True)['terminalStates']['trace'])
        self.control_command('terminalProbe', shellId=child['id'], kind='stale-drop')
        self.wait(lambda: any(record['event']=='drop' and record['terminalId']==hidden['id'] and record['data'].get('accepted') for record in self.snapshot(True)['terminalStates']['trace'][trace_start:]), 'hidden child accepted drop completion delivered')
        observed = []
        deadline = time.monotonic()+.25
        while time.monotonic()<deadline:
            snapshot = self.snapshot()
            window = next(window for window in snapshot['windows'] if window['windowId']==self.windows[self.projects[0]['id']])
            layout = window['layout']
            selected = layout['panes'][str(layout['active_pane'])]['active_tab_key']
            observed.append(selected)
            if selected != f'shell:{parent["id"]}': raise RuntimeError('queued hidden child drop completion stole active parent selection')
            time.sleep(.025)
        self.evidence['stale_hidden_drop_ignored'] = dict(hidden_shell=child['id'], selected_shell=parent['id'], observation_ms=250, readbacks=observed)
        self.save_extras()

    def vim(self):
        path = self.root/'repos/Alpha'/"Vim ü_日本語 ' $.txt"
        path.write_text('before fixture\nsecond line\n')
        shell = self.new_tab({'kind':'openEditor', 'path':str(path), 'line':2}, 'dedicated Vim editor tab')
        if shell.get('editor_path') != str(path): raise RuntimeError('Vim tab did not store canonical editor path')
        self.wait(lambda: self.tmux_cmd('display-message','-p','-t',f'={shell["id"]}:0.0','#{pane_current_command}').stdout.strip()==b'vim', 'Vim is foreground')
        # Native input callback exercises the terminal editor, and :w uses the
        # existing dedicated-Vim save semantics rather than a host file write.
        changed = 'after ü_日本語 fixture'
        self.control_command('terminalInput', shellId=shell['id'], text='\x1bgg0C'+changed+'\x1b:w\r')
        self.wait(lambda: path.read_text()==changed+'\nsecond line\n', 'Vim edit-save round trip')
        registry_before = {row['id'] for row in self.registry()}
        self.route({'kind':'openEditor', 'path':str(path), 'line':2})
        self.wait(lambda: self.tmux_cmd('display-message','-p','-t',f'={shell["id"]}:0.0','#{cursor_y}').stdout.strip()==b'1', 'reopened Vim tab navigates to requested second line')
        self.wait_selected(self.windows[self.projects[0]['id']], shell['id'])
        if {row['id'] for row in self.registry()} != registry_before:
            raise RuntimeError('reopening same Vim file created another session')
        self.evidence['vim'] = dict(shell_id=shell['id'], path=str(path), content=path.read_text(), dedicated_session=True, reopen_reused_session=True, reopen_line=2)
        self.control_command('terminalInput', shellId=shell['id'], text='\x1b:q\r')
        self.wait(lambda: self.tmux_cmd('has-session','-t',f'={shell["id"]}',check=False).returncode != 0, 'Vim exits independently')
        self.select(self.input_shell)
        self.save_extras()

    def release_policy(self):
        # Five hidden live tabs plus a visible tab exercise warm ranking, 30s
        # grace, freezing, pinning, two-per-pass and image retention on release.
        recorders = [self.input_shell] + [self.create_recorder(index) for index in range(1, 5)]
        shown = self.sessions[0]
        self.select(shown)
        for shell in recorders:
            self.wait(lambda: self.state(shell, active=True, visible=False), 'hidden native terminal retained')
        self.policy(recorders[0], frozen=True, defer=True)
        for index, shell in enumerate(recorders):
            self.policy(shell, hiddenForMs=1000*(index+1), defer=True)
        frozen = self.policy(recorders[-1], hiddenForMs=700000)
        plan = self.release_plan(frozen, recorders[0])
        if not plan['frozen'] or plan['release'] or plan['recheckMs'] != 5000:
            raise RuntimeError(f'frozen release policy differs: {plan}')
        if not all(path.exists() for path in self.images): raise RuntimeError('hidden terminal discarded its image before release')
        # Expire only the oldest nonwarm tab; the visible tab is aged as well
        # and must remain attached. Pinned oldest does not occupy a warm slot.
        self.policy(recorders[-1], pinned=True, hiddenForMs=800000, defer=True)
        self.policy(self.ended_shell, hiddenForMs=800000, defer=True)
        self.policy(shown, hiddenForMs=800000, defer=True)
        self.policy(recorders[-2], hiddenForMs=31000, defer=True)
        thawed = self.policy(recorders[0], frozen=False)
        plan = self.release_plan(thawed, recorders[0])
        expected = self.state(recorders[-2])['id']
        if plan['release'] != [expected]: raise RuntimeError(f'warm/grace/pinned policy differs: {plan}, expected {[expected]}')
        self.wait(lambda: self.state(recorders[-2], active=False), 'grace-expired hidden surface released')
        if not self.state(shown, active=True, visible=True) or not self.state(recorders[-1], active=True):
            raise RuntimeError('visible or pinned surface released')
        # Unpin and expire all hidden surfaces. First real pass may release two
        # only, and asks for the production 250ms follow-up.
        self.policy(recorders[0], frozen=True, defer=True)
        for shell in recorders:
            self.policy(shell, hiddenForMs=800000, pinned=False, defer=True)
        expired = self.policy(recorders[0], frozen=False)
        plan = self.release_plan(expired, recorders[0])
        if len(plan['release']) != 2 or plan['recheckMs'] != 250:
            raise RuntimeError(f'expired release cap/deadline differs: {plan}')
        self.wait(lambda: all(self.state(shell, active=False) for shell in recorders), 'all expired hidden surfaces released')
        if not self.state(self.ended_shell, active=True, childExited=True, ended=True):
            raise RuntimeError('ended hidden session lost its last surface')
        if not all(path.exists() for path in self.images): raise RuntimeError('surface release removed image still owned by tab')
        identity = self.identity()
        self.select(self.input_shell)
        if self.identity() != identity or not all(path.exists() for path in self.images): raise RuntimeError('reattach changed tmux identity or tab image lifecycle')
        window = self.windows[self.projects[0]['id']]
        closed_id = self.state(self.input_shell)['id']
        siblings = {client['shell']:client['pid'] for client in self.clients() if client['shell']!=self.input_shell['id']}
        self.control_command('command', windowId=window, id='closeTab')
        self.wait(lambda: not any(path.exists() for path in self.images), 'closing terminal tab cleans staged images')
        self.wait(lambda: any(row['event']=='context-released' and row['terminalId']==closed_id for row in self.snapshot(True)['terminalStates']['trace']), 'closed tab native context released while siblings remain')
        clients = self.clients()
        if any(client['shell']==self.input_shell['id'] for client in clients):
            raise RuntimeError('closing one terminal tab retained its attach client')
        if any(not any(client['shell']==shell and client['pid']==pid for client in clients) for shell,pid in siblings.items()):
            raise RuntimeError('closing one terminal tab recreated a sibling attach client')
        self.evidence['single_tab_disposal'] = dict(terminal_id=closed_id, context_released=True, closed_attach_client_absent=True, sibling_clients_preserved=siblings)
        self.evidence['release_policy'] = dict(warm_hidden=3, grace_ms=30000, warm_expiry_ms=600000, frozen_recheck_ms=5000, max_releases=2, capped_recheck_ms=250, pinned_retained=True, shown_retained=True, ended_retained=True, image_survived_release=True, image_removed_on_tab_close=True, same_tmux_identity_after_reattach=True)
        self.save_extras()

    def save_extras(self):
        self.manifest['extras'] = self.evidence
        self.save()

    def shutdown(self):
        self.control_command('quit')
        self.child.wait(timeout=15)
        if self.child.returncode or self.clients(): raise RuntimeError('extras fixture did not quit without attach clients')
        if list((self.home/'tmp/terminal-images').glob('*/*.png')):
            raise RuntimeError('successful quit left staged clipboard images')
        final=json.loads((self.control/'final-snapshot.json').read_text())
        write_json(self.root/'final-snapshot.json', final)
        trace=final['terminalStates']['trace']
        by_id={}
        for record in trace:
            if record['event']=='teardown': by_id.setdefault(record['terminalId'], []).append(record['data'])
        # A hidden surface can be released and attached again on the same
        # native view/terminalId. Validate each created surface separately.
        lifetimes = []
        maxima = dict(shutdown_ms=0, free_ms=0, disposal_ms=0)
        for terminal, events in by_id.items():
            segments = []
            for event in events:
                if event['phase']=='created': segments.append([])
                if segments: segments[-1].append(event)
            for generation, segment in enumerate(segments):
                phases = [event['phase'] for event in segment]
                required=['shutdown-requested','detach-confirmed','child-exit-confirmed','free-begin','free-end','disposed']
                once=['created','shutdown-requested','shutdown-returned','child-exit-confirmed','free-begin','free-end','disposed']
                if any(phases.count(phase)!=1 for phase in once) or any(phase not in phases for phase in required):
                    raise RuntimeError(f'extras terminal {terminal} surface {generation} lifecycle differs: {phases}')
                if [phases.index(phase) for phase in required] != sorted(phases.index(phase) for phase in required) or max(index for index,phase in enumerate(phases) if phase=='detach-confirmed')>=phases.index('child-exit-confirmed'):
                    raise RuntimeError(f'extras terminal {terminal} surface {generation} detach/free order differs: {phases}')
                if any(phase in phases for phase in ('parked','detach-retry')) or any(not event['mainThread'] for event in segment):
                    raise RuntimeError('extras native lifecycle parked/retried or left main thread')
                keyed = {event['phase']:event for event in segment}
                for key, phase in [('shutdown_ms','shutdown-returned'),('free_ms','free-end'),('disposal_ms','disposed')]:
                    maxima[key] = max(maxima[key], keyed[phase]['elapsedMs'])
                lifetimes.append(dict(terminal_id=terminal, generation=generation, phases=phases))
            if segments and sum(record['event']=='context-released' and record['terminalId']==terminal for record in trace)!=1:
                raise RuntimeError(f'extras native view {terminal} context leaked or released twice')
        if maxima['shutdown_ms']>=100 or maxima['free_ms']>=100 or maxima['disposal_ms']>=2000:
            raise RuntimeError(f'extras native latency bound failed: {maxima}')
        if final['terminalStates']['pending'] != 0: raise RuntimeError('extras quit left pending surfaces')
        write_json(self.root/'surface-lifetimes.json', lifetimes)
        self.manifest['graceful_shutdown'] = dict(exit_code=0, leaked_clients=0, native_views=len(by_id), terminal_lifetimes=len(lifetimes), detach_before_free=True, maxima=maxima)
        self.save()

    def cleanup(self):
        for shell in self.extra_sessions:
            self.tmux_cmd('kill-session', '-t', f'={shell["id"]}', check=False)
        super().cleanup()


def perform(run, steps):
    try:
        for step in steps: getattr(run, step)()
        run.manifest['status'] = 'passed'
    except Exception as error:
        run.manifest.update(status='failed', error=str(error))
        if run.child is not None and run.child.poll() is None:
            try: write_json(run.root/'failed-snapshot.json', run.snapshot(True))
            except Exception as snapshot_error: run.manifest['failure_snapshot_error'] = str(snapshot_error)
        raise
    finally:
        run.cleanup(); run.save()
        print(json.dumps({'status':run.manifest['status'], 'manifest':str(run.root/'manifest.json')}), flush=True)


def main():
    signal.pthread_sigmask(signal.SIG_SETMASK, [])
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--resources', type=Path)
    parser.add_argument('--cycles', type=int, default=20)
    parser.add_argument('--linger-seconds', type=int, default=120)
    args = parser.parse_args()
    if args.cycles < 20: parser.error('acceptance requires at least 20 disposal cycles')
    extras = Extras(args)
    perform(extras, ('seed', 'launch', 'paste_drop_image_find', 'subshells', 'vim', 'release_policy', 'shutdown'))
    disposal = Run(args)
    perform(disposal, ('seed', 'launch', 'input_resize_theme', 'cycles_run', 'shutdown'))
    print(json.dumps({'status':'passed', 'verification_environment':extras.manifest['verification_environment'], 'extras_manifest':str(extras.root/'manifest.json'), 'disposal_manifest':str(disposal.root/'manifest.json')}), flush=True)


if __name__ == '__main__': main()
