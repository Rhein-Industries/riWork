# Automations verification artifacts

All desktop screenshots were captured using RiWork's Cua.ai Driver MCP. No real provider prompt was submitted. These are disposable fixture windows, not production RiWork sessions.

- `empty.png`: verified empty Automations panel, Cmd+Shift+S discovery and guidance (fixture before the final capability/shell corrections; its visual layout is unchanged).
- `editor.png`: new project chat defaults, Codex/Supervised and title/prompt before optional settings (same preceding fixture).
- `narrow-editor.png`: earlier 640×900 logical editor with wrapping/scrolling; captured before title/prompt were moved above optional settings. It is not proof of the final narrow editor.
- `edited-candidate.png`: final candidate's seeded edit form with title/prompt/options and future time visible. No changed title/prompt is claimed from unsuccessful typing acknowledgements.
- `results.png`: final candidate's paused inert result, last/next, Open created chat and Project chats.
- `open-created-chat.png`: final capture remained on the previously painted panel frame with Project chats keyboard focus. Treat it as focus/paint evidence, not a screenshot of the opened ChatView.
- `fixture-receipt.json`: semantic receipt from the final candidate. Saved layout includes and selects chat `ebd0815a-696d-4bc5-a907-9bac5e4da60a`; ledger revision 3 is paused; seeded stopped chat contains no model prompt. This is the proof that the app opened the result tab.
- `cleanup-receipt.json`: exact fixture GUI/host shutdown and production PID 78989 preserved.
- `initial.png`, `current.png`, `menu.png`: preceding fixture source captures retained without claiming final-candidate input verification.
- `test-logs/`: passing final engine, interfaces, host, CLI/MCP, native label and native build logs.
- `test-logs/followup-*.log`: focused parent-review corrections: eight interface tests, 12 CLI/MCP contract tests and a compile check. Only the endings of the five original test logs were normalized; the scheduler and GUI suites were not repeated.
- `test-logs/directory-*.log`: live project-directory fix: four targeted regressions, 47 scheduler tests (two existing opt-in/helper ignores), seven input tests and a desktop compile check. Fixtures verify real process directory changes and zero claim/input on rejection; root/subdirectory dispatch and legacy/explicit project orchestrator behavior pass.
- `test-logs/independent-directory-drift-reproducer.log`: preserved independent evidence of the defect on a38ea39, where outside-project input was submitted. Its test intentionally confirmed the old defect; this is prior-failure evidence, not a passing acceptance result for the corrected candidate.
- `directory-receipt.json`: exact corrected code candidate, source hashes, isolated test counts and committed base-diff check. No new GUI or production changes were made.

Cua transport acknowledgements alone are not accepted as semantic proof. Final title/prompt replacement, mouse hit reliability, complete narrow keyboard navigation, GUI pause/resume/delete and revision-conflict flows remain unverified. Shared-service/CLI/MCP lifecycle tests pass; the Cua editor save is proven by revision 2→3 while retaining paused=true. The script initially paused the seeded schedule via the CLI; revision 3 does not establish a GUI resume/re-pause cycle.

Driver 0.30.4 reported Accessibility and Screen Recording granted. Exact captures worked, while off-Space AX geometry/input and session escalation sometimes refused or lagged; no alternate provider was used. Cua ownership is ended. Only the unique fixture bundles and their processes were removed; the paused fixture home and these artifacts are retained.
