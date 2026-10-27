#!/bin/zsh
# usage: nsh-run.sh <label>   (from the worktree root)
set -u
source /private/tmp/claude-501/-Users-dominik-orca-projects-riWork-nsh-p1-consolidate/9a3154ae-fe47-4e8d-9a82-0eb37e7e6df5/scratchpad/c12f/env.sh
label=$1
run=$RW_SHORT/nsh-$label
rm -rf $run; mkdir -p $run/tmux
out=$C12F/logs/$label
mkdir -p $out
cargo run --locked --offline -q -p riwork-nsh-workspace --example seed_fixture -- $run/home > $out/seed.log 2>&1; echo "seed exit $?" | tee -a $out/seed.log
export RIWORK_HOME=$run/home TMUX_TMPDIR=$run/tmux
"$NSH_CLI" check apps/riwork --tsc "$NSH_TSC" > $out/nsh-check.log 2>&1; echo "check exit $?" | tee -a $out/nsh-check.log
"$NSH_CLI" test apps/riwork --junit $out/nsh.xml > $out/nsh-test.log 2>&1; echo "test exit $?" | tee -a $out/nsh-test.log
echo "tmux entries:"; find $run/tmux -mindepth 1 ! -type d | tee $out/tmux-files.txt
grep -E '^(✓|✗) ' $out/nsh-test.log | cut -c1-2 | sort | uniq -c
grep -E '^✗ ' $out/nsh-test.log
tail -3 $out/nsh-test.log
