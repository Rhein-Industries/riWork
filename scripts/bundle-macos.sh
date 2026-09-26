#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

profile=${1:-debug}
binary="target/$profile/riwork"
bundle="target/$profile/RiWork.app"
resources="$bundle/Contents/Resources"
zig=${ZIG:-zig}

if [ ! -x "$binary" ]; then
    if [ "$profile" = debug ]; then
        echo "Build RiWork first: cargo build" >&2
    else
        echo "Build RiWork first: cargo build --profile $profile" >&2
    fi
    exit 1
fi

case "$("$zig" version 2>/dev/null || true)" in
    0.16.*) ;;
    *) echo "Zig 0.16 is required; set ZIG to its executable" >&2; exit 1 ;;
esac
if ! command -v tic >/dev/null 2>&1; then
    echo "tic is required to compile Ghostty terminfo" >&2
    exit 1
fi

ghostty_crate=${GHOSTTY_SOURCE_DIR:-}
if [ -z "$ghostty_crate" ]; then
    set -- "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/gpui-libghostty-0.3.1
    ghostty_crate=$1
fi
ghostty_source="$ghostty_crate/vendor/ghostty/src"
if [ ! -f "$ghostty_source/terminfo/ghostty.zig" ] ||
   [ ! -d "$ghostty_source/shell-integration" ]; then
    echo "Could not find gpui-libghostty 0.3.1 sources; set GHOSTTY_SOURCE_DIR to the crate directory" >&2
    exit 1
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/riwork-ghostty.XXXXXX")
trap 'rm -rf "$work"' 0
cp "$ghostty_source/terminfo/Source.zig" "$ghostty_source/terminfo/ghostty.zig" "$work/"
cat > "$work/emit-terminfo.zig" <<'ZIG'
const std = @import("std");

pub fn main(init: std.process.Init) !void {
    var buffer: [1024]u8 = undefined;
    var stdout_writer = std.Io.File.stdout().writerStreaming(init.io, &buffer);
    try @import("ghostty.zig").ghostty.encode(&stdout_writer.interface);
    try stdout_writer.end();
}
ZIG
"$zig" run "$work/emit-terminfo.zig" -lc -O ReleaseFast > "$work/ghostty.terminfo"

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$resources/terminfo" "$resources/ghostty"
cp "$binary" "$bundle/Contents/MacOS/riwork"
tic -x -o "$resources/terminfo" "$work/ghostty.terminfo"
cp -R "$ghostty_source/shell-integration" "$resources/ghostty/"
if [ ! -f "$resources/terminfo/78/xterm-ghostty" ] ||
   [ ! -f "$resources/ghostty/shell-integration/zsh/ghostty-integration" ]; then
    echo "Ghostty runtime resources were not packaged correctly" >&2
    exit 1
fi
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleExecutable</key><string>riwork</string>
    <key>CFBundleIdentifier</key><string>dev.riwork.shell</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>RiWork</string>
    <key>CFBundleDisplayName</key><string>RiWork</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "$bundle"
