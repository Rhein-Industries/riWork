#!/bin/zsh
# Speaks the sentences of AppTests/SpeechClipTests.swift with macOS `say` and writes them as 16 kHz mono WAV files, for the
# simulator to run through the on-device recognizers:
#
#   scripts/make-speech-clips.sh [directory]        # default /tmp/riwork-speech-clips; VOICE=Samantha by default
#   TEST_RUNNER_RIWORK_SPEECH_CLIPS=/tmp/riwork-speech-clips xcodebuild … -only-testing:RiWorkAppTests/SpeechClipTests test
set -euo pipefail
dir=${1:-/tmp/riwork-speech-clips}
mkdir -p "$dir"
sentences=(
  "git checkout ios-speech-input and run swift test"
  "Ask Claude to refactor the KeyBarView capsule inset"
  "Codex, open RemoteModel plus Keys dot swift and fix the pending key buffer"
  "run xcodebuild with the iPhone 17 Pro simulator"
  "cd into the riWork worktree and run npm install"
  "kubectl get pods in the staging namespace"
  "Tell Codex to rename SpeechVocabulary to SessionVocabulary"
  "grep for ChatComposer in the ios folder"
)
i=0
for sentence in "${sentences[@]}"; do
  i=$((i + 1))
  say -v "${VOICE:-Samantha}" -o "$dir/clip$i.aiff" "$sentence"
  afconvert -f WAVE -d LEI16@16000 -c 1 "$dir/clip$i.aiff" "$dir/clip$i.wav"
  rm "$dir/clip$i.aiff"
done
echo "$i clips in $dir"
