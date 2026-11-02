#!/bin/sh
# Takes pictures of the iPad's screens (apps/riwork-mobile, 600 points and wider) from
# apps/riwork/tests/ipad/iPad.fixture.tsx, in a hidden window (scripts/pane-shot.sh): the hosts,
# a host's dashboard and a project's page, at an iPad mini's size in landscape (1133 x 744) or,
# with --portrait, portrait (744 x 1133). The pictures are twice the size (Retina).
#
#   scripts/ipad-shot.sh OUTDIR [--portrait] [--light]
set -eu
[ $# -ge 1 ] || { echo "Usage: scripts/ipad-shot.sh OUTDIR [--portrait] [--light]" >&2; exit 64; }
out=$1; shift
width=1133 height=744 dark=--dark suffix=
while [ $# -gt 0 ]; do
	case $1 in
	--portrait) width=744 height=1133 suffix=-portrait; shift ;;
	--light) dark=; suffix="$suffix-light"; shift ;;
	*) echo "Unknown option $1" >&2; exit 64 ;;
	esac
done
case $out in /*) ;; *) out=$PWD/$out ;; esac
mkdir -p "$out"
cd "$(dirname "$0")/.."
export RIWORK_GHOSTTY_INSTALL_DIR=${RIWORK_GHOSTTY_INSTALL_DIR:-$PWD/crates/riwork-nsh-terminal/.runtime/pinned/install}
shot() { printf "await app.screenshot(%s);" "\"$out/$1$suffix.png\""; }
scripts/pane-shot.sh apps/riwork/tests/ipad/iPad.fixture.tsx "$out/project$suffix.png" --width $width --height $height $dark \
	--step "await app.wait(800); $(shot hosts)" \
	--step "try { await app.press('id:host:mac-1'); } catch {} await app.wait(1000); $(shot dashboard)" \
	--step "await app.press('id:project:riwork'); await app.wait(1200);"
