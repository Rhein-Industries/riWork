# Sourced by pane-shot.sh and pane-test.sh. Finds Somtus's nsh 0.3 and, unless RIWORK_HOME is
# already a /tmp fixture home, gives the run a private Somtus home, runtime folder and tmux
# folder, so a fixture or a test can never change the person's own settings, layouts, projects
# or Keychain key, nor read their Orca profile. RIWORK_NSH_FILES_FIXTURE=1 selects the fixture
# services such homes allow (an in-memory API key store, fixture folders) and
# RIWORK_NSH_CHAT_FIXTURE=1 the in-process chat host that replays recorded provider streams
# (never a live provider). Sets `nsh` and `private_home` (empty when the caller brought its own
# /tmp home); the caller removes `private_home` when it is done.
real_home=$HOME/.local/share/riwork
nsh=${NSH_CLI:-}
if [ -z "$nsh" ]; then
	for candidate in "${RIWORK_HOME:-$real_home}"/toolchains/nsh-0.3*/bin/nsh "$real_home"/toolchains/nsh-0.3*/bin/nsh; do
		[ -x "$candidate" ] && { nsh=$candidate; break; }
	done
fi
nsh=${nsh:-nsh}
case ${RIWORK_HOME:-} in
/tmp/* | /private/tmp/*) private_home= ;;
*)
	# Short, under /tmp: the desktop socket's path has a length limit.
	private_home=$(mktemp -d /tmp/riwork-pane.XXXXXX)
	mkdir -p "$private_home/home" "$private_home/run" "$private_home/tmux"
	export RIWORK_HOME="$private_home/home" RIWORK_RUNTIME_DIR="$private_home/run" TMUX_TMPDIR="$private_home/tmux" \
		ORCA_USER_DATA_PATH="$private_home/orca"
	;;
esac
# Codex accounts are read from Orca's profile, ~/Library/Application Support/orca unless
# ORCA_USER_DATA_PATH says otherwise: give the run its own, beside its Somtus home.
case ${ORCA_USER_DATA_PATH:-} in
/tmp/* | /private/tmp/*) ;;
*) export ORCA_USER_DATA_PATH="${RIWORK_HOME%/}-orca" ;;
esac
export RIWORK_NSH_FILES_FIXTURE=1 RIWORK_NSH_CHAT_FIXTURE=1
