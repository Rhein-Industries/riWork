#!/usr/bin/env python3
"""Serve only a disposable tool-ui-fixture home; never starts a provider.

Start before the debug app:
  python3 scripts/tool-ui-perf-host.py --home /tmp/riwork-perf-home
  RIWORK_HOME=/tmp/riwork-perf-home ZED_MEASUREMENTS=1 target/debug/riwork /tmp/riwork-perf-project
Then stream (or send any message in the fixture app):
  python3 scripts/tool-ui-perf-host.py --home /tmp/riwork-perf-home --trigger --tokens 600 --rate 30

The fixture host implements RiWork's List/Subscribe/Command wire protocol so token
updates take the normal desktop subscription path. Streaming changes stay in
memory; restarting this server restores the identical fixture. Ctrl-C stops only
this foreground fixture host. Main-thread timings belong to the debug app.
"""
import argparse
import json
from pathlib import Path
import queue
import socket
import socketserver
import threading
import time
import uuid


def encoded(value):
    return (json.dumps(value, separators=(',', ':')) + '\n').encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--home', type=Path, default=Path('/tmp/riwork-perf-home'))
    client = parser.add_mutually_exclusive_group()
    client.add_argument('--trigger', action='store_true', help='Request a stream from an already running fixture host.')
    client.add_argument('--shutdown', action='store_true', help='Stop only this fixture socket host cleanly.')
    parser.add_argument('--chat', help='Chat ID; defaults to the synthetic fixture chat.')
    parser.add_argument('--tokens', type=int, default=600)
    parser.add_argument('--rate', type=float, default=30, help='Delta events per second.')
    parser.add_argument('--kind', choices=('text', 'output'), default='text')
    args = parser.parse_args()
    home = args.home.resolve()
    if not home.is_relative_to(Path('/tmp').resolve()) or not (home/'fixture.json').is_file():
        parser.error('--home must be a generated fixture under /tmp')
    if args.tokens < 1 or not 0 < args.rate <= 1000:
        parser.error('--tokens must be positive and --rate must be in (0, 1000]')
    metadata = json.loads((home/'fixture.json').read_text())
    chat_id = args.chat or metadata['chat']
    path = home/'run/chat.sock'
    if args.trigger or args.shutdown:
        with socket.socket(socket.AF_UNIX) as connection:
            connection.connect(str(path))
            command = dict(command='send', text=f'perf-stream:{args.tokens}:{args.rate}:{args.kind}')
            request = dict(op='fixture_shutdown', id='perf-shutdown') if args.shutdown else dict(op='command', id='perf-trigger', chat_id=chat_id, command=command)
            connection.sendall(encoded(request))
            response = json.loads(connection.makefile('rb').readline())
            if not response.get('ok'):
                raise SystemExit(response.get('error', 'fixture host refused'))
            print(json.dumps({'phase':'host_stopping'} if args.shutdown else dict(chat=chat_id, tokens=args.tokens, delta_events_per_second=args.rate,
                                  kind=args.kind, duration_seconds=args.tokens/args.rate)))
        return

    lock = threading.RLock()
    chats = {}
    for directory in (home/'chats').iterdir():
        if (directory/'info.json').is_file() and (directory/'events.jsonl').is_file():
            # Keep serialized lines to avoid decoding/retaining large output bodies in
            # the feed process. Each subscriber receives the same source records.
            chats[directory.name] = dict(info=json.loads((directory/'info.json').read_text()),
                                        lines=(directory/'events.jsonl').read_bytes().splitlines(keepends=True),
                                        subscribers=[], streaming=False)

    def emit(chat, event):
        with lock:
            if event['event'] == 'state':
                chat['info']['state'] = event['state']
            frame = encoded(dict(chat_id=chat['info']['id'], seq=len(chat['lines'])+1, event=event))
            chat['lines'].append(frame)
            for subscriber in chat['subscribers']:
                subscriber.put(frame)

    def stream(chat, tokens, rate, kind):
        started = int(time.time()*1000)
        turn = f'perf-live-{uuid.uuid4().hex[:8]}'
        owner = turn + '-item'
        body = dict(type='agent_message', text='') if kind == 'text' else dict(
            type='command', command='perf fixture token stream', cwd=metadata['root'], output='', exit_code=None)
        item = dict(id=owner, turn_id=turn, status='in_progress', started_at=started,
                    body=body, presentation={'phase':'final'} if kind == 'text' else {})
        try:
            emit(chat, dict(event='state', state={'state':'running'}))
            emit(chat, dict(event='turn_started', turn_id=turn, started_at=started))
            emit(chat, dict(event='item_completed', item=dict(id=turn+'-user', turn_id=turn,
                 status='completed', body={'type':'user_message', 'text':'Token-rate performance stream'}, presentation={})))
            emit(chat, dict(event='item_started', item=item))
            text = ''
            deadline = time.monotonic()
            for index in range(tokens):
                chunk = 'token ' if kind == 'text' else f'output {index:05}: fixture payload\n'
                text += chunk
                emit(chat, dict(event='item_delta', item_id=owner, delta={'kind':kind, 'text':chunk}))
                deadline += 1/rate
                time.sleep(max(0, deadline-time.monotonic()))
            body['text' if kind == 'text' else 'output'] = text
            if kind == 'output':
                body['exit_code'] = 0
            item.update(status='completed', ended_at=int(time.time()*1000))
            emit(chat, dict(event='item_completed', item=item))
            emit(chat, dict(event='turn_completed', turn_id=turn, outcome={'outcome':'completed'}, ended_at=int(time.time()*1000)))
            emit(chat, dict(event='state', state={'state':'stopped'}))
        finally:
            with lock:
                chat['streaming'] = False
            print(json.dumps(dict(phase='stream_complete', chat=chat['info']['id'], tokens=tokens,
                                  kind=kind, producer_interval_ms=(time.time()*1000-started)/tokens)), flush=True)

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            subscriber = None
            chat = None
            try:
                for raw in self.rfile:
                    request = json.loads(raw)
                    operation = request.get('op')
                    response = dict(id=request.get('id', ''), ok=True, result=None)
                    if operation == 'fixture_status':
                        response['result'] = {'fixture': True, 'home': str(home)}
                    elif operation == 'list':
                        response['result'] = [entry['info'] for entry in chats.values()]
                    elif operation == 'capabilities':
                        response['result'] = {'identified_create':False}
                    elif operation == 'subscribe':
                        chat = chats.get(request.get('chat_id'))
                        if chat is None:
                            response.update(ok=False, error='Unknown fixture chat')
                        else:
                            subscriber = queue.Queue()
                            with lock:
                                snapshot = list(chat['lines'][request.get('since', 0):])
                                chat['subscribers'].append(subscriber)
                            self.wfile.write(encoded(response))
                            for line in snapshot:
                                self.wfile.write(line)
                            self.wfile.flush()
                            print(json.dumps(dict(phase='subscribed', chat=chat['info']['id'], replay_events=len(snapshot))), flush=True)
                            while True:
                                self.wfile.write(subscriber.get())
                                self.wfile.flush()
                    elif operation == 'fixture_shutdown':
                        threading.Thread(target=self.server.shutdown, daemon=True).start()
                    elif operation == 'command':
                        chat = chats.get(request.get('chat_id'))
                        command = request.get('command', {})
                        if chat is None or command.get('command') != 'send':
                            response.update(ok=False, error='Fixture supports Send only')
                        else:
                            tokens, rate, kind = args.tokens, args.rate, args.kind
                            text = command.get('text', '')
                            if text.startswith('perf-stream:'):
                                _, tokens, rate, kind = text.split(':')
                                tokens, rate = int(tokens), float(rate)
                            if tokens < 1 or not 0 < rate <= 1000 or kind not in ('text', 'output'):
                                response.update(ok=False, error='Invalid stream arguments')
                            else:
                                with lock:
                                    if chat['streaming']:
                                        response.update(ok=False, error='Fixture stream already running')
                                    else:
                                        chat['streaming'] = True
                                        threading.Thread(target=stream, args=(chat, tokens, rate, kind), daemon=True).start()
                    else:
                        response.update(ok=False, error='Unsupported operation in inert fixture host')
                    self.wfile.write(encoded(response))
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            finally:
                if subscriber is not None:
                    with lock:
                        chat['subscribers'].remove(subscriber)

    class Server(socketserver.ThreadingUnixStreamServer):
        daemon_threads = True

    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    # Refuse an existing socket, including any real debug host. Never unlink a
    # running process's socket; use a fresh fixture or stop its owning process.
    if path.exists():
        raise SystemExit(f'{path} already exists; stop its fixture host and remove the stale socket first.')
    try:
        with Server(str(path), Handler) as server:
            path.chmod(0o600)
            print(json.dumps(dict(phase='ready', socket=str(path), chats=len(chats), chat=chat_id)), flush=True)
            server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        path.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
