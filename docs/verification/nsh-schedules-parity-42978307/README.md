# Schedules parity evidence: 42978307

New evidence only. Earlier archives and scratch receipts were checked against
`prior-evidence-sha256.json`; all 1,912 remained byte-identical.

`commands.jsonl` retains expanded argv, exit results, timings and original log
paths, including nested seed/check/test commands. Duplicate nested labels have
different disposable roots and separate numbered log captures. `results.json`
indexes lossless gzip captures with hashes of both stored and decoded bytes.
For example: `gzip -dc logs/01-fresh-compile.log.gz`. Failures are intentional
evidence: first fixture socket setup, then two UI focus regressions before fix.

`fresh-wire-audits.json` is extracted from the final dispatch audit run.
`ui-red.xml` and `ui-green.xml` retain independent JUnit outcomes.
`sources.json` pins local/pinned-crate source hashes; numbered excerpts are in
`source-evidence.txt.gz`. The maintained fixture and regression source is in
the same commit. Wrapper/driver snapshots describe isolation and execution.

Any final staging checks are appended as separate `final-checks*` files; the
initial command/result/log snapshot is never rewritten to append those checks.
See `../../nsh-schedules-parity-followup.md` for scope and explicit open parity.
