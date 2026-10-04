#!/bin/sh
# Regenerates ios/Tests/Fixtures/chat-serde.json from the Rust chat model (src/chat/model.rs), in a throwaway crate.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/src"
cp "$here/gen-chat-fixtures.rs" "$work/src/main.rs"
# The program refers to ../../src/chat/model.rs relative to itself: keep that path valid from the throwaway crate.
sed -i.bak "s#\"../../src/chat/model.rs\"#\"$here/../../src/chat/model.rs\"#" "$work/src/main.rs"
cat > "$work/Cargo.toml" <<TOML
[package]
name = "gen-chat-fixtures"
version = "0.0.0"
edition = "2024"
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
TOML
cargo run --quiet --manifest-path "$work/Cargo.toml" > "$here/../Tests/Fixtures/chat-serde.json"
echo "wrote ios/Tests/Fixtures/chat-serde.json"
