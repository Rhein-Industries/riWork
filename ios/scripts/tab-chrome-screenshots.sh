#!/bin/zsh
# Pictures of the top of the tab screen, a shell tab and a chat tab and the switch between them (AppTests/TabChromeScreenshots.swift),
# taken by the simulator itself so the status bar and Liquid Glass are drawn:
#   scripts/tab-chrome-screenshots.sh <directory> <simulator-udid> [landscape]   (landscape: iPad only; the iPhone app is portrait only)
# The test leaves <name>.ready while a state is on screen (this takes <name>.png) and <name>.rec while it switches tabs (this records
# <name>.mp4 and pulls frames from it into <name>-frames.png). Build first with `xcodebuild ... build-for-testing`.
set -uo pipefail
dir=$1; udid=$2; landscape=0; [[ "${3:-}" == landscape ]] && landscape=1
mkdir -p "$dir"
dir=$(cd "$dir" && pwd -P)
# Only signal and reap this invocation's own build and recorder children.
build=0; recorder=0
stop_child() {
  local child=$1 signal=$2
  (( child > 0 )) || return 0
  kill -$signal $child 2>/dev/null || true
  for attempt in {1..50}; do
    kill -0 $child 2>/dev/null || break
    sleep 0.1
  done
  kill -KILL $child 2>/dev/null || true
  wait $child 2>/dev/null || true
}
cleanup() { stop_child $recorder INT; stop_child $build TERM; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cd "$(dirname "$0")/.."
TEST_RUNNER_RIWORK_TAB_SCREENSHOTS_ONLY="${RIWORK_TAB_SCREENSHOTS_ONLY:-}" TEST_RUNNER_RIWORK_TAB_SCREENSHOTS="$dir" TEST_RUNNER_RIWORK_TAB_SCREENSHOTS_LANDSCAPE=$landscape xcodebuild -project RiWorkRemote.xcodeproj -scheme RiWorkRemote -destination "platform=iOS Simulator,id=$udid" -derivedDataPath .derivedData \
  CODE_SIGN_IDENTITY=- -only-testing:RiWorkAppTests/TabChromeScreenshots${RIWORK_TAB_SCREENSHOTS_ONLY:+/$RIWORK_TAB_SCREENSHOTS_ONLY} test-without-building > "$dir/xcodebuild.log" 2>&1 &
build=$!
deadline=$(( SECONDS + 900 ))
while kill -0 $build 2>/dev/null; do
  (( SECONDS < deadline )) || exit 124
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
    recording_deadline=$(( SECONDS + 30 ))
    while [[ -e "$rec" ]]; do
      kill -0 $build 2>/dev/null || exit 1
      (( SECONDS < recording_deadline && SECONDS < deadline )) || exit 124
      sleep 0.1
    done
    stop_child $recorder INT; recorder=0
    if command -v ffmpeg >/dev/null; then
      ffmpeg -loglevel error -y -i "$base.mp4" -vf "fps=6,scale=-2:600,tile=12x5" -frames:v 1 "$base-frames.png"
    fi
  done
  sleep 0.1
done
wait $build; result=$?; build=0
grep -E "Test Case .*(passed|failed)" "$dir/xcodebuild.log"
exit $result
