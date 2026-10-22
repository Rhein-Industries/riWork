#!/usr/bin/env python3
"""Seed inert tool-UI history in /tmp for native visual review; never runs a provider.
Perf: --home /tmp/riwork-perf-home --items 2400 --copy-largest-real-chat
Launch: RIWORK_HOME=/tmp/riwork-test-home RIWORK_RUNTIME_DIR=/tmp/riwork-m2-runtime cargo run -- /tmp/riwork-m2-project
"""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import time
import uuid
import zlib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--home', type=Path, default=Path('/tmp/riwork-test-home'))
parser.add_argument('--root', type=Path, help='Disposable project folder (defaults beside custom --home).')
parser.add_argument('--items', type=int, help='Total completed items; use 2400 for the performance fixture.')
parser.add_argument('--dense-calls', type=int, default=0, help='Add one uninterrupted group of N reads before varied history (use 1200).')
parser.add_argument('--output-bytes', type=int, default=65536, help='Bytes per large call output in extended history.')
parser.add_argument('--image-every', type=int, default=10, help='Attach an image every N extended items.')
parser.add_argument('--image-count', type=int, default=64, help='Distinct decoded images in extended history.')
copy = parser.add_mutually_exclusive_group()
copy.add_argument('--copy-real-chat', type=Path, metavar='EVENTS_JSONL', help='Copy an inert second chat from this read-only source.')
copy.add_argument('--copy-largest-real-chat', action='store_true', help='Find and copy the largest ~/.local/share/riwork/chats/*/events.jsonl.')
args = parser.parse_args()
if args.items is not None and args.items < 13:
    parser.error('--items must be at least 13 (the original visual fixture size)')
if args.dense_calls < 0 or (args.dense_calls and (args.items is None or args.items < args.dense_calls + 15)):
    parser.error('--dense-calls requires --items of at least N + 15')
if args.output_bytes < 0 or args.image_every < 1 or args.image_count < 1:
    parser.error('--output-bytes must be nonnegative; image arguments must be positive')
home = args.home.resolve()
root = (args.root or (Path('/tmp/riwork-m2-project') if home == Path('/tmp/riwork-test-home').resolve() else home.with_name(home.name + '-project'))).resolve()
tmp = Path('/tmp').resolve()
if home == tmp or root == tmp or not home.is_relative_to(tmp) or not root.is_relative_to(tmp):
    parser.error('--home and --root must be disposable folders under /tmp')
if (home / 'state.json').exists():
    raise SystemExit(f'Fixture already exists; use the existing {home}.')
home.mkdir(parents=True, exist_ok=True)
root.mkdir(parents=True, exist_ok=True)
project, chat = str(uuid.uuid4()), str(uuid.uuid4())

def write_json(path, value):
    path.write_text(json.dumps(value, indent=2))

def png(path, phase):
    w, h = 640, 360
    pixels = bytearray()
    for y in range(h):
        pixels.append(0)
        for x in range(w):
            if 45 < x < 595 and 60 < y < 295:
                color = (235, 239, 247) if y < 95 else (250, 251, 253)
                if 70 < x < 565 and abs(y - (220 - ((x + phase) % 160) // 2)) < 4:
                    color = (53, 120, 230)
            else:
                color = (190 + y // 12, 205 + x // 24, 230)
            pixels.extend(color)
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(bytes(pixels))) + chunk(b'IEND', b''))

for n in range(1, 4):
    png(root / f'screenshot-{n}.png', n * 25)
png(root / 'chat-window.png', 70)
(root / 'model.rs').write_text('\n'.join(f'// Line {n}: transcript presentation metadata' for n in range(1, 95)))
(root / 'design-notes.md').write_text('# Tool UI\n\nNormal shows one activity line per turn.\nVerbose uses the shared engine grouping projection.\n')
(root / 'tool-ui-report.md').write_text('# UI report\n\nNative tool rows and preview navigation are ready for review.\n')
info = dict(id=chat, provider='codex', project_id=project, cwd=str(root), title='Tool UI visual check', user_title='Tool UI visual check', created_at_unix=int(time.time()), state={'state':'stopped'}, approval_mode='supervised')
write_json(home / 'state.json', dict(schema_version=1, active_project_id=project, projects=[dict(id=project, name='Tool UI Check', root=str(root), created_at=int(time.time()))], worktrees=[], tasks=[]))
layout = dict(layout={'pane':1}, panes={'1':dict(tabs=[{'kind':'chat','chat_id':chat}], active_tab_key=f'chat:{chat}')}, active_pane=1, main_pane=1, locked_panes=[], panels_initialized=True, sidebar_visible=False, window_size={'width':1180,'height':900})
write_json(home / 'layouts.json', dict(schema_version=1, projects={project:layout}))
write_json(home / 'settings.json', dict(schema_version=1, theme='native', panel_tab_icons=False, chat_display='normal', orchestrator_mode='chat'))
chatdir = home / 'chats' / chat
chatdir.mkdir(parents=True)
write_json(chatdir / 'info.json', info)
events = []
item_count = 0
image_refs = 0
def event(kind, **kw):
    events.append(dict(chat_id=chat, seq=len(events)+1, event=dict(event=kind, **kw)))

def item(id, body, semantics=None, phase=None, presentation=None, duration=1000, status='completed', turn_id='turn1'):
    global item_count, image_refs
    item_count += 1
    image_refs += len((presentation or {}).get('images', []))
    pres = presentation or {}
    if semantics: pres['semantics'] = semantics
    if phase: pres['phase'] = phase
    value=dict(id=id, turn_id=turn_id, status=status, started_at=100000, ended_at=100000+duration, body=body, presentation=pres)
    event('item_completed', item=value)
    return value

attachments=[]
images=[]
for n, name in enumerate(['screenshot-1.png','screenshot-2.png','screenshot-3.png','model.rs','design-notes.md']):
    path=root/name
    data=path.read_bytes()
    h=0xcbf29ce484222325
    for b in data: h=((h^b)*0x100000001b3)&0xffffffffffffffff
    image=name.endswith('.png')
    attachments.append(dict(id=f'att{n}', name=name, path=str(path), kind={'kind':'image','mime':'image/png'} if image else {'kind':'text'}, bytes=len(data), fingerprint=f'{h:016x}', preview={'kind':'image','path':str(path)} if image else {'kind':'text','excerpt':data[:300].decode()}))
    if image: images.append(dict(label=name, source={'kind':'local','path':str(path)}))
event('info', info=info)
event('turn_started', turn_id='turn1', started_at=100000)
item('user', dict(type='user_message', text='Make tool calls readable. Here are three screenshots and the source files.'), presentation=dict(attachments=attachments, images=images))
item('comment',dict(type='agent_message',text="I’ll look at the item model and the current row code first."), phase='commentary')
item('thought',dict(type='reasoning',text='Keep semantic metadata separate from the rendered header.'))
for n,name in enumerate(['model.rs','design-notes.md','tool-ui-report.md']):
    item(f'read{n}',dict(type='tool_call',server=None,tool='Read',input={'file_path':str(root/name)},output=(root/name).read_text()),dict(kind='read',target=name,range={'start':1,'end':40}))
item('search',dict(type='tool_call',server=None,tool='Grep',input={'pattern':'Presentation'},output='model.rs:12: Presentation'),dict(kind='search',target='Presentation in model.rs',match_count=14))
event('approval_requested', approval=dict(request_id='approval', item_id='command', kind='command', title='cargo test -p riwork-engine', choices=['accept','decline']))
event('approval_resolved', request_id='approval', decision='accept')
item('fail',dict(type='command',command='cargo clippy -p riwork-desktop',cwd=str(root),output='    Checking riwork-desktop v0.1.0\nerror: unused variable: `kind`\n  --> chat_view/cards.rs:61:9\nerror: could not compile `riwork-desktop`\n',exit_code=1),duration=21000,status='failed')
item('edit',dict(type='file_change',changes=[dict(path='model.rs',kind='modify',diff='@@ -1,2 +1,4 @@\n // Presentation\n-let title = generic_title();\n+let kind = item.presentation.semantics;\n+let title = semantic_title(kind);\n+let target = semantic_target(kind);')]),duration=2000)
item('mcp',dict(type='tool_call',server='linear',tool='create_issue',input={'title':'Relay drops presentation'},output='Created RIW-214'),dict(kind='mcp',target='linear · create_issue'))
item('command',dict(type='command',command='cargo test -p riwork-engine',cwd=str(root),output='   Compiling riwork-engine v0.1.0\n'+''.join(f'test chat::projection::case_{n} ... ok\n' for n in range(1,32))+'\ntest result: ok. 35 passed; 0 failed\n',exit_code=0),duration=12000)
item('image',dict(type='command',command='riwork-shot --window chat',cwd=str(root),output='Saved chat-window.png and tool-ui-report.md',exit_code=0),dict(kind='shell',produced=[{'path':'tool-ui-report.md'}]),presentation={'images':[dict(label='chat-window.png',source={'kind':'local','path':str(root/'chat-window.png')})]},duration=2000)
item('final',dict(type='agent_message',text='Tool rows now share a semantic header. See [model.rs:12](model.rs:12) and the [UI report](tool-ui-report.md).\n\nThe grouped reads, failure badge, command output and image preview are ready for review.'),phase='final')
event('turn_completed',turn_id='turn1',outcome={'outcome':'completed'},ended_at=172000)
# The original visual fixture is retained above. Extended history uses many turns so
# Normal grouping, Verbose rows, disclosure, images and selection all have scale.
if args.items is not None:
    for n in range(1, args.image_count + 1):
        png(root / f'perf-image-{n:03}.png', n * 7)
    output_line = 'perf fixture: call output 0123456789 abcdefghijklmnopqrstuvwxyz\n'
    output = (output_line * (args.output_bytes // len(output_line) + 1))[:args.output_bytes]
    diff_line = '+let presentation = transcript_projection_for_revision(revision);\n'
    diff = '@@ -1,2 +1,1024 @@\n' + (diff_line * (args.output_bytes // len(diff_line) + 1))[:args.output_bytes]
    if args.dense_calls:
        turn_id = 'perf-dense-turn'
        event('turn_started', turn_id=turn_id, started_at=172000)
        item('perf-dense-user', dict(type='user_message', text='Inspect this dense run of calls and screenshots.'), turn_id=turn_id)
        for n in range(args.dense_calls):
            presentation = {}
            if n % args.image_every == 0:
                name = f'perf-image-{(n // args.image_every) % args.image_count + 1:03}.png'
                presentation['images'] = [dict(label=name, source={'kind':'local', 'path':str(root/name)})]
            item(f'perf-dense-read-{n:05}', dict(type='tool_call', server=None, tool='Read', input={'file_path':str(root/'model.rs')}, output=output), dict(kind='read', target='model.rs', range={'start':1, 'end':94}), presentation=presentation, turn_id=turn_id)
        item('perf-dense-final', dict(type='agent_message', text='Dense call run complete. See [model.rs:12](model.rs:12).'), phase='final', turn_id=turn_id)
        event('turn_completed', turn_id=turn_id, outcome={'outcome':'completed'}, ended_at=272000)
    turn_id = None
    varied_start = item_count
    while item_count < args.items:
        offset = (item_count - varied_start) % 20
        n = item_count
        if turn_id is None or offset == 0:
            if turn_id is not None:
                event('turn_completed', turn_id=turn_id, outcome={'outcome':'completed'}, ended_at=172000 + n * 1000)
            turn_id = f'perf-turn-{n // 20:04}'
            event('turn_started', turn_id=turn_id, started_at=172000 + n * 1000)
        presentation = {}
        if n % args.image_every == 0:
            name = f'perf-image-{(n // args.image_every) % args.image_count + 1:03}.png'
            presentation['images'] = [dict(label=name, source={'kind':'local', 'path':str(root/name)})]
        if offset == 0:
            body = dict(type='user_message', text=f'Performance turn {n // 20}: inspect calls, output, images and preview.')
            semantics, phase = None, None
        elif offset == 19:
            body = dict(type='agent_message', text=f'Completed performance turn {n // 20}. See [model.rs:12](model.rs:12).\n\n```rust\nlet rows = projection(revision);\n```')
            semantics, phase = None, 'final'
        elif offset % 8 == 0:
            body = dict(type='file_change', changes=[dict(path='model.rs', kind='modify', diff=diff)])
            semantics, phase = None, None
        elif offset % 5 in (1, 2, 3):
            body = dict(type='tool_call', server=None, tool='Read', input={'file_path':str(root/'model.rs')}, output=output)
            semantics, phase = dict(kind='read', target='model.rs', range={'start':1, 'end':94}), None
        elif offset % 5 == 4:
            body = dict(type='tool_call', server=None, tool='Grep', input={'pattern':'presentation'}, output=output)
            semantics, phase = dict(kind='search', target='presentation in model.rs', match_count=94), None
        else:
            body = dict(type='command', command='cargo test -p riwork-engine', cwd=str(root), output=output, exit_code=0)
            semantics, phase = dict(kind='shell'), None
        item(f'perf-item-{n:05}', body, semantics, phase, presentation, turn_id=turn_id)
    if turn_id is not None:
        event('turn_completed', turn_id=turn_id, outcome={'outcome':'completed'}, ended_at=172000 + item_count * 1000)
event('usage',usage=dict(input_tokens=84000, output_tokens=4000, context_used=84000, context_window=200000))
event('state',state={'state':'stopped'})
(chatdir/'events.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
metadata = dict(home=str(home), root=str(root), project=project, chat=chat,
                items=item_count, events=len(events), image_references=image_refs, dense_calls=args.dense_calls,
                output_bytes=args.output_bytes if args.items else None,
                events_bytes=(chatdir/'events.jsonl').stat().st_size)

# A second chat retains real item payloads, but gets inert fixture metadata and
# substitutes local generated images. No provider state, account, or settings
# are copied, and the source is opened read-only. Never print chat contents.
source = args.copy_real_chat
if args.copy_largest_real_chat:
    candidates = (Path.home()/'.local/share/riwork/chats').glob('*/events.jsonl')
    source = max(candidates, key=lambda path: path.stat().st_size, default=None)
    if source is None:
        raise SystemExit('No source events.jsonl found for --copy-largest-real-chat')
if source:
    source = source.resolve()
    copied_chat = str(uuid.uuid4())
    copied_info = {**info, 'id':copied_chat, 'title':'Real long history (inert copy)', 'user_title':'Real long history (inert copy)'}
    copied_dir = home/'chats'/copied_chat
    copied_dir.mkdir()
    write_json(copied_dir/'info.json', copied_info)
    count, completed, source_image_refs, active_turn = 0, set(), 0, None
    digest = hashlib.sha256()
    with source.open('rb') as reader, (copied_dir/'events.jsonl').open('w') as writer:
        for raw in reader:
            digest.update(raw)
            if not raw.endswith(b'\n'):
                break  # A running source can have a partial final record.
            envelope = json.loads(raw)
            count += 1
            envelope.update(chat_id=copied_chat, seq=count)
            value = envelope['event']
            kind = value['event']
            if kind == 'info':
                value['info'] = copied_info
            elif kind == 'turn_started':
                active_turn = value['turn_id']
            elif kind == 'turn_completed' and value['turn_id'] == active_turn:
                active_turn = None
            elif kind in ('item_started', 'item_completed'):
                entry = value['item']
                completed.add(entry['id'])
                pres = entry.get('presentation') or {}
                for image in pres.get('images', []):
                    source_image_refs += 1
                    image['source'] = {'kind':'local', 'path':str(root/'chat-window.png')}
                for attachment in pres.get('attachments', []):
                    attachment['path'] = str(root/'chat-window.png') if attachment.get('kind', {}).get('kind') == 'image' else str(root/'design-notes.md')
                    if attachment.get('preview', {}).get('kind') == 'image':
                        attachment['preview']['path'] = str(root/'chat-window.png')
            writer.write(json.dumps(envelope) + '\n')
        if active_turn:
            count += 1
            writer.write(json.dumps(dict(chat_id=copied_chat, seq=count, event=dict(event='turn_completed', turn_id=active_turn, outcome={'outcome':'interrupted'}))) + '\n')
        count += 1
        writer.write(json.dumps(dict(chat_id=copied_chat, seq=count, event=dict(event='state', state={'state':'stopped'}))) + '\n')
    metadata['real_copy'] = dict(chat=copied_chat, source=str(source), source_bytes=source.stat().st_size,
                                 source_sha256=digest.hexdigest(), events=count, items=len(completed),
                                 image_references=source_image_refs,
                                 events_bytes=(copied_dir/'events.jsonl').stat().st_size)
write_json(home/'fixture.json', metadata)
if home == Path('/tmp/riwork-test-home').resolve():
    write_json(Path('/tmp/riwork-m2-fixture.json'), metadata)
print(json.dumps(metadata, indent=2))
