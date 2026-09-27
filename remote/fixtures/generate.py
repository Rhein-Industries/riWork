# Test fixture generator, using the independently maintained cryptography library.
# Run: uv run --with cryptography python remote/fixtures/generate.py
import base64, hashlib, hmac, json, pathlib, uuid
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives import hashes
b64=lambda b:base64.urlsafe_b64encode(b).decode().rstrip('=')
ids=['11111111-1111-4111-8111-111111111111','22222222-2222-4222-8222-222222222222','33333333-3333-4333-8333-333333333333']
psk=bytes(range(32)); c=bytes(range(32,64)); d=bytes(range(64,96))
i=b''.join(uuid.UUID(x).bytes for x in ids); t=b'riwork/v1/session\0'+i+c+d
salt=hashlib.sha256(t).digest(); s=salt[:16]
mac=lambda b:b64(hmac.new(psk,b,hashlib.sha256).digest())
keys={x:HKDF(algorithm=hashes.SHA256(),length=32,salt=salt,info=('riwork/v1/'+x).encode()).derive(psk) for x in ['c2d','d2c']}
hello=dict(v=1,type='client_hello',desktop_id=ids[0],device_id=ids[1],route_id=ids[2],client_nonce=b64(c),mac=mac(b'riwork/v1/client-hello\0'+i+c))
server=dict(v=1,type='server_hello',desktop_nonce=b64(d),mac=mac(b'riwork/v1/server-hello\0'+t))
finish=dict(v=1,type='client_finish',mac=mac(b'riwork/v1/client-finish\0'+t))
frames=[]
for direction,p in [('c2d',dict(v=1,type='request',id='44444444-4444-4444-8444-444444444444',method='projects.list',params={})),('d2c',dict(v=1,type='ready',desktop_id=ids[0],device_id=ids[1]))]:
    plain=json.dumps(p,separators=(',',':')).encode(); nonce=bytes(12)
    aad=b'riwork/v1/frame\0'+s+bytes([direction=='d2c'])+bytes(8)
    cipher=ChaCha20Poly1305(keys[direction]).encrypt(nonce,plain,aad)
    frames.append(dict(plaintext_utf8=plain.decode(),nonce_hex=nonce.hex(),aad_hex=aad.hex(),envelope=dict(v=1,type='encrypted',session_id=b64(s),direction=direction,counter='0',ciphertext=b64(cipher))))
f=dict(v=1,desktop_id=ids[0],device_id=ids[1],route_id=ids[2],pairing_secret=b64(psk),client_nonce=b64(c),desktop_nonce=b64(d),identity_hex=i.hex(),transcript_hex=t.hex(),salt_hex=salt.hex(),c2d_key_hex=keys['c2d'].hex(),d2c_key_hex=keys['d2c'].hex(),session_id=b64(s),client_hello=hello,server_hello=server,client_finish=finish,frames=frames)
pathlib.Path(__file__).with_name('v1.json').write_text(json.dumps(f,indent=2)+'\n')
