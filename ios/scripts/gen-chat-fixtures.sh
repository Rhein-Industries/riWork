#!/bin/sh
# Regenerates ios/Tests/Fixtures/chat-serde.json from the Rust chat model (crates/riwork-engine/src/chat/model.rs), in a throwaway crate.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
riwork_root=$(cd "$here/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/src"
cp "$here/gen-chat-fixtures.rs" "$work/src/main.rs"
cat > "$work/Cargo.toml" <<TOML
[package]
name = "gen-chat-fixtures"
version = "0.0.0"
edition = "2024"
[dependencies]
riwork-engine = { path = "$riwork_root/crates/riwork-engine" }
serde_json = "1"
TOML
# Into a temporary file first, so a failed build leaves the fixture as it was.
cargo run --quiet --manifest-path "$work/Cargo.toml" > "$work/chat-serde.json"
mv "$work/chat-serde.json" "$here/../Tests/Fixtures/chat-serde.json"
echo "wrote ios/Tests/Fixtures/chat-serde.json"
