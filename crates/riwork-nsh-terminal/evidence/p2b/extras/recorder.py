
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
