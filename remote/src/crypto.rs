//! Frozen protocol bytes. No custom cipher primitives.
use anyhow::{Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}
pub fn decode<const N: usize>(s: &str) -> Result<[u8; N]> {
    let v = URL_SAFE_NO_PAD.decode(s)?;
    ensure!(b64(&v) == s, "noncanonical base64url");
    v.try_into()
        .map_err(|_| anyhow::anyhow!("wrong byte length"))
}
pub fn random32() -> [u8; 32] {
    let mut a = [0; 32];
    OsRng.fill_bytes(&mut a);
    a
}
pub fn uuid(s: &str) -> Result<Uuid> {
    let u = Uuid::parse_str(s)?;
    ensure!(u.to_string() == s, "expected full lowercase canonical UUID");
    Ok(u)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub desktop_id: String,
    pub device_id: String,
    pub route_id: String,
}
impl Identity {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        let mut v = Vec::with_capacity(48);
        for id in [&self.desktop_id, &self.device_id, &self.route_id] {
            v.extend_from_slice(uuid(id)?.as_bytes());
        }
        Ok(v)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub desktop_id: String,
    pub device_id: String,
    pub route_id: String,
    pub client_nonce: String,
    pub mac: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerHello {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub desktop_nonce: String,
    pub mac: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientFinish {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub mac: String,
}

fn tagged(label: &[u8], body: &[u8]) -> Vec<u8> {
    [label, body].concat()
}
fn proof(secret: &[u8; 32], input: &[u8]) -> String {
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("32-byte key");
    h.update(input);
    b64(&h.finalize().into_bytes())
}
fn verify(secret: &[u8; 32], input: &[u8], mac: &str) -> Result<()> {
    let mac = decode::<32>(mac)?;
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(secret)?;
    h.update(input);
    h.verify_slice(&mac)
        .map_err(|_| anyhow::anyhow!("authentication failed"))
}
fn transcript(id: &Identity, c: &[u8; 32], d: &[u8; 32]) -> Result<Vec<u8>> {
    Ok([b"riwork/v1/session\0".as_slice(), &id.bytes()?, c, d].concat())
}
pub fn client_hello(id: &Identity, secret: &[u8; 32], c: [u8; 32]) -> Result<ClientHello> {
    let body = [id.bytes()?.as_slice(), &c].concat();
    Ok(ClientHello {
        v: 1,
        kind: "client_hello".into(),
        desktop_id: id.desktop_id.clone(),
        device_id: id.device_id.clone(),
        route_id: id.route_id.clone(),
        client_nonce: b64(&c),
        mac: proof(secret, &tagged(b"riwork/v1/client-hello\0", &body)),
    })
}
/// Desktop verifies the route-bound device before creating any session keys.
pub fn accept_hello(
    id: &Identity,
    secret: &[u8; 32],
    hello: &ClientHello,
    d: [u8; 32],
) -> Result<(ServerHello, Pending)> {
    ensure!(
        hello.v == 1 && hello.kind == "client_hello",
        "unexpected hello"
    );
    ensure!(
        hello.desktop_id == id.desktop_id
            && hello.device_id == id.device_id
            && hello.route_id == id.route_id,
        "wrong device/route"
    );
    let c = decode::<32>(&hello.client_nonce)?;
    let body = [id.bytes()?.as_slice(), &c].concat();
    verify(
        secret,
        &tagged(b"riwork/v1/client-hello\0", &body),
        &hello.mac,
    )?;
    let t = transcript(id, &c, &d)?;
    let server = ServerHello {
        v: 1,
        kind: "server_hello".into(),
        desktop_nonce: b64(&d),
        mac: proof(secret, &tagged(b"riwork/v1/server-hello\0", &t)),
    };
    Ok((server, Pending { t, secret: *secret }))
}
pub fn accept_server(
    id: &Identity,
    secret: &[u8; 32],
    c: &[u8; 32],
    server: &ServerHello,
) -> Result<(ClientFinish, Session)> {
    ensure!(
        server.v == 1 && server.kind == "server_hello",
        "unexpected server hello"
    );
    let d = decode::<32>(&server.desktop_nonce)?;
    let t = transcript(id, c, &d)?;
    verify(
        secret,
        &tagged(b"riwork/v1/server-hello\0", &t),
        &server.mac,
    )?;
    Ok((
        ClientFinish {
            v: 1,
            kind: "client_finish".into(),
            mac: proof(secret, &tagged(b"riwork/v1/client-finish\0", &t)),
        },
        Session::derive(secret, &t)?,
    ))
}
pub struct Pending {
    t: Vec<u8>,
    secret: [u8; 32],
}
impl Pending {
    pub fn finish(self, f: &ClientFinish) -> Result<Session> {
        ensure!(f.v == 1 && f.kind == "client_finish", "unexpected finish");
        verify(
            &self.secret,
            &tagged(b"riwork/v1/client-finish\0", &self.t),
            &f.mac,
        )?;
        Session::derive(&self.secret, &self.t)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub session_id: String,
    pub direction: String,
    pub counter: String,
    pub ciphertext: String,
}
pub struct Session {
    pub id: [u8; 16],
    pub c2d: [u8; 32],
    pub d2c: [u8; 32],
    c2d_counter: u64,
    d2c_counter: u64,
}
impl Session {
    fn derive(secret: &[u8; 32], t: &[u8]) -> Result<Self> {
        let salt = Sha256::digest(t);
        let hk = Hkdf::<Sha256>::new(Some(&salt), secret);
        let mut c2d = [0; 32];
        let mut d2c = [0; 32];
        hk.expand(b"riwork/v1/c2d", &mut c2d)
            .map_err(|_| anyhow::anyhow!("HKDF"))?;
        hk.expand(b"riwork/v1/d2c", &mut d2c)
            .map_err(|_| anyhow::anyhow!("HKDF"))?;
        let mut id = [0; 16];
        id.copy_from_slice(&salt[..16]);
        Ok(Self {
            id,
            c2d,
            d2c,
            c2d_counter: 0,
            d2c_counter: 0,
        })
    }
    fn parts(&self, direction: &str, counter: u64) -> Result<([u8; 32], [u8; 12], Vec<u8>)> {
        let (key, dir) = match direction {
            "c2d" => (self.c2d, 0),
            "d2c" => (self.d2c, 1),
            _ => bail!("bad direction"),
        };
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&counter.to_be_bytes());
        let aad = [
            b"riwork/v1/frame\0".as_slice(),
            &self.id,
            &[dir],
            &counter.to_be_bytes(),
        ]
        .concat();
        Ok((key, nonce, aad))
    }
    fn next(&self, direction: &str) -> Result<u64> {
        match direction {
            "c2d" => Ok(self.c2d_counter),
            "d2c" => Ok(self.d2c_counter),
            _ => bail!("bad direction"),
        }
    }
    fn advance(&mut self, direction: &str) -> Result<()> {
        let counter = if direction == "c2d" {
            &mut self.c2d_counter
        } else {
            &mut self.d2c_counter
        };
        *counter = counter
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("counter exhausted"))?;
        Ok(())
    }
    pub fn seal(&mut self, direction: &str, plaintext: &[u8]) -> Result<Envelope> {
        ensure!(plaintext.len() <= crate::MAX_PLAINTEXT, "plaintext limit");
        let counter = self.next(direction)?;
        ensure!(counter < u64::MAX, "counter exhausted");
        let (key, nonce, aad) = self.parts(direction, counter)?;
        let cipher = ChaCha20Poly1305::new((&key).into())
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        self.advance(direction)?;
        Ok(Envelope {
            v: 1,
            kind: "encrypted".into(),
            session_id: b64(&self.id),
            direction: direction.into(),
            counter: counter.to_string(),
            ciphertext: b64(&cipher),
        })
    }
    pub fn open(&mut self, direction: &str, e: &Envelope) -> Result<Vec<u8>> {
        ensure!(
            e.v == 1
                && e.kind == "encrypted"
                && e.direction == direction
                && e.session_id == b64(&self.id),
            "wrong session/direction"
        );
        let counter = e.counter.parse::<u64>()?;
        ensure!(
            counter.to_string() == e.counter
                && counter == self.next(direction)?
                && counter < u64::MAX,
            "replay or counter gap"
        );
        ensure!(
            e.ciphertext.len() <= (crate::MAX_PLAINTEXT + 16).div_ceil(3) * 4,
            "ciphertext limit"
        );
        let cipher = URL_SAFE_NO_PAD.decode(&e.ciphertext)?;
        ensure!(
            b64(&cipher) == e.ciphertext && cipher.len() <= crate::MAX_PLAINTEXT + 16,
            "ciphertext encoding/limit"
        );
        let (key, nonce, aad) = self.parts(direction, counter)?;
        let plain = ChaCha20Poly1305::new((&key).into())
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &cipher,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("authentication failed"))?;
        self.advance(direction)?;
        Ok(plain)
    }
}
