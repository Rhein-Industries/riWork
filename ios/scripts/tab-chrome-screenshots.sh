#!/bin/zsh
# Pictures of the top of the tab screen, a shell tab and a chat tab and the switch between them (AppTests/TabChromeScreenshots.swift),
# taken by the simulator itself so the status bar and Liquid Glass are drawn:
#   scripts/tab-chrome-screenshots.sh <directory> <simulator-udid> [landscape]   (landscape: iPad only; the iPhone app is portrait only)
# The test leaves <name>.ready while a state is on screen (this takes <name>.png) and <name>.rec while it switches tabs (this records
# <name>.mp4 and pulls frames from it into <name>-frames.png). Build first with `xcodebuild ... build-for-testing`.
set -uo pipefail
dir=$1; udid=$2; landscape=0; [[ "${3:-}" == landscape ]] && landscape=1
mkdir -p "$dir"
cd "$(dirname "$0")/.."
TEST_RUNNER_RIWORK_TAB_SCREENSHOTS="$dir" TEST_RUNNER_RIWORK_TAB_SCREENSHOTS_LANDSCAPE=$landscape xcodebuild -project RiWorkRemote.xcodeproj -scheme RiWorkRemote -destination "platform=iOS Simulator,id=$udid" -derivedDataPath .derivedData \
  CODE_SIGN_IDENTITY=- -only-testing:RiWorkAppTests/TabChromeScreenshots test-without-building > "$dir/xcodebuild.log" 2>&1 &
build=$!
while kill -0 $build 2>/dev/null; do
  for ready in "$dir"/*.ready(N); do
    sleep 0.2
    xcrun simctl io "$udid" screenshot "${ready%.ready}.png" >/dev/null 2>&1
  done
  for rec in "$dir"/*.rec(N); do
    base=${rec%.rec}
    [[ -e "$base.recording" ]] && continue
    xcrun simctl io "$udid" recordVideo --codec=h264 --force "$base.mp4" >/dev/null 2>&1 &
    recorder=$!
    sleep 0.8
    touch "$base.recording"
    while [[ -e "$rec" ]]; do sleep 0.1; done
    kill -INT $recorder; wait $recorder
    if command -v ffmpeg >/dev/null; then
      ffmpeg -loglevel error -y -i "$base.mp4" -vf "fps=6,scale=-2:600,tile=12x5" -frames:v 1 "$base-frames.png"
    fi
  done
  sleep 0.1
done
wait $build; result=$?
grep -E "Test Case .*(passed|failed)" "$dir/xcodebuild.log"
exit $result
