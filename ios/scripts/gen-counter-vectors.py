#!/usr/bin/env python3
"""Independent RFC 8439 ChaCha20-Poly1305 + riwork/v1 frame construction (from docs/remote-protocol.md).
Validates against the Rust-generated fixture (counter 0, both directions), then emits counter >= 1 vectors.
Usage: python3 ios/scripts/gen-counter-vectors.py ios/Tests/Fixtures/v1.json > ios/Tests/Fixtures/counter-vectors.json"""
import json, struct, base64, sys

def rotl(v, n): return ((v << n) & 0xffffffff) | (v >> (32 - n))
def qr(s, a, b, c, d):
    s[a] = (s[a] + s[b]) & 0xffffffff; s[d] = rotl(s[d] ^ s[a], 16)
    s[c] = (s[c] + s[d]) & 0xffffffff; s[b] = rotl(s[b] ^ s[c], 12)
    s[a] = (s[a] + s[b]) & 0xffffffff; s[d] = rotl(s[d] ^ s[a], 8)
    s[c] = (s[c] + s[d]) & 0xffffffff; s[b] = rotl(s[b] ^ s[c], 7)
def block(key, counter, nonce):
    const = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]
    st = const + list(struct.unpack('<8I', key)) + [counter] + list(struct.unpack('<3I', nonce))
    w = st[:]
    for _ in range(10):
        qr(w,0,4,8,12); qr(w,1,5,9,13); qr(w,2,6,10,14); qr(w,3,7,11,15)
        qr(w,0,5,10,15); qr(w,1,6,11,12); qr(w,2,7,8,13); qr(w,3,4,9,14)
    return struct.pack('<16I', *[(a + b) & 0xffffffff for a, b in zip(w, st)])
def chacha20_xor(key, counter, nonce, data):
    out = bytearray()
    for i in range(0, len(data), 64):
        ks = block(key, counter + i // 64, nonce)
        out += bytes(x ^ y for x, y in zip(data[i:i+64], ks))
    return bytes(out)
def poly1305(key, msg):
    r = int.from_bytes(key[:16], 'little') & 0x0ffffffc0ffffffc0ffffffc0fffffff
    s = int.from_bytes(key[16:], 'little')
    p = (1 << 130) - 5; acc = 0
    for i in range(0, len(msg), 16):
        n = int.from_bytes(msg[i:i+16] + b'\x01', 'little')
        acc = ((acc + n) * r) % p
    return ((acc + s) & ((1 << 128) - 1)).to_bytes(16, 'little')
def pad16(b): return b + b'\x00' * (-len(b) % 16)
def seal(key, nonce, aad, pt):
    otk = block(key, 0, nonce)[:32]
    ct = chacha20_xor(key, 1, nonce, pt)
    tag = poly1305(otk, pad16(aad) + pad16(ct) + struct.pack('<QQ', len(aad), len(ct)))
    return ct + tag
def b64u(b): return base64.urlsafe_b64encode(b).rstrip(b'=').decode()

# RFC 8439 2.8.2 sanity vector
rk = bytes(range(0x80, 0xa0)); rn = bytes.fromhex('070000004041424344454647')
raad = bytes.fromhex('50515253c0c1c2c3c4c5c6c7')
rpt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it."
assert seal(rk, rn, raad, rpt)[-16:].hex() == '1ae10b594f09e26a7e902ecbd0600691', 'RFC 8439 tag mismatch'

f = json.load(open(sys.argv[1]))
sid = base64.urlsafe_b64decode(f['session_id'] + '==')
keys = {'c2d': bytes.fromhex(f['c2d_key_hex']), 'd2c': bytes.fromhex(f['d2c_key_hex'])}
def frame(direction, counter, plaintext):
    nonce = b'\x00' * 4 + struct.pack('>Q', counter)
    aad = b'riwork/v1/frame\x00' + sid + bytes([0 if direction == 'c2d' else 1]) + struct.pack('>Q', counter)
    ct = seal(keys[direction], nonce, aad, plaintext.encode())
    return nonce, aad, ct
# The Rust fixture must be reproduced exactly at counter 0 in both directions.
for fr, d in zip(f['frames'], ['c2d', 'd2c']):
    nonce, aad, ct = frame(d, 0, fr['plaintext_utf8'])
    assert nonce.hex() == fr['nonce_hex'] and aad.hex() == fr['aad_hex'] and b64u(ct) == fr['envelope']['ciphertext'], d
print('independent implementation reproduces the Rust fixture at counter 0', file=sys.stderr)

req = '{"v":1,"type":"request","id":"44444444-4444-4444-8444-444444444444","method":"projects.list","params":{}}'
res = '{"v":1,"type":"response","id":"44444444-4444-4444-8444-444444444444","ok":true,"result":{"projects":[]}}'
counters = [1, 2, 255, 256, 65535, 65536, 0x0102030405060708, 2**64 - 2]
out = {'source': 'Independent pure-Python RFC 8439 implementation following docs/remote-protocol.md; validated against remote/fixtures/v1.json at counter 0 (see ios/scripts/gen-counter-vectors.py). Reuses that fixture session (keys, session_id).', 'vectors': []}
for d, pt in (('c2d', req), ('d2c', res)):
    for c in counters:
        nonce, aad, ct = frame(d, c, pt)
        out['vectors'].append({'direction': d, 'counter': str(c), 'nonce_hex': nonce.hex(), 'aad_hex': aad.hex(), 'plaintext_utf8': pt,
            'envelope': {'v': 1, 'type': 'encrypted', 'session_id': f['session_id'], 'direction': d, 'counter': str(c), 'ciphertext': b64u(ct)}})
json.dump(out, sys.stdout, indent=2); print()
