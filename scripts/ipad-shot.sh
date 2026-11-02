#!/bin/sh
# Takes pictures of the iPad's screens (apps/riwork-mobile, 600 points and wider) from
# apps/riwork/tests/ipad/iPad.fixture.tsx, in a hidden window (scripts/pane-shot.sh): a host's
# dashboard (where the iPad opens with its one selected desktop), All desktops (the host menu's)
# and a project's page (chosen in the sidebar, opened over the dashboard in portrait), at an iPad
# mini's size in landscape (1133 x 744) or, with --portrait, portrait (744 x 1133). The pictures
# are twice the size (Retina).
#
#   scripts/ipad-shot.sh OUTDIR [--portrait] [--light]
set -eu
[ $# -ge 1 ] || { echo "Usage: scripts/ipad-shot.sh OUTDIR [--portrait] [--light]" >&2; exit 64; }
out=$1; shift
width=1133 height=744 scheme=dark suffix=
while [ $# -gt 0 ]; do
	case $1 in
	--portrait) width=744 height=1133 suffix=-portrait; shift ;;
	--light) scheme=light; suffix="$suffix-light"; shift ;;
	*) echo "Unknown option $1" >&2; exit 64 ;;
	esac
done
case $out in /*) ;; *) out=$PWD/$out ;; esac
mkdir -p "$out"
cd "$(dirname "$0")/.."
export RIWORK_GHOSTTY_INSTALL_DIR=${RIWORK_GHOSTTY_INSTALL_DIR:-$PWD/crates/riwork-nsh-terminal/.runtime/pinned/install}
shot() { printf "await app.screenshot(%s);" "\"$out/$1$suffix.png\""; }
scripts/pane-shot.sh apps/riwork/tests/ipad/iPad.fixture.tsx "$out/project$suffix.png" --width $width --height $height \
	--step "await app.setColorScheme('$scheme'); await app.wait(1000); $(shot dashboard)" \
	--step "app.onDialog((dialog) => dialog.buttons.includes('All desktops') ? 'All desktops' : null); await app.press('id:status-link'); await app.wait(800); $(shot hosts)" \
	--step "await app.press('id:host:mac-1'); await app.wait(1000); if (!app.shows('id:project:riwork')) { await app.press('id:sidebar-toggle'); await app.wait(500); }" \
	--step "await app.press('id:project:riwork'); await app.wait(1200);"
