#!/bin/zsh
# Pictures of the dictation mic (AppTests/DictationScreenshots.swift), taken by the simulator itself so Liquid Glass is drawn:
#   scripts/dictation-screenshots.sh <directory> <simulator-udid>
# The tests leave <name>.ready while a state is on screen; this takes <name>.png and the tests move on.
set -uo pipefail
dir=$1; udid=$2
mkdir -p "$dir"
cd "$(dirname "$0")/.."
TEST_RUNNER_RIWORK_SPEECH_SCREENSHOTS="$dir" xcodebuild -project RiWorkRemote.xcodeproj -scheme RiWorkRemote -destination "platform=iOS Simulator,id=$udid" -derivedDataPath .derivedData \
  CODE_SIGN_IDENTITY=- -only-testing:RiWorkAppTests/DictationScreenshots test-without-building > "$dir/xcodebuild.log" 2>&1 &
build=$!
while kill -0 $build 2>/dev/null; do
  for ready in "$dir"/*.ready(N); do
    sleep 0.3
    xcrun simctl io "$udid" screenshot "${ready%.ready}.png" >/dev/null 2>&1
  done
  sleep 0.2
done
wait $build; result=$?
grep -E "Test Case .*(passed|failed)" "$dir/xcodebuild.log"
exit $result
