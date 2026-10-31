#!/bin/sh
# Takes a picture of an app pane from its fixture, drawn by the real NSH macOS host.
#
#   scripts/pane-shot.sh FIXTURE OUT.png [--width N] [--height N] [--dark]
#                        [--wait SELECTOR] [--step 'await app.press("id:...")']...
#
# FIXTURE is a *.fixture.tsx that mounts the pane with in-memory data. The app around it (the
# nearest folder with an App.tsx) is built and opened in the NSH macOS development host with a
# hidden window (NSH_HIDDEN_WINDOW, see apps/riwork/vendor/nsh-platform-macos): nothing shows
# and the focus stays where it is; PANE_SHOT_VISIBLE=1 shows the window. --wait waits for a selector (`id:flow:code-review`) and each
# --step is a line of test code run before the picture, in order. The picture is at twice the
# size (Retina). NSH_CLI, else RiWork's nsh 0.3 toolchain, else `nsh` on PATH runs it.
set -eu

usage() { echo "Usage: scripts/pane-shot.sh FIXTURE OUT.png [--width N] [--height N] [--dark] [--wait SELECTOR] [--step CODE]..." >&2; exit 64; }
[ $# -ge 2 ] || usage
fixture=$1 out=$2
shift 2
width=900 height=900 dark= wait= steps=
while [ $# -gt 0 ]; do
	case $1 in
	--width) width=${2:?}; shift 2 ;;
	--height) height=${2:?}; shift 2 ;;
	--dark) dark=1; shift ;;
	--wait) wait=${2:?}; shift 2 ;;
	--step) steps="$steps    $2
"; shift 2 ;;
	*) usage ;;
	esac
done
[ -f "$fixture" ] || { echo "No fixture at $fixture" >&2; exit 66; }

nsh=${NSH_CLI:-}
if [ -z "$nsh" ]; then
	for candidate in "${RIWORK_HOME:-$HOME/.local/share/riwork}"/toolchains/nsh-0.3*/bin/nsh; do
		[ -x "$candidate" ] && { nsh=$candidate; break; }
	done
fi
nsh=${nsh:-nsh}

fixture=$(cd "$(dirname "$fixture")" && pwd)/$(basename "$fixture")
case $out in /*) ;; *) out=$PWD/$out ;; esac
mkdir -p "$(dirname "$out")"
app=$(dirname "$fixture")
while [ ! -f "$app/App.tsx" ]; do
	[ "$app" != / ] || { echo "No app (App.tsx) around $fixture" >&2; exit 66; }
	app=$(dirname "$app")
done

dir=$app/.pane-shot
mkdir -p "$dir"
test_file=$dir/shot-$$.test.tsx
trap 'rm -f "$test_file"' EXIT INT TERM
entry=$(perl -MFile::Spec -e 'print File::Spec->abs2rel($ARGV[0], $ARGV[1])' "$fixture" "$dir")
json() { perl -MJSON::PP -e 'print JSON::PP->new->allow_nonref->encode($ARGV[0])' "$1"; }
{
	echo 'import { open, test } from "nsh/test";'
	echo 'test("pane shot", async () => {'
	echo "  const app = await open($(json "$entry"), { platform: \"macos\", width: $width, height: $height });"
	echo '  try {'
	[ -n "$dark" ] && echo '    await app.setColorScheme("dark");'
	[ -n "$wait" ] && echo "    await app.waitFor($(json "$wait"), { timeout: 15000 });"
	printf '%s' "$steps"
	echo '    await app.wait(400);'
	echo "    await app.screenshot($(json "$out"));"
	echo '  } finally { app.dispose(); }'
	echo '});'
} >"$test_file"

cd "$app"
[ -n "${PANE_SHOT_VISIBLE:-}" ] || export NSH_HIDDEN_WINDOW=1
"$nsh" test --target macos "$test_file" 2>&1 | grep -v '^warn riwork/native exports' || true
[ -s "$out" ] || { echo "No picture was taken." >&2; exit 1; }
size=$(sips -g pixelWidth -g pixelHeight "$out" 2>/dev/null | awk '/pixel/ { printf "%s%s", sep, $2; sep="x" }')
echo "$out ($size)"
