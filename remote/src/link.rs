//! The link extension (protocol v1 and v2 sessions alike, 2026-10-01).
//!
//! Two additive things, both in the desktop's replies, neither of which an older
//! phone or desktop can trip over:
//!
//! - `server_ms` on every response: how long the connector had the request, from
//!   the moment it was decrypted to the moment the reply was ready to be sealed. The
//!   phone takes it out of its round trip to see what the network cost (the CLI
//!   and tmux can take longer than the transfer itself).
//! - Optional compression of a reply's plaintext before it is encrypted. The
//!   desktop announces what it can do in the `features` of its first encrypted
//!   frame (`ready`); the phone opts in with the `link.configure` request. Until
//!   then, and for every reply under `MIN_COMPRESS_BYTES`, the plaintext is the
//!   plain JSON it always was. See `docs/remote-protocol.md`, "Link extension".
//!
//! A sealed plaintext is therefore one of two things, told apart by its first byte:
//!
//! - `{` (JSON never starts with anything else): the JSON text, as before.
//! - `FRAME_DEFLATE` (0x01): `0x01 || inflated_len (u32 big-endian) || raw deflate`
//!   (RFC 1951, no zlib header or checksum) of that JSON text. This is what Apple's
//!   `COMPRESSION_ZLIB` and zlib's `windowBits = -15` read.
//!
//! The marker is inside the authenticated ciphertext: nobody on the path can flip
//! it, and the envelope, its AAD and the fixtures stay as they were.
use anyhow::{Context, Result, bail, ensure};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    time::Instant,
};

/// First byte of a sealed plaintext that holds a deflated reply.
pub const FRAME_DEFLATE: u8 = 0x01;
/// Replies under this are sent as they are: the saving is gone in the header and the
/// 16-byte tag, and a short reply is the one an observer could learn the most from.
pub const MIN_COMPRESS_BYTES: usize = 2048;
/// The most a compressed reply may inflate to. A sealed frame is still at most
/// `MAX_PLAINTEXT` (128 KiB) long, so this is a bound on the work and the memory a
/// phone is asked for, not a way around the frame limit.
pub const MAX_INFLATED: usize = 2 * 1024 * 1024;
/// Largest `shell.history` page this connector accepts (the protocol said 1000 until
/// 2026-10-01). It is announced in `features`; the installed CLI has the last word.
pub const HISTORY_MAX_LINES: u32 = 5000;
/// 1 is the fastest, 9 the smallest. Measured on styled scrollback (a 270 KB page of
/// Claude Code output, 8.6x at level 1, 11.1x at 6, 12.0x at 9): level 6 costs about half
/// a millisecond per 100 KB, and level 9 only saves another 7 % for several times that.
pub const DEFLATE_LEVEL: u32 = 6;
/// Past this size compression runs on a blocking thread, so it does not occupy a tokio
/// worker (the connection's own loop still waits for it, a millisecond or two: level 6 does
/// about 250 MB/s).
pub const OFFLOAD_BYTES: usize = 128 * 1024;

/// What the first encrypted frame (`ready`) says this desktop offers. `history_max_lines` is what the connector takes, or less if the
/// installed CLI has been seen to refuse more (see `Rpc::history_max_lines`).
pub fn features(history_max_lines: u32) -> Value {
    json!({
        "deflate": {"min_bytes": MIN_COMPRESS_BYTES, "max_inflated": MAX_INFLATED},
        "history_max_lines": history_max_lines
    })
}

/// `link.configure` params. Unknown fields are refused like everywhere else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Configure {
    compression: Option<String>,
}

/// The connection's answer to `link.configure`, and the compression state it asks
/// for (`None`: leave as it was). `request` is the decoded request, `current` the state
/// of the session now; a malformed one is answered as `invalid_request` like any other.
pub fn configure(request: &Value, current: bool) -> (Value, Option<bool>) {
    let id = match request.get("id") {
        Some(Value::String(s)) if s.len() <= 64 => s.as_str(),
        _ => {
            return (
                crate::rpc::error_for(
                    Value::Null,
                    "invalid_request",
                    "request must be a JSON object with a string id of at most 64 bytes",
                ),
                None,
            );
        }
    };
    let fail = |message: &str| (crate::rpc::error(id, "invalid_request", message), None);
    // The same envelope rules as every request: no other field, version 1, a canonical UUID.
    let envelope: crate::rpc::Request = match serde_json::from_value(request.clone()) {
        Ok(envelope) => envelope,
        Err(e) => return fail(&e.to_string()),
    };
    if envelope.v != 1 || envelope.kind != "request" {
        return fail("unsupported request version/type");
    }
    if crate::crypto::uuid(id).is_err() {
        return fail("expected full lowercase canonical UUID");
    }
    // A struct would also read a JSON array as its fields in order.
    if !envelope.params.is_object() {
        return fail("params must be an object");
    }
    let params: Configure = match serde_json::from_value(envelope.params) {
        Ok(p) => p,
        Err(e) => return fail(&e.to_string()),
    };
    let on = match params.compression.as_deref() {
        None => None,
        Some("deflate") => Some(true),
        Some("none") => Some(false),
        Some(_) => return fail("compression must be \"deflate\" or \"none\""),
    };
    // Asking without a `compression` changes nothing, and the answer says what is.
    let state = if on.unwrap_or(current) {
        "deflate"
    } else {
        "none"
    };
    (
        json!({"v":1,"type":"response","id":id,"ok":true,"result":{"compression":state,"min_bytes":MIN_COMPRESS_BYTES,"max_inflated":MAX_INFLATED}}),
        on,
    )
}

/// A reply that would not fit one encrypted frame, compressed or not.
#[derive(Debug)]
pub struct TooLarge;
impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reply exceeds the encrypted response limit")
    }
}
impl std::error::Error for TooLarge {}

/// One reply, ready to be sealed.
#[derive(Debug)]
pub struct Encoded {
    /// The plaintext to seal: JSON, or a deflated frame.
    pub plaintext: Vec<u8>,
    /// Whether `plaintext` is a deflated frame.
    pub deflated: bool,
    /// The JSON text's length, whatever `plaintext` is.
    pub json_bytes: usize,
    /// The `server_ms` the reply carries.
    pub server_ms: u64,
}

fn elapsed_ms(since: Instant) -> u64 {
    // Rounded to the nearest millisecond.
    u64::try_from((since.elapsed().as_micros() + 500) / 1000).unwrap_or(u64::MAX)
}

fn suffix(has_fields: bool, ms: u64) -> Vec<u8> {
    format!("{}\"server_ms\":{ms}}}", if has_fields { "," } else { "" }).into_bytes()
}

/// Serializes `response` (a JSON object), adds `server_ms` as its last field, and
/// deflates it when `compress` is set and it is long enough to be worth it.
///
/// `received` is when the request was decrypted. The time spent compressing the
/// body counts: the body is deflated first and flushed to a byte boundary, the
/// clock is read, and the closing field is deflated onto the same stream, so the
/// number that goes out has already paid for the work that carries it. Sealing and
/// writing the socket come after and are not counted.
///
/// A reply whose sealed plaintext cannot fit `max_sealed` is `TooLarge`; so is one
/// whose JSON is past `MAX_INFLATED`.
pub fn encode_reply(
    response: &Value,
    received: Instant,
    compress: bool,
    max_sealed: usize,
) -> Result<Encoded, EncodeError> {
    let body = serde_json::to_vec(response).map_err(|e| EncodeError::Other(e.into()))?;
    encode_body(body, received, compress, max_sealed)
}

/// `encode_reply` for a response that is already serialized.
pub fn encode_body(
    mut body: Vec<u8>,
    received: Instant,
    compress: bool,
    max_sealed: usize,
) -> Result<Encoded, EncodeError> {
    if body.last() != Some(&b'}') || body.first() != Some(&b'{') {
        return Err(EncodeError::Other(anyhow::anyhow!(
            "reply is not a JSON object"
        )));
    }
    body.pop();
    let has_fields = body.len() > 1;
    // The whole text, including the field to come, bounds what a phone must inflate.
    let json_len = body.len() + suffix(has_fields, u64::MAX).len();
    if json_len > MAX_INFLATED {
        return Err(EncodeError::TooLarge);
    }
    if compress && json_len >= MIN_COMPRESS_BYTES {
        let level = Compression::new(DEFLATE_LEVEL);
        let mut encoder = DeflateEncoder::new(Vec::with_capacity(body.len() / 4 + 64), level);
        encoder
            .write_all(&body)
            .and_then(|()| encoder.flush())
            .map_err(|e| EncodeError::Other(e.into()))?;
        let ms = elapsed_ms(received);
        let tail = suffix(has_fields, ms);
        encoder
            .write_all(&tail)
            .map_err(|e| EncodeError::Other(e.into()))?;
        let deflated = encoder.finish().map_err(|e| EncodeError::Other(e.into()))?;
        let json_bytes = body.len() + tail.len();
        if let Some(plaintext) = framed(&deflated, json_bytes) {
            if plaintext.len() > max_sealed {
                return Err(EncodeError::TooLarge);
            }
            return Ok(Encoded {
                plaintext,
                deflated: true,
                json_bytes,
                server_ms: ms,
            });
        }
        body.extend_from_slice(&tail);
        if body.len() > max_sealed {
            return Err(EncodeError::TooLarge);
        }
        return Ok(Encoded {
            json_bytes,
            plaintext: body,
            deflated: false,
            server_ms: ms,
        });
    }
    let ms = elapsed_ms(received);
    body.extend_from_slice(&suffix(has_fields, ms));
    if body.len() > max_sealed {
        return Err(EncodeError::TooLarge);
    }
    Ok(Encoded {
        json_bytes: body.len(),
        plaintext: body,
        deflated: false,
        server_ms: ms,
    })
}

/// The deflated frame for a JSON text of `json_bytes`, or `None` when it would not be smaller than the
/// text itself (the header costs five bytes): then the text is sent as it is.
fn framed(deflated: &[u8], json_bytes: usize) -> Option<Vec<u8>> {
    if 5 + deflated.len() >= json_bytes {
        return None;
    }
    let mut plaintext = Vec::with_capacity(5 + deflated.len());
    plaintext.push(FRAME_DEFLATE);
    plaintext.extend_from_slice(&u32::try_from(json_bytes).ok()?.to_be_bytes());
    plaintext.extend_from_slice(deflated);
    Some(plaintext)
}

#[derive(Debug)]
pub enum EncodeError {
    TooLarge,
    Other(anyhow::Error),
}
impl From<EncodeError> for anyhow::Error {
    fn from(e: EncodeError) -> Self {
        match e {
            EncodeError::TooLarge => TooLarge.into(),
            EncodeError::Other(e) => e,
        }
    }
}

/// The JSON text inside a sealed plaintext, for either format. A deflated frame must
/// declare a length within `MAX_INFLATED` and inflate to exactly that many bytes.
pub fn decode_frame(plaintext: &[u8]) -> Result<Vec<u8>> {
    match plaintext.first() {
        Some(b'{') => Ok(plaintext.to_vec()),
        Some(&FRAME_DEFLATE) => {
            ensure!(plaintext.len() >= 5, "short deflate frame");
            let declared = u32::from_be_bytes(plaintext[1..5].try_into()?) as usize;
            ensure!(declared <= MAX_INFLATED, "inflated size over the limit");
            let mut out = Vec::with_capacity(declared);
            // One byte more than declared: a stream that inflates further is refused,
            // not silently cut.
            DeflateDecoder::new(&plaintext[5..])
                .take(declared as u64 + 1)
                .read_to_end(&mut out)
                .context("inflate")?;
            ensure!(
                out.len() == declared,
                "inflated size differs from the header"
            );
            Ok(out)
        }
        _ => bail!("unknown frame format"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(lines: usize) -> Value {
        let text: String = (0..lines)
            .map(|i| {
                format!(
                    "\u{1b}[38;5;{}mline {i} of the build output\u{1b}[0m\n",
                    i % 200
                )
            })
            .collect();
        json!({"v":1,"type":"response","id":"11111111-2222-4333-8444-555555555555","ok":true,
               "result":{"shell_id":"s","output":text,"line_count":lines}})
    }

    /// `cargo test --lib print_the_rust_vector -- --ignored --nocapture` prints the frame the
    /// fixture `fixtures/link.json` holds as its Rust-made vector (see `fixtures/generate_link.py`).
    #[test]
    #[ignore = "prints a vector; used to regenerate fixtures/link.json"]
    fn print_the_rust_vector() {
        let reply = fixture_reply();
        let encoded = encode_reply(&reply, Instant::now(), true, 131_072).unwrap();
        assert!(encoded.deflated);
        let json = String::from_utf8(decode_frame(&encoded.plaintext).unwrap()).unwrap();
        println!("RUST_FRAME_HEX={}", hex::encode(&encoded.plaintext));
        println!("RUST_JSON={json}");
    }
    /// The reply the fixture's vectors carry: styled history lines, like a real page.
    pub(super) fn fixture_reply() -> Value {
        let text: String = (0..60)
            .map(|i| {
                format!(
                    "\u{1b}[38;5;{}m{:>4} \u{2502} \u{1b}[0m\u{1b}[1mcargo test\u{1b}[0m --lib case_{i}::ok\n",
                    30 + i % 12,
                    i
                )
            })
            .collect();
        json!({"v":1,"type":"response","id":"7f9c2f1e-3b7d-4a39-8d0c-5b6f1c2d3e4f","ok":true,
               "result":{"shell_id":"44444444-4444-4444-8444-444444444444","output":text.trim_end_matches('\n'),"line_count":60,"history_size":4000,"complete":false}})
    }

    #[test]
    fn a_short_reply_stays_json_and_gains_server_ms() {
        let reply = json!({"v":1,"type":"response","id":"x","ok":true,"result":{"a":1}});
        let encoded = encode_reply(&reply, Instant::now(), true, 131_072).unwrap();
        assert!(!encoded.deflated);
        let value: Value = serde_json::from_slice(&encoded.plaintext).unwrap();
        assert_eq!(value["result"]["a"], 1);
        assert!(value["server_ms"].is_u64());
        assert_eq!(value["server_ms"].as_u64(), Some(encoded.server_ms));
        assert_eq!(encoded.json_bytes, encoded.plaintext.len());
    }

    #[test]
    fn server_ms_is_the_time_since_the_request_was_decrypted() {
        let long_ago = Instant::now() - std::time::Duration::from_millis(250);
        let reply = json!({"v":1,"type":"response","id":"x","ok":true,"result":{}});
        let encoded = encode_reply(&reply, long_ago, false, 131_072).unwrap();
        assert!(
            (250..400).contains(&encoded.server_ms),
            "{}",
            encoded.server_ms
        );
    }

    #[test]
    fn a_long_reply_is_deflated_and_inflates_to_the_same_json_plus_server_ms() {
        let reply = sample(400);
        let plain = serde_json::to_vec(&reply).unwrap();
        let encoded = encode_reply(&reply, Instant::now(), true, 131_072).unwrap();
        assert!(encoded.deflated);
        assert_eq!(encoded.plaintext[0], FRAME_DEFLATE);
        assert!(
            encoded.plaintext.len() * 3 < plain.len(),
            "styled text compresses well"
        );
        let json = decode_frame(&encoded.plaintext).unwrap();
        assert_eq!(json.len(), encoded.json_bytes);
        let mut value: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["server_ms"].as_u64(), Some(encoded.server_ms));
        value.as_object_mut().unwrap().remove("server_ms");
        assert_eq!(value, reply);
    }

    #[test]
    fn without_the_opt_in_a_long_reply_is_plain_json() {
        let encoded = encode_reply(&sample(400), Instant::now(), false, 262_144).unwrap();
        assert!(!encoded.deflated);
        assert_eq!(encoded.plaintext[0], b'{');
        assert_eq!(decode_frame(&encoded.plaintext).unwrap(), encoded.plaintext);
    }

    #[test]
    fn noisy_text_still_round_trips_and_a_frame_is_only_used_when_it_is_smaller() {
        // Printable noise deflates by a fifth or so, not to nothing; either way it must decode to the reply.
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let noise: String = (0..4000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                char::from(b'!' + (x % 90) as u8)
            })
            .collect();
        let reply = json!({"v":1,"type":"response","id":"x","ok":true,"result":{"t":noise}});
        let encoded = encode_reply(&reply, Instant::now(), true, 131_072).unwrap();
        let value: Value =
            serde_json::from_slice(&decode_frame(&encoded.plaintext).unwrap()).unwrap();
        assert_eq!(value["result"]["t"], reply["result"]["t"]);
        // The decision itself: a stream that is not smaller than the text by the header is not framed.
        assert!(
            framed(&[0; 95], 100).is_none(),
            "5 + 95 is not less than 100"
        );
        assert!(framed(&[0; 200], 100).is_none());
        assert_eq!(
            framed(&[7; 94], 100).map(|f| f.len()),
            Some(99),
            "5 + 94 is"
        );
    }

    #[test]
    fn a_reply_over_the_sealed_limit_is_too_large_even_when_compressed() {
        // Compresses well but not into 200 bytes.
        let reply = sample(400);
        assert!(matches!(
            encode_reply(&reply, Instant::now(), true, 200),
            Err(EncodeError::TooLarge)
        ));
        assert!(matches!(
            encode_reply(&sample(400), Instant::now(), false, 10_000),
            Err(EncodeError::TooLarge)
        ));
    }

    #[test]
    fn json_past_the_inflate_limit_is_too_large_whatever_the_ratio() {
        let blank = " ".repeat(MAX_INFLATED);
        let reply = json!({"v":1,"type":"response","id":"x","ok":true,"result":{"t":blank}});
        assert!(matches!(
            encode_reply(&reply, Instant::now(), true, 131_072),
            Err(EncodeError::TooLarge)
        ));
    }

    #[test]
    fn a_frame_is_refused_when_its_length_lies_or_its_format_is_unknown() {
        let encoded = encode_reply(&sample(400), Instant::now(), true, 131_072).unwrap();
        let mut shorter = encoded.plaintext.clone();
        let declared = u32::from_be_bytes(shorter[1..5].try_into().unwrap());
        shorter[1..5].copy_from_slice(&(declared - 1).to_be_bytes());
        assert!(
            decode_frame(&shorter).is_err(),
            "stream longer than declared"
        );
        let mut longer = encoded.plaintext.clone();
        longer[1..5].copy_from_slice(&(declared + 1).to_be_bytes());
        assert!(
            decode_frame(&longer).is_err(),
            "stream shorter than declared"
        );
        let mut huge = encoded.plaintext.clone();
        huge[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_frame(&huge).is_err());
        assert!(decode_frame(&encoded.plaintext[..3]).is_err());
        assert!(decode_frame(&[0x02, 0, 0, 0, 0]).is_err());
        assert!(decode_frame(b"").is_err());
        let mut truncated = encoded.plaintext;
        truncated.truncate(truncated.len() - 10);
        assert!(decode_frame(&truncated).is_err());
    }

    #[test]
    fn configure_turns_compression_on_and_off_and_refuses_anything_else() {
        let id = "11111111-2222-4333-8444-555555555555";
        let ask = |params: Value, current: bool| {
            configure(
                &json!({"v":1,"type":"request","id":id,"method":"link.configure","params":params}),
                current,
            )
        };
        let (reply, state) = ask(json!({"compression":"deflate"}), false);
        assert_eq!(state, Some(true));
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["result"]["compression"], "deflate");
        assert_eq!(reply["result"]["min_bytes"], MIN_COMPRESS_BYTES);
        let (reply, state) = ask(json!({"compression":"none"}), true);
        assert_eq!(
            (state, reply["result"]["compression"].as_str()),
            (Some(false), Some("none"))
        );
        // Asking changes nothing, and the answer says what is.
        for current in [true, false] {
            let (reply, state) = ask(json!({}), current);
            assert_eq!((state, reply["ok"].clone()), (None, json!(true)));
            assert_eq!(
                reply["result"]["compression"],
                if current { "deflate" } else { "none" }
            );
        }
        for bad in [
            json!({"compression":"zstd"}),
            json!({"compression":1}),
            json!({"compression":"deflate","level":9}),
            json!([]),
            json!(["deflate"]),
            Value::Null,
        ] {
            let (reply, state) = ask(bad.clone(), false);
            assert_eq!(state, None, "{bad}");
            assert_eq!(reply["ok"], false, "{bad}");
            assert_eq!(reply["error"]["code"], "invalid_request");
            assert_eq!(reply["id"], id);
        }
        // The envelope is held to the rules of every request.
        for bad in [
            json!({"v":1,"type":"request","id":id,"method":"link.configure","params":{},"extra":1}),
            json!({"v":2,"type":"request","id":id,"method":"link.configure","params":{}}),
            json!({"v":1,"type":"response","id":id,"method":"link.configure","params":{}}),
            json!({"v":1,"type":"request","id":id,"method":"link.configure"}),
        ] {
            let (reply, state) = configure(&bad, false);
            assert_eq!(state, None, "{bad}");
            assert_eq!(reply["error"]["code"], "invalid_request", "{bad}");
            assert_eq!(reply["id"], id);
        }
        let (reply, _) = configure(&json!({"v":1,"type":"request","id":7,"params":{}}), false);
        assert_eq!(reply["id"], Value::Null);
        let (reply, _) = configure(
            &json!({"v":1,"type":"request","id":"not-a-uuid","method":"link.configure","params":{}}),
            false,
        );
        assert_eq!(reply["error"]["code"], "invalid_request");
    }
}
