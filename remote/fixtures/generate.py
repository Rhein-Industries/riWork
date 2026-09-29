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

# v2 vectors. Private scalars are test-only; both Rust and Swift clamp them per RFC 7748.
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from cryptography.hazmat.primitives import serialization
invite_id='55555555-5555-4555-8555-555555555555'
invite_secret=bytes(range(160,192)); pair_c=bytes(range(192,224)); pair_d=bytes(range(224,256))
expires=1893456000
relay_url='wss://relay.example.com/v1/ws'
url=relay_url.encode(); prefix=i+uuid.UUID(invite_id).bytes+expires.to_bytes(8,'big')+len(url).to_bytes(2,'big')+url
pair_t=b'riwork/v2/pair\0'+prefix+pair_c+pair_d
pair_salt=hashlib.sha256(pair_t).digest()
root=HKDF(algorithm=hashes.SHA256(),length=32,salt=pair_salt,info=b'riwork/v2/root').derive(invite_secret)
hm=lambda key,body:b64(hmac.new(key,body,hashlib.sha256).digest())
pair_hello=dict(v=2,type='pair_hello',invite_id=invite_id,desktop_id=ids[0],device_id=ids[1],route_id=ids[2],client_nonce=b64(pair_c),mac=hm(invite_secret,b'riwork/v2/pair-hello\0'+prefix+pair_c))
pair_accept=dict(v=2,type='pair_accept',desktop_nonce=b64(pair_d),mac=hm(root,b'riwork/v2/pair-accept\0'+pair_t))
pair_finish=dict(v=2,type='pair_finish',mac=hm(root,b'riwork/v2/pair-finish\0'+pair_t))
def x25519(secret, peer=None):
    key=X25519PrivateKey.from_private_bytes(secret)
    if peer is None: return key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    return key.exchange(X25519PrivateKey.from_private_bytes(peer).public_key())
ce_priv=bytes(range(96,128)); de_priv=bytes(range(128,160))
ce,de=x25519(ce_priv),x25519(de_priv); dh=x25519(ce_priv,de_priv)
session_t=b'riwork/v2/session\0'+i+ce+de
session_salt=hashlib.sha256(session_t).digest(); sid=session_salt[:16]
ikm=root+dh
skeys={name:HKDF(algorithm=hashes.SHA256(),length=32,salt=session_salt,info=b'riwork/v2/'+name).derive(ikm) for name in [b'c2d',b'd2c',b'hs']}
client_hello=dict(v=2,type='client_hello',desktop_id=ids[0],device_id=ids[1],route_id=ids[2],client_eph=b64(ce),mac=hm(root,b'riwork/v2/client-hello\0'+i+ce))
server_hello=dict(v=2,type='server_hello',desktop_eph=b64(de),mac=hm(skeys[b'hs'],b'riwork/v2/server-hello\0'+session_t))
client_finish=dict(v=2,type='client_finish',mac=hm(skeys[b'hs'],b'riwork/v2/client-finish\0'+session_t))
v2_frames=[]
for direction,payload in [('c2d',dict(v=1,type='request',id='44444444-4444-4444-8444-444444444444',method='projects.list',params={})),('d2c',dict(v=1,type='ready',desktop_id=ids[0],device_id=ids[1]))]:
    plain=json.dumps(payload,separators=(',',':')).encode(); nonce=bytes(12)
    aad=b'riwork/v2/frame\0'+sid+bytes([direction=='d2c'])+bytes(8)
    cipher=ChaCha20Poly1305(skeys[direction.encode()]).encrypt(nonce,plain,aad)
    v2_frames.append(dict(plaintext_utf8=plain.decode(),nonce_hex=nonce.hex(),aad_hex=aad.hex(),envelope=dict(v=2,type='encrypted',session_id=b64(sid),direction=direction,counter='0',ciphertext=b64(cipher))))
v2=dict(v=2,desktop_id=ids[0],device_id=ids[1],route_id=ids[2],invite_id=invite_id,relay_url=relay_url,expires_at=expires,invite_secret=b64(invite_secret),pair_client_nonce=b64(pair_c),pair_desktop_nonce=b64(pair_d),identity_hex=i.hex(),pair_transcript_hex=pair_t.hex(),root_key_hex=root.hex(),client_private_hex=ce_priv.hex(),desktop_private_hex=de_priv.hex(),client_public_hex=ce.hex(),desktop_public_hex=de.hex(),dh_hex=dh.hex(),session_transcript_hex=session_t.hex(),session_salt_hex=session_salt.hex(),hs_key_hex=skeys[b'hs'].hex(),c2d_key_hex=skeys[b'c2d'].hex(),d2c_key_hex=skeys[b'd2c'].hex(),session_id=b64(sid),pair_hello=pair_hello,pair_accept=pair_accept,pair_finish=pair_finish,client_hello=client_hello,server_hello=server_hello,client_finish=client_finish,frames=v2_frames)
text=json.dumps(v2,indent=2)+'\n'
here=pathlib.Path(__file__).resolve()
here.with_name('v2.json').write_text(text)
here.parents[2].joinpath('ios/Tests/Fixtures/v2.json').write_text(text)
