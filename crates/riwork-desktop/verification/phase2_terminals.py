#!/usr/bin/env python3
"""Integrated local-terminal acceptance, isolated RIWORK_HOME only.

Uses a terminal-fixture feature binary plus the production TSX bundle. Fixture
commands drive real host routes, real NSH tab unmount/remount, and AppKit input
callbacks. Cua is read-only and performed separately against the printed PID.
No default tmux socket, live RiWork state, install, or app activation is used.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[3]


def write_json(path, value):
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    temporary.replace(path)


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stable_hash(data):
    value = 0xcbf29ce484222325
    for byte in data:
        value = ((value ^ byte) * 0x100000001b3) & 0xffffffffffffffff
    return value


PRESSURE = r'''
import fcntl, json, os, select, signal, struct, sys, termios, time, tty
from pathlib import Path
root, name = Path(sys.argv[1]), sys.argv[2]
root.mkdir(parents=True, exist_ok=True)
tty.setraw(0)
os.write(1, b'\x1b[?1002h\x1b[?1006h')
received = root.joinpath(name + '.input').open('ab', buffering=0)
resizes = root.joinpath(name + '.resize.jsonl').open('a', buffering=1)
def resized(*_):
    rows, cols, _, _ = struct.unpack('HHHH', fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8)))
    resizes.write(json.dumps({'time':time.monotonic(), 'rows':rows, 'cols':cols}) + '\n')
signal.signal(signal.SIGWINCH, resized)
resized()
block = (('https://example.com/ RIWORK ' + name + ' OUTPUT PRESSURE ' + '-'*48 + '\r\n')*96).encode()
written = 0
last = 0
while True:
    ready, _, _ = select.select([0], [], [], 0.01)
    if ready:
        value = os.read(0, 65536)
        if value: received.write(value)
    os.write(1, block)
    written += len(block)
    now = time.monotonic()
    if now-last >= .1:
        target = root/(name+'.counter.json')
        pending = target.with_suffix('.tmp')
        pending.write_text(json.dumps({'pid':os.getpid(), 'bytes':written, 'time':now}))
        pending.replace(target)
        last=now
'''


class Run:
    def __init__(self, args):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix='rwnsh-p2-', dir='/tmp')).resolve()
        self.home = self.root/'state'
        self.control = self.home/'terminal-fixture'
        for directory in (self.control/'requests', self.control/'responses', self.root/'user/.config', self.root/'runtime', self.root/'pressure', self.root/'tmp'):
            directory.mkdir(parents=True, exist_ok=True)
        (self.control/'marker').write_text('riwork-terminal-fixture-v1\n')
        self.binary = args.binary.resolve(strict=True)
        self.bundle = args.bundle.resolve(strict=True)
        self.tmux = Path(shutil.which('tmux') or '/opt/homebrew/bin/tmux').resolve(strict=True)
        self.python = Path(sys.executable).resolve(strict=True)
        # Engine deliberately removes TMUX_TMPDIR. RIWORK_HOME determines this
        # unique -L name; every observational tmux call below names it explicitly.
        self.socket = f'riwork-{stable_hash(str(self.home).encode()):016x}'
        self.env = dict(PATH=f'{self.tmux.parent}:/usr/bin:/bin:/usr/sbin:/sbin',
                        HOME=str(self.root/'user'), RIWORK_HOME=str(self.home),
                        RIWORK_RUNTIME_DIR=str(self.root/'runtime'), TMPDIR=str(self.root/'tmp'),
                        XDG_CONFIG_HOME=str(self.root/'user/.config'), RIWORK_NSH_BACKGROUND='1',
                        RIWORK_NSH_BUNDLE=str(self.bundle), RIWORK_NSH_TERMINAL_FIXTURE='1',
                        GHOSTTY_LOG='stderr,no-macos',
                        LANG='en_US.UTF-8', SHELL='/bin/zsh')
        if args.resources:
            self.env['GHOSTTY_RESOURCES_DIR'] = str(args.resources.resolve(strict=True))
        self.seq = 0
        self.child = None
        self.projects = []
        self.sessions = []
        self.cycles = []
        self.seen_attach_pids = set()
        self.manifest = dict(verification_environment='isolated RIWORK_HOME; integrated fixture binary and production TSX bundle; read-only Cua only',
                             root=str(self.root), home=str(self.home), socket=self.socket,
                             binary=dict(path=str(self.binary), sha256=sha256(self.binary)),
                             bundle=dict(path=str(self.bundle), sha256=sha256(self.bundle)),
                             environment=self.env, commands=[], status='preparing')
        self.save()
        print(json.dumps({'root':str(self.root), 'manifest':str(self.root/'manifest.json')}), flush=True)

    def save(self):
        write_json(self.root/'manifest.json', self.manifest)

    def cmd(self, argv, *, check=True):
        argv = [str(a) for a in argv]
        started = time.monotonic()
        result = subprocess.run(argv, env=self.env, cwd=self.root, stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=30)
        self.manifest['commands'].append(dict(argv=argv, exit_code=result.returncode,
                                              elapsed_ms=(time.monotonic()-started)*1000,
                                              stdout=result.stdout.decode(errors='replace'),
                                              stderr=result.stderr.decode(errors='replace')))
        self.save()
        if check and result.returncode:
            raise RuntimeError(f'command failed {argv}: {result.stderr.decode(errors="replace")}')
        return result

    def tmux_cmd(self, *args, check=True):
        return self.cmd([self.tmux, '-u', '-L', self.socket, '-f', '/dev/null', *args], check=check)

    def control_command(self, op, *, timeout=15, **data):
        self.seq += 1
        seq = self.seq
        request = dict(seq=seq, op=op, **data)
        write_json(self.control/'requests'/f'{seq}.json', request)
        deadline = time.monotonic()+timeout
        path = self.control/'responses'/f'{seq}.json'
        while time.monotonic() < deadline:
            if path.exists():
                response = json.loads(path.read_text())['result']
                if response.get('ok') is False:
                    raise RuntimeError(f'fixture {op} failed: {response}')
                return response
            if self.child.poll() is not None:
                if op == 'quit' and self.child.returncode == 0:
                    return {'ok':True, 'exited':True}
                raise RuntimeError(f'fixture exited {self.child.returncode} during {op}')
            time.sleep(.01)
        raise RuntimeError(f'fixture {op} timed out after {timeout}s')

    def wait(self, check, description, timeout=30):
        deadline = time.monotonic()+timeout
        last = None
        while time.monotonic()<deadline:
            if self.child and self.child.poll() is not None:
                raise RuntimeError(f'fixture exited {self.child.returncode}: {description}')
            last = check()
            if last:
                return last
            time.sleep(.05)
        raise RuntimeError(f'timed out: {description}; last={last}')

    def seed(self):
        pressure = self.root/'pressure.py'
        pressure.write_text(PRESSURE)
        write_json(self.home/'settings.json', {'tab_close_behavior':'detach'})
        config_dir = self.root/'user/.config/ghostty'
        config_dir.mkdir(parents=True)
        (config_dir/'config').write_text('font-family = Menlo\nfont-size = 11\nwindow-padding-x = 4\nwindow-padding-y = 4\nshell-integration = none\nbackground = 101820\nforeground = c9d8e6\n')
        layouts = {}
        for index, name in enumerate(('Alpha', 'Beta')):
            repo = self.root/'repos'/name
            repo.mkdir(parents=True)
            project = json.loads(self.cmd([self.binary, 'project', 'add', repo, '--name', f'Terminal fixture {name}', '--json']).stdout)
            self.projects.append(project)
            shells = []
            for local in range(2):
                tag = f'{name.lower()}-{local}'
                command = f'{self.python} -u {pressure} {self.root / "pressure"} {tag}'
                shell = json.loads(self.cmd([self.binary, 'shell', 'create', '--project', project['id'], '--command', command, '--no-parent', '--background', '--json']).stdout)
                shell['fixtureTag'] = tag
                self.sessions.append(shell)
                shells.append(shell)
            panes = {}
            for local, shell in enumerate(shells):
                pane = str(local+1)
                panes[pane] = dict(tabs=[dict(kind='shell',shell_id=shell['id'])], active_tab_key=f'shell:{shell["id"]}',
                                   shell_ids=[shell['id']], active_shell_id=shell['id'], tab_edge='top')
            layouts[project['id']] = dict(layout={'split':dict(axis='stacked',ratio=.5,first={'pane':1},second={'pane':2})},
                                           panes=panes, active_pane=1, locked_panes=[], main_pane=None,
                                           panels_initialized=True, sidebar_visible=False, window_size={'width':960.0,'height':720.0})
        write_json(self.home/'layouts.json', {'schema_version':1,'projects':layouts})
        self.manifest.update(projects=self.projects,sessions=self.sessions, initial_layouts=layouts)
        self.save()
        self.initial_identity = self.identity()
        if len(self.initial_identity)!=4:
            raise RuntimeError(f'expected4 fixture sessions, got {self.initial_identity}')
        write_json(self.root/'initial-identity.json',self.initial_identity)

    def identity(self):
        value = self.tmux_cmd('list-panes','-a','-F','#{session_name}\t#{session_id}\t#{session_created}\t#{pane_id}\t#{pane_pid}').stdout.decode()
        return sorted(line.split('\t') for line in value.splitlines() if line)

    def clients(self):
        value = self.tmux_cmd('list-clients','-F','#{client_pid}\t#{client_tty}\t#{session_name}\t#{client_width}\t#{client_height}').stdout.decode()
        return [dict(pid=int(parts[0]),tty=parts[1],shell=parts[2],cols=int(parts[3]),rows=int(parts[4]))
                for line in value.splitlines() if (parts:=line.split('\t')) and len(parts)==5]

    def process_table(self):
        result = subprocess.run(['/bin/ps','-axo','pid=,ppid=,command='],capture_output=True,text=True,check=True)
        output = {}
        for line in result.stdout.splitlines():
            values=line.strip().split(None,2)
            if len(values)>=2: output[int(values[0])]=(int(values[1]),values[2] if len(values)>2 else '')
        return output

    def descendants(self,clients):
        table = self.process_table()
        found = set()
        for client in clients:
            pid=client['pid']
            while pid and pid != self.child.pid and pid in table and pid not in found:
                found.add(pid)
                pid=table[pid][0]
            if pid != self.child.pid:
                raise RuntimeError(f'attach client{client} is not a fixture descendant')
        self.seen_attach_pids.update(found)
        return sorted(found)

    def pressure(self):
        return {shell['id']:json.loads((self.root/'pressure'/f'{shell["fixtureTag"]}.counter.json').read_text())
                for shell in self.sessions}

    def snapshot(self, include_trace=False):
        return self.control_command('snapshot',includeTrace=include_trace)

    def wait_selected(self, window_id, shell_id):
        def selected():
            snapshot=self.snapshot()
            window=next((w for w in snapshot['windows'] if w['windowId']==window_id),None)
            if not window or not window.get('layout'): return None
            layout=window['layout']
            pane=layout.get('panes',{}).get(str(layout.get('active_pane')),{})
            return snapshot if pane.get('active_tab_key')==f'shell:{shell_id}' else None
        return self.wait(selected,f'shell {shell_id} selected in window {window_id}')

    def wait_terminals(self,count):
        def ready():
            snapshot=self.snapshot()
            state=snapshot['terminalStates']
            active=[s for s in state['states'] if s.get('active') and not s.get('closing') and not s.get('attaching')]
            return snapshot if len(active)==count and state['pending']==0 else None
        return self.wait(ready,f'{count} settled terminal surfaces')

    def wait_rendered(self,shell):
        def rendered():
            self.control_command('terminalProbe',shellId=shell['id'],kind='geometry')
            records=self.snapshot(include_trace=True)['terminalStates']['trace']
            latest=next(r for r in reversed(records) if r.get('event')=='teardown' and r['data'].get('phase')=='geometry-probe')
            geometry=latest['data']
            # A live PTY and parsed text are insufficient: verify that Ghostty
            # received actual window visibility and rendered nonuniform pixels.
            if not geometry['lastShown'] or geometry['renderer'].get('differentPixels',0)<=100:
                return None
            if not geometry['resolved'] or shell['fixtureTag'] not in geometry['viewportText']:
                return None
            return dict(terminalId=latest['terminalId'],shellId=shell['id'],**geometry)
        return self.wait(rendered,f'visible glyph rendering for {shell["fixtureTag"]}',timeout=10)

    def launch(self):
        log=(self.root/'host.log').open('wb')
        self.child=subprocess.Popen([str(self.binary),self.projects[0]['root']],env=self.env,cwd=self.root,stdin=subprocess.DEVNULL,stdout=log,stderr=subprocess.STDOUT)
        log.close()
        self.manifest.update(pid=self.child.pid,status='starting')
        self.save()
        self.wait(lambda:(self.control/'responses').exists() and self.snapshot(), 'first fixture host',timeout=45)
        self.cmd([self.binary,self.projects[1]['root']])
        snapshot=self.wait(lambda:(s if len((s:=self.snapshot())['windows'])==2 else None),'two fixture windows')
        self.windows={window['projectId']:window['windowId'] for window in snapshot['windows']}
        self.wait_terminals(4)
        self.wait(lambda:len(self.clients())==4,'four attached clients')
        self.manifest['fixture_windows_shown']=self.control_command('show')
        self.manifest['rendering_initial']=[self.wait_rendered(shell) for shell in self.sessions]
        write_json(self.root/'initial-native-snapshot.json',self.snapshot(include_trace=True))
        self.manifest.update(status='ready_for_read_only_cua',windows=snapshot['windows'])
        self.save()
        print(json.dumps(dict(ready=True,pid=self.child.pid,root=str(self.root),windows=snapshot['windows'],
                              cua_policy='RiWork Cua.ai Driver MCP read-only with explicit fixture PID')),flush=True)
        # Root can inspect both real windows without GUI input; a marker lets
        # automation continue immediately after the external read-only capture.
        deadline=time.monotonic()+self.args.linger_seconds
        while time.monotonic()<deadline and not (self.root/'cua-done').exists():
            if self.child.poll() is not None: raise RuntimeError('fixture exited during Cua observation interval')
            time.sleep(.1)

    def input_resize_theme(self):
        shell=self.sessions[0]
        token='RIWORK_NATIVE_INPUT_24ab1d55'
        self.control_command('terminalInput',shellId=shell['id'],text=token)
        input_path=self.root/'pressure'/f'{shell["fixtureTag"]}.input'
        self.wait(lambda:input_path.exists() and token.encode() in input_path.read_bytes(),'native input bytes reach tmux pane')
        received=input_path.read_bytes()
        if received.count(token.encode())!=1: raise RuntimeError('native input duplicated')
        for kind in ('keys','ime','mouse'):
            self.control_command('terminalProbe',shellId=shell['id'],kind=kind)
        def probe_bytes():
            data=input_path.read_bytes()
            return data if b'\x1b[D' in data and b'\r' in data and b'\x03' in data and '日本語'.encode() in data and b'\x1b[<' in data else None
        received=self.wait(probe_bytes,'native key/IME/mouse bytes reach tmux pane')
        if received.count('日本語'.encode())!=1: raise RuntimeError('IME commit duplicated')
        before=self.clients()
        window=self.windows[self.projects[0]['id']]
        frame=next(f for f in self.manifest['fixture_windows_shown']['frames'] if f['windowId']==window)
        self.control_command('resize',windowId=window,width=max(640,frame['width']-110),height=max(400,frame['height']-90))
        self.wait(lambda:any((c['cols'],c['rows'])!=(p['cols'],p['rows']) for c in self.clients() for p in before if c['shell']==p['shell']), 'resize changes tmux grid')
        after=self.clients()
        if {c['shell']:c['pid'] for c in after}!={c['shell']:c['pid'] for c in before}: raise RuntimeError('resize recreated client')
        self.control_command('terminalProbe',shellId=shell['id'],kind='link-hover')
        def resolved_link():
            records=self.snapshot(include_trace=True)['terminalStates']['trace']
            return any(r.get('event')=='teardown' and r['data'].get('phase')=='link-resolved' and r['data'].get('spans',0)>0 for r in records)
        self.wait(resolved_link,'native hover resolves captured tmux URL')
        self.control_command('terminalProbe',shellId=shell['id'],kind='link-click')
        def opened_link():
            records=self.snapshot(include_trace=True)['terminalStates']['trace']
            return any(r.get('event')=='linkOpen' and r['data'].get('target')=='https://example.com/' for r in records)
        self.wait(opened_link,'native command-click emits validated link event')
        self.manifest['links']=dict(target='https://example.com/',capture_resolver=True,native_hover_click=True,external_open_intercepted=True)
        self.manifest['input_resize']=dict(input_hex=received.hex(),before=before,after=after,
            coverage='direct production NSTextInputClient/NSResponder callbacks; not OS input-source UI coverage')
        theme_results=[]
        for theme, background in [('gruvbox_light','#fbf1c7'),('ri_work','#090d14')]:
            write_json(self.home/'settings.json',{'tab_close_behavior':'detach','theme':theme})
            def applied():
                trace=self.snapshot(include_trace=True)['terminalStates']['trace']
                ids={record['terminalId'] for record in trace if record.get('event')=='teardown'
                     and record['data'].get('phase')=='theme-updated' and record['data'].get('background')==background}
                return ids if len(ids)==4 else None
            self.wait(applied,f'{theme} palette live update on all four surfaces',timeout=20)
            current=self.clients()
            if {c['shell']:c['pid'] for c in current}!={c['shell']:c['pid'] for c in after}: raise RuntimeError('palette update recreated clients')
            theme_results.append(dict(theme=theme,background=background,clients_preserved=True))
        self.manifest['live_theme']=theme_results
        self.save()

    def cycles_run(self):
        for cycle in range(1,self.args.cycles+1):
            before=self.clients()
            if len(before)!=4: raise RuntimeError(f'cycle{cycle}: expected4 clients')
            descendants=self.descendants(before)
            pressure_before=self.pressure()
            start=time.monotonic()
            for shell in self.sessions:
                window=self.windows[shell['project_id']]
                self.control_command('route',windowId=window,step={'kind':'showShell','shellId':shell['id']})
                self.wait_selected(window,shell['id'])
                self.control_command('command',windowId=window,id='closeTab')
            closed=self.wait_terminals(0)
            self.wait(lambda:not self.clients(),f'cycle{cycle} zero attach clients')
            table=self.process_table()
            if any(pid in table for pid in descendants):
                raise RuntimeError(f'cycle{cycle}: leaked attach descendants {[pid for pid in descendants if pid in table]}')
            if self.identity()!=self.initial_identity: raise RuntimeError(f'cycle{cycle}: session/pane identity changed')
            for shell in self.sessions:
                self.control_command('route',windowId=self.windows[shell['project_id']],step={'kind':'showShell','shellId':shell['id']})
            self.wait_terminals(4)
            self.wait(lambda:len(self.clients())==4,f'cycle{cycle} four reattached clients')
            pressure_after=self.pressure()
            if any(pressure_after[k]['bytes']<=v['bytes'] for k,v in pressure_before.items()): raise RuntimeError('output pressure stopped')
            record=dict(cycle=cycle,elapsed_ms=(time.monotonic()-start)*1000,before=before,closed_clients=[],
                        old_descendants=descendants,after=self.clients(),identity=self.initial_identity,
                        pressure_before=pressure_before,pressure_after=pressure_after)
            self.cycles.append(record)
            write_json(self.root/'cycles.json',self.cycles)
            print(json.dumps({'cycle':cycle,'elapsed_ms':record['elapsed_ms'],'clients':4}),flush=True)
        self.manifest['cycles']=self.args.cycles
        self.save()

    def shutdown(self):
        self.manifest['rendering_final']=[self.wait_rendered(shell) for shell in self.sessions]
        self.save()
        before=self.clients(); self.descendants(before)
        self.control_command('closeWindow',windowId=self.windows[self.projects[0]['id']])
        self.wait_terminals(2)
        self.wait(lambda:len(self.clients())==2,'window close leaves only other window clients')
        if self.identity()!=self.initial_identity: raise RuntimeError('window close killed a session')
        snapshot=self.snapshot()
        write_json(self.root/'before-quit.json',snapshot)
        self.control_command('quit')
        self.child.wait(timeout=15)
        if self.child.returncode: raise RuntimeError(f'graceful quit exit{self.child.returncode}')
        if self.clients(): raise RuntimeError('quit leaked clients')
        if self.identity()!=self.initial_identity: raise RuntimeError('quit killed a session')
        table=self.process_table()
        leaked=sorted(self.seen_attach_pids.intersection(table))
        if leaked: raise RuntimeError(f'quit leaked attach descendants{leaked}')
        self.manifest['graceful_shutdown']=dict(exit_code=self.child.returncode,session_survival=True,leaked_clients=0,leaked_descendants=0)
        self.verify_trace()

    def verify_trace(self):
        path=self.control/'final-snapshot.json'
        final=json.loads(path.read_text())
        write_json(self.root/'final-snapshot.json',final)
        terminals=final['terminalStates']
        trace=terminals['trace']
        released=[record['terminalId'] for record in trace if record.get('event')=='context-released']
        geometry=self.manifest['rendering_initial']+self.manifest['rendering_final']
        if len(geometry)!=8 or any(not g['resolved'] or abs(g['originX']-4)>0.01 or abs(g['originY']-4)>0.01 for g in geometry):
            raise RuntimeError(f'link grid origin must match explicit fixture padding: {geometry}')
        if any(abs(g['widthPx']-g['viewWidth']*g['scale'])>1 or abs(g['heightPx']-g['viewHeight']*g['scale'])>1 for g in geometry):
            raise RuntimeError('native backing-scale sizing mismatch')
        by_id={}
        for record in trace:
            if record.get('event')=='teardown': by_id.setdefault(record['terminalId'],[]).append(record['data'])
        expected=4*(self.args.cycles+1)
        if len(by_id)!=expected: raise RuntimeError(f'expected{expected} terminal lifetimes, got{len(by_id)}')
        if len(released)!=expected or set(released)!=set(by_id): raise RuntimeError(f'native context leak/double release: {len(released)} releases for{expected} views')
        maxima=dict(shutdown_ms=0,free_ms=0,disposal_ms=0)
        intervals=[]
        for identity,events in by_id.items():
            phases=[e['phase'] for e in events]
            for phase in ('created','shutdown-requested','child-exit-confirmed','free-begin','free-end','disposed'):
                if phases.count(phase)!=1: raise RuntimeError(f'terminal{identity}: phase{phase} count{phases.count(phase)}')
            if phases.count('detach-confirmed')<1: raise RuntimeError(f'terminal{identity}: missing exact-tty detach confirmation')
            if max(i for i,p in enumerate(phases) if p=='detach-confirmed')>=phases.index('child-exit-confirmed'): raise RuntimeError('detach confirmation must precede child-exit/free')
            required=['shutdown-requested','detach-confirmed','child-exit-confirmed','free-begin','free-end','disposed']
            if [phases.index(p) for p in required]!=sorted(phases.index(p) for p in required): raise RuntimeError(f'terminal{identity}: barrier order')
            if any(p in phases for p in ('parked','detach-retry')): raise RuntimeError(f'terminal{identity}: parked or retried; not a successful disposal')
            if any(not e['mainThread'] for e in events): raise RuntimeError('native lifecycle off main thread')
            keyed={e['phase']:e for e in events}
            maxima['shutdown_ms']=max(maxima['shutdown_ms'],keyed['shutdown-returned']['elapsedMs'])
            maxima['free_ms']=max(maxima['free_ms'],keyed['free-end']['elapsedMs'])
            maxima['disposal_ms']=max(maxima['disposal_ms'],keyed['disposed']['elapsedMs'])
            intervals.append((keyed['shutdown-requested']['monotonicMs'],keyed['disposed']['monotonicMs']))
        if maxima['shutdown_ms']>=100 or maxima['free_ms']>=100 or maxima['disposal_ms']>=2000: raise RuntimeError(f'native latency bound failed{maxima}')
        heartbeats=[r['data']['monotonicMs'] for r in trace if r.get('event')=='heartbeat']
        gaps=[dict(start=a,end=b,gap_ms=b-a,teardown_overlap=any(a<end and b>start for start,end in intervals)) for a,b in zip(heartbeats,heartbeats[1:])]
        teardown_gaps=[g for g in gaps if g['teardown_overlap']]
        if not teardown_gaps: raise RuntimeError('missing independent main-loop heartbeat during teardown')
        if max(g['gap_ms'] for g in teardown_gaps)>=100: raise RuntimeError('main-loop stalled100ms during teardown')
        if terminals['pending']!=0: raise RuntimeError('pending surfaces at process exit')
        self.manifest['native_verification']=dict(created=expected,freed=expected,contexts_released=expected,pending=0,maxima=maxima,geometry=geometry[0],rendered_surfaces=len(geometry),
            heartbeats=len(heartbeats),max_teardown_heartbeat_gap_ms=max(g['gap_ms'] for g in teardown_gaps),
            global_gaps_over100ms=[g for g in gaps if g['gap_ms']>=100])
        self.save()

    def cleanup(self):
        # Only this newly generated isolated server/session collection is named.
        if self.child and self.child.poll() is None:
            self.child.kill(); self.child.wait(timeout=5)
            self.manifest['failed_fixture_cleanup']='SIGKILL exact fixture PID; not counted as successful quit'
        pressure_pids=[]
        for shell in self.sessions:
            counter=self.root/'pressure'/f'{shell["fixtureTag"]}.counter.json'
            if counter.exists(): pressure_pids.append(json.loads(counter.read_text())['pid'])
        for shell in self.sessions:
            self.tmux_cmd('kill-session','-t',f'={shell["id"]}',check=False)
        remaining=self.tmux_cmd('list-sessions',check=False)
        deadline=time.monotonic()+5
        remaining_pids=[]
        while time.monotonic()<deadline:
            table=self.process_table()
            remaining_pids=[pid for pid in pressure_pids if pid in table]
            if not remaining_pids: break
            time.sleep(.05)
        self.manifest['cleanup']=dict(server_absent=remaining.returncode!=0,fixture_pid_absent=self.child is None or self.child.poll() is not None,
                                      pressure_pids=pressure_pids,pressure_pids_remaining=remaining_pids)
        if self.manifest['status']=='passed' and (remaining.returncode==0 or remaining_pids):
            self.manifest.update(status='failed',error='isolated fixture cleanup left a server or pressure process')
        self.save()


def main():
    # Tool shells can inherit blocked signals. Match the normal app launch
    # environment before spawning CLI sessions, PTY children, or AppKit.
    signal.pthread_sigmask(signal.SIG_SETMASK, [])
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--bundle',type=Path,required=True)
    parser.add_argument('--resources',type=Path)
    parser.add_argument('--cycles',type=int,default=20)
    parser.add_argument('--linger-seconds',type=int,default=120)
    args=parser.parse_args()
    if args.cycles<20: parser.error('acceptance requires at least20 cycles')
    run=Run(args)
    try:
        run.seed(); run.launch(); run.input_resize_theme(); run.cycles_run(); run.shutdown()
        run.manifest['status']='passed'
    except Exception as error:
        run.manifest.update(status='failed',error=str(error))
        if run.child is not None and run.child.poll() is None:
            try: write_json(run.root/'failed-snapshot.json',run.snapshot(include_trace=True))
            except Exception as snapshot_error: run.manifest['failure_snapshot_error']=str(snapshot_error)
        raise
    finally:
        run.cleanup(); run.save()
        print(json.dumps({'status':run.manifest['status'],'manifest':str(run.root/'manifest.json')}),flush=True)


if __name__=='__main__': main()
