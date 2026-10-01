#!/usr/bin/env python3
# Vectors for the compressed reply frames of the link extension (docs/remote-protocol.md, "Link extension").
# A frame is JSON text, or 0x01 || inflated length (u32 big-endian) || raw deflate (RFC 1951) of that JSON text.
# The Python vectors are made with zlib, independently of the Rust and Swift code; the Rust vector is the desktop's own output
# (flate2) so that Apple's decoder is shown to read what the connector writes. Run:
#   cargo test --manifest-path remote/Cargo.toml --lib print_the_rust_vector -- --ignored --nocapture \
#     | python3 remote/fixtures/generate_link.py
# Writes remote/fixtures/link.json and ios/Tests/Fixtures/link.json.
import json, pathlib, re, struct, sys, zlib

def deflate(data, level=6):
    c = zlib.compressobj(level, zlib.DEFLATED, -15)
    return c.compress(data) + c.flush(zlib.Z_FINISH)

def deflate_with_sync_flush(head, tail, level=6):
    # What the connector does: the body, flushed to a byte boundary (so the clock can be read), then the closing field.
    c = zlib.compressobj(level, zlib.DEFLATED, -15)
    return c.compress(head) + c.flush(zlib.Z_SYNC_FLUSH) + c.compress(tail) + c.flush(zlib.Z_FINISH)

def frame(json_bytes, deflated):
    return b"\x01" + struct.pack(">I", len(json_bytes)) + deflated

rust_hex = rust_json = None
for line in sys.stdin.read().splitlines():
    m = re.match(r"RUST_FRAME_HEX=([0-9a-f]+)$", line)
    if m: rust_hex = m.group(1)
    if line.startswith("RUST_JSON="): rust_json = line[len("RUST_JSON="):]
assert rust_hex and rust_json, "pipe the output of the ignored Rust test in"

# The same reply the Rust vector carries (without its closing field), so every vector decodes to a page of styled history.
head = rust_json[: rust_json.rindex(',"server_ms"')].encode()
tail = b',"server_ms":3}'
text = head + tail
short = b'{"id":"7f9c2f1e-3b7d-4a39-8d0c-5b6f1c2d3e4f","ok":true,"result":{"unchanged":true},"server_ms":3}'
vectors = [
    dict(name="plain JSON (a short reply, or one the phone did not ask to compress)", frame_hex=short.hex(), json=short.decode()),
    dict(name="python zlib level 6, one stream", frame_hex=frame(text, deflate(text)).hex(), json=text.decode()),
    dict(name="python zlib level 6, sync flush before the closing field", frame_hex=frame(text, deflate_with_sync_flush(head, tail)).hex(), json=text.decode()),
    dict(name="python zlib level 1", frame_hex=frame(text, deflate(text, 1)).hex(), json=text.decode()),
    dict(name="rust flate2 level 6 (the connector)", frame_hex=rust_hex, json=rust_json),
]
good = frame(text, deflate(text))
declared = struct.unpack(">I", good[1:5])[0]
refused = [
    dict(name="declares one byte fewer than it inflates to", frame_hex=(good[:1] + struct.pack(">I", declared - 1) + good[5:]).hex()),
    dict(name="declares one byte more than it inflates to", frame_hex=(good[:1] + struct.pack(">I", declared + 1) + good[5:]).hex()),
    dict(name="declares more than the 2 MiB limit", frame_hex=(good[:1] + struct.pack(">I", 2 * 1024 * 1024 + 1) + good[5:]).hex()),
    dict(name="truncated stream", frame_hex=good[:-12].hex()),
    dict(name="unknown format marker", frame_hex=(b"\x02" + good[1:]).hex()),
    dict(name="header only", frame_hex=good[:5].hex()),
]
out = dict(description="Sealed-plaintext forms of a reply: JSON text, or 0x01 || u32be inflated length || raw deflate. See docs/remote-protocol.md, Link extension.",
           max_inflated=2 * 1024 * 1024, vectors=vectors, refused=refused)
for path in (pathlib.Path(__file__).with_name("link.json"), pathlib.Path(__file__).parents[2] / "ios/Tests/Fixtures/link.json"):
    path.write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n")
    print("wrote", path)
