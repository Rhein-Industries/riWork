//! Protocol v2: single-use invite proof and X25519 session keys.
//! Ephemeral private keys are wiped after the Diffie-Hellman output is mixed in.
use super::{ClientFinish, Identity, Pending, Session, b64, decode, proof, tagged, uuid, verify};
use anyhow::{Result, ensure};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairHello {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub invite_id: String,
    pub desktop_id: String,
    pub device_id: String,
    pub route_id: String,
    pub client_nonce: String,
    pub mac: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairAccept {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub desktop_nonce: String,
    pub mac: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairFinish {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub mac: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHelloV2 {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub desktop_id: String,
    pub device_id: String,
    pub route_id: String,
    pub client_eph: String,
    pub mac: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerHelloV2 {
    pub v: u8,
    #[serde(rename = "type")]
    pub kind: String,
    pub desktop_eph: String,
    pub mac: String,
}

fn url_bytes(url: &str) -> Result<Vec<u8>> {
    let bytes = url.as_bytes();
    ensure!(bytes.len() <= 2048, "relay URL too long");
    let mut out = Vec::with_capacity(2 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(out)
}
fn pair_prefix(
    id: &Identity,
    invite_id: &str,
    expires_at: u64,
    relay_url: &str,
) -> Result<Vec<u8>> {
    let mut body = id.bytes()?;
    body.extend_from_slice(uuid(invite_id)?.as_bytes());
    body.extend_from_slice(&expires_at.to_be_bytes());
    body.extend(url_bytes(relay_url)?);
    Ok(body)
}
fn ids_match(hello_desktop: &str, hello_device: &str, hello_route: &str, id: &Identity) -> bool {
    hello_desktop == id.desktop_id && hello_device == id.device_id && hello_route == id.route_id
}
fn root_key(invite_secret: &[u8; 32], transcript: &[u8]) -> Result<[u8; 32]> {
    let salt = Sha256::digest(transcript);
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), invite_secret)
        .expand(b"riwork/v2/root", &mut out)
        .map_err(|_| anyhow::anyhow!("HKDF"))?;
    Ok(out)
}
fn x25519_public(private_key: &[u8; 32]) -> [u8; 32] {
    PublicKey::from(&StaticSecret::from(*private_key)).to_bytes()
}
fn x25519_dh(private_key: &[u8; 32], peer_public: &[u8; 32]) -> Result<[u8; 32]> {
    let shared = StaticSecret::from(*private_key)
        .diffie_hellman(&PublicKey::from(*peer_public))
        .to_bytes();
    ensure!(
        !bool::from(shared.ct_eq(&[0u8; 32])),
        "degenerate Diffie-Hellman output"
    );
    Ok(shared)
}
fn session_material(
    root: &[u8; 32],
    dh: &[u8; 32],
    transcript: &[u8],
) -> Result<(Session, [u8; 32])> {
    let salt = Sha256::digest(transcript);
    let mut ikm = [0u8; 64];
    ikm[..32].copy_from_slice(root);
    ikm[32..].copy_from_slice(dh);
    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    ikm.zeroize();
    let mut c2d = [0u8; 32];
    let mut d2c = [0u8; 32];
    let mut hs = [0u8; 32];
    hk.expand(b"riwork/v2/c2d", &mut c2d)
        .map_err(|_| anyhow::anyhow!("HKDF"))?;
    hk.expand(b"riwork/v2/d2c", &mut d2c)
        .map_err(|_| anyhow::anyhow!("HKDF"))?;
    hk.expand(b"riwork/v2/hs", &mut hs)
        .map_err(|_| anyhow::anyhow!("HKDF"))?;
    let mut id = [0u8; 16];
    id.copy_from_slice(&salt[..16]);
    Ok((
        Session {
            id,
            c2d,
            d2c,
            version: 2,
            c2d_counter: 0,
            d2c_counter: 0,
        },
        hs,
    ))
}

pub fn pair_hello(
    id: &Identity,
    relay_url: &str,
    invite_id: &str,
    expires_at: u64,
    invite_secret: &[u8; 32],
    client_nonce: [u8; 32],
) -> Result<PairHello> {
    let body = [
        pair_prefix(id, invite_id, expires_at, relay_url)?.as_slice(),
        &client_nonce,
    ]
    .concat();
    Ok(PairHello {
        v: 2,
        kind: "pair_hello".into(),
        invite_id: invite_id.into(),
        desktop_id: id.desktop_id.clone(),
        device_id: id.device_id.clone(),
        route_id: id.route_id.clone(),
        client_nonce: b64(&client_nonce),
        mac: proof(invite_secret, &tagged(b"riwork/v2/pair-hello\0", &body)),
    })
}
pub fn verify_pair_hello(
    id: &Identity,
    relay_url: &str,
    invite_id: &str,
    expires_at: u64,
    invite_secret: &[u8; 32],
    hello: &PairHello,
) -> Result<[u8; 32]> {
    ensure!(
        hello.v == 2 && hello.kind == "pair_hello",
        "unexpected pair hello"
    );
    ensure!(
        hello.invite_id == invite_id
            && ids_match(&hello.desktop_id, &hello.device_id, &hello.route_id, id),
        "wrong invite"
    );
    let nonce = decode::<32>(&hello.client_nonce)?;
    let body = [
        pair_prefix(id, invite_id, expires_at, relay_url)?.as_slice(),
        &nonce,
    ]
    .concat();
    verify(
        invite_secret,
        &tagged(b"riwork/v2/pair-hello\0", &body),
        &hello.mac,
    )?;
    Ok(nonce)
}
pub fn complete_pair(
    id: &Identity,
    relay_url: &str,
    invite_id: &str,
    expires_at: u64,
    invite_secret: &[u8; 32],
    hello: &PairHello,
    desktop_nonce: [u8; 32],
) -> Result<(PairAccept, [u8; 32], Vec<u8>)> {
    let client_nonce =
        verify_pair_hello(id, relay_url, invite_id, expires_at, invite_secret, hello)?;
    let transcript = [
        b"riwork/v2/pair\0".as_slice(),
        &pair_prefix(id, invite_id, expires_at, relay_url)?,
        &client_nonce,
        &desktop_nonce,
    ]
    .concat();
    let root = root_key(invite_secret, &transcript)?;
    let accept = PairAccept {
        v: 2,
        kind: "pair_accept".into(),
        desktop_nonce: b64(&desktop_nonce),
        mac: proof(&root, &tagged(b"riwork/v2/pair-accept\0", &transcript)),
    };
    Ok((accept, root, transcript))
}
pub fn accept_pair(
    id: &Identity,
    relay_url: &str,
    invite_id: &str,
    expires_at: u64,
    invite_secret: &[u8; 32],
    client_nonce: &[u8; 32],
    accept: &PairAccept,
) -> Result<(PairFinish, [u8; 32], Vec<u8>)> {
    ensure!(
        accept.v == 2 && accept.kind == "pair_accept",
        "unexpected pair accept"
    );
    let desktop_nonce = decode::<32>(&accept.desktop_nonce)?;
    let transcript = [
        b"riwork/v2/pair\0".as_slice(),
        &pair_prefix(id, invite_id, expires_at, relay_url)?,
        client_nonce,
        &desktop_nonce,
    ]
    .concat();
    let root = root_key(invite_secret, &transcript)?;
    verify(
        &root,
        &tagged(b"riwork/v2/pair-accept\0", &transcript),
        &accept.mac,
    )?;
    Ok((pair_finish_message(&root, &transcript), root, transcript))
}
pub fn pair_finish_message(root: &[u8; 32], transcript: &[u8]) -> PairFinish {
    PairFinish {
        v: 2,
        kind: "pair_finish".into(),
        mac: proof(root, &tagged(b"riwork/v2/pair-finish\0", transcript)),
    }
}
pub fn verify_pair_finish(root: &[u8; 32], transcript: &[u8], finish: &PairFinish) -> Result<()> {
    ensure!(
        finish.v == 2 && finish.kind == "pair_finish",
        "unexpected pair finish"
    );
    verify(
        root,
        &tagged(b"riwork/v2/pair-finish\0", transcript),
        &finish.mac,
    )
}
pub fn client_hello_v2(
    id: &Identity,
    root: &[u8; 32],
    mut client_private: [u8; 32],
) -> Result<(ClientHelloV2, [u8; 32])> {
    let public = x25519_public(&client_private);
    client_private.zeroize();
    let body = [id.bytes()?.as_slice(), &public].concat();
    Ok((
        ClientHelloV2 {
            v: 2,
            kind: "client_hello".into(),
            desktop_id: id.desktop_id.clone(),
            device_id: id.device_id.clone(),
            route_id: id.route_id.clone(),
            client_eph: b64(&public),
            mac: proof(root, &tagged(b"riwork/v2/client-hello\0", &body)),
        },
        public,
    ))
}
pub fn accept_client_hello_v2(
    id: &Identity,
    root: &[u8; 32],
    hello: &ClientHelloV2,
    mut desktop_private: [u8; 32],
) -> Result<(ServerHelloV2, Pending)> {
    let result = (|| {
        ensure!(
            hello.v == 2 && hello.kind == "client_hello",
            "unexpected hello"
        );
        ensure!(
            ids_match(&hello.desktop_id, &hello.device_id, &hello.route_id, id),
            "wrong device/route"
        );
        let client_public = decode::<32>(&hello.client_eph)?;
        let body = [id.bytes()?.as_slice(), &client_public].concat();
        verify(
            root,
            &tagged(b"riwork/v2/client-hello\0", &body),
            &hello.mac,
        )?;
        let desktop_public = x25519_public(&desktop_private);
        let mut dh = x25519_dh(&desktop_private, &client_public)?;
        let transcript = [
            b"riwork/v2/session\0".as_slice(),
            &id.bytes()?,
            &client_public,
            &desktop_public,
        ]
        .concat();
        let (session, hs) = session_material(root, &dh, &transcript)?;
        dh.zeroize();
        let server = ServerHelloV2 {
            v: 2,
            kind: "server_hello".into(),
            desktop_eph: b64(&desktop_public),
            mac: proof(&hs, &tagged(b"riwork/v2/server-hello\0", &transcript)),
        };
        let pending = Pending {
            t: transcript,
            secret: hs,
            version: 2,
            keys: Some((session.id, session.c2d, session.d2c)),
        };
        Ok((server, pending))
    })();
    desktop_private.zeroize();
    result
}
pub fn accept_server_hello_v2(
    id: &Identity,
    root: &[u8; 32],
    mut client_private: [u8; 32],
    server: &ServerHelloV2,
) -> Result<(ClientFinish, Session)> {
    let result = (|| {
        ensure!(
            server.v == 2 && server.kind == "server_hello",
            "unexpected server hello"
        );
        let client_public = x25519_public(&client_private);
        let desktop_public = decode::<32>(&server.desktop_eph)?;
        let mut dh = x25519_dh(&client_private, &desktop_public)?;
        let transcript = [
            b"riwork/v2/session\0".as_slice(),
            &id.bytes()?,
            &client_public,
            &desktop_public,
        ]
        .concat();
        let (session, hs) = session_material(root, &dh, &transcript)?;
        dh.zeroize();
        verify(
            &hs,
            &tagged(b"riwork/v2/server-hello\0", &transcript),
            &server.mac,
        )?;
        let finish = ClientFinish {
            v: 2,
            kind: "client_finish".into(),
            mac: proof(&hs, &tagged(b"riwork/v2/client-finish\0", &transcript)),
        };
        Ok((finish, session))
    })();
    client_private.zeroize();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_diffie_hellman_is_rejected() {
        // RFC 7748 all-zero output is the low-order-point failure mode.
        let mut scalar = [0u8; 32];
        scalar[0] = 1;
        // A peer public key of all zeros is a low-order encoding x25519 rejects as a
        // shared secret of zero after clamping. Some scalars still produce zero; the
        // helper must not return that output.
        let peer = [0u8; 32];
        assert!(x25519_dh(&scalar, &peer).is_err());
    }
    #[test]
    fn recorded_session_key_is_not_a_function_of_the_root_alone() {
        let root = [7u8; 32];
        let dh = [9u8; 32];
        let other = [8u8; 32];
        let transcript = b"riwork/v2/session\0fixture";
        let (session, _) = session_material(&root, &dh, transcript).unwrap();
        let (without_dh, _) = session_material(&root, &other, transcript).unwrap();
        assert_ne!(session.c2d, without_dh.c2d);
        assert_ne!(session.d2c, without_dh.d2c);
    }
}
