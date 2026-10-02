#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

profile=${1:-debug}
binary="target/$profile/riwork"
bundle="target/$profile/RiWork.app"
resources="$bundle/Contents/Resources"
app_icon="assets/app-icon/RiWork-legacy.icns"
zig=${ZIG:-zig}

if [ ! -x "$binary" ]; then
    if [ "$profile" = debug ]; then
        echo "Build RiWork first: cargo build" >&2
    else
        echo "Build RiWork first: cargo build --profile $profile" >&2
    fi
    exit 1
fi

if [ ! -f "$app_icon" ]; then
    echo "RiWork app icon is missing: $app_icon" >&2
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
if ! command -v codesign >/dev/null 2>&1; then
    echo "codesign is required to sign the app bundle" >&2
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

# libghostty's crate contains the engine but not Ghostty's named theme catalog.
themes=${GHOSTTY_THEMES_DIR:-/Applications/Ghostty.app/Contents/Resources/ghostty/themes}
if [ -n "${GHOSTTY_THEMES_DIR:-}" ] && [ ! -d "$themes" ]; then
    echo "GHOSTTY_THEMES_DIR must point to Ghostty's themes directory" >&2
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
# Optional standalone connector: build with remote/Cargo.toml before bundling.
remote_binary="remote/target/$profile/riwork-remote"
if [ -x "$remote_binary" ]; then
    cp "$remote_binary" "$bundle/Contents/MacOS/riwork-remote"
fi
cp "$app_icon" "$resources/RiWork.icns"
tic -x -o "$resources/terminfo" "$work/ghostty.terminfo"
cp -R "$ghostty_source/shell-integration" "$resources/ghostty/"
if [ -d "$themes" ]; then
    cp -R "$themes" "$resources/ghostty/"
fi
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
    <key>CFBundleIconFile</key><string>RiWork.icns</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSMicrophoneUsageDescription</key><string>A program running within RiWork would like to use your microphone.</string>
</dict>
</plist>
PLIST

# Give the bundle a real, stable signature instead of the linker's per-build
# ad-hoc one: it binds Info.plist, seals the resources and fixes the identifier
# to CFBundleIdentifier. Set CODESIGN_IDENTITY to a certificate name to sign for
# distribution; the default is ad-hoc.
codesign_identity=${CODESIGN_IDENTITY:--}
identifier=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$bundle/Contents/Info.plist")
if [ -z "$identifier" ]; then
    echo "Could not read CFBundleIdentifier from $bundle/Contents/Info.plist" >&2
    exit 1
fi
# Extended attributes copied with the resources would make codesign reject the bundle.
xattr -cr "$bundle"
# Sign nested executables first: sealing the bundle records them but does not sign them.
if [ -f "$bundle/Contents/MacOS/riwork-remote" ]; then
    codesign --force --sign "$codesign_identity" --identifier "$identifier.remote" \
        "$bundle/Contents/MacOS/riwork-remote"
fi
codesign --force --sign "$codesign_identity" --identifier "$identifier" "$bundle"
codesign --verify --deep --strict "$bundle"

echo "$bundle"
