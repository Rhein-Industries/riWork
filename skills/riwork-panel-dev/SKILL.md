---
name: riwork-panel-dev
description: Improve one panel or pane of the RiWork app (apps/riwork) by looking at it - take a screenshot from its fixture, critique the design, then the logic, make focused fixes, and show before and after for the person to approve or send back. Use when asked to review, polish, critique or redesign a RiWork panel, pane, tab or view, or to "look at" how one renders.
---

# RiWork panel dev

A loop for one panel: **capture → design critique → logic critique → improve → show → wait**. The
person decides after every round: they approve it, or tell you what to change and you go round
again. Never move on from "show" by yourself.

Work in the RiWork repository (a worktree of it when other work is going on). Keep this review's
files in `.pane-shot/<panel>/round-N/` at the repository root; git ignores `.pane-shot/`. Do not
write new tests unless the person asks. Do not commit.

## Capture

1. Find the panel's component under `apps/riwork` (`Workspace.tsx` mounts each panel), its
   fixture (a `*.fixture.tsx` that mounts it with in-memory data) and the names of its existing
   tests (the ids in its `*.test.tsx`, such as `FLOWS-UI`). If it has no fixture, write a small one
   beside it as `flows/Flows.fixture.tsx` does: realistic data, an in-memory API, no native calls.
2. Take the picture with `scripts/pane-shot.sh`, the same command every round so pictures compare:

   ```sh
   scripts/pane-shot.sh apps/riwork/flows/Flows.fixture.tsx .pane-shot/flows/round-1/before.png \
     --width 760 --height 900 --wait id:flow:code-review \
     --step 'await app.press("id:flow-run");'
   ```

   It builds the app and draws the fixture in the NSH macOS host with a hidden window: nothing
   shows and the focus stays where it is (`PANE_SHOT_VISIBLE=1` shows it). Pick the panel's usual
   size, `--dark` when dark mode matters, and `--step` lines (test code: `app.press`, `app.type`,
   `app.host.send`, ...) that bring it to its most telling state. The first run in a fresh
   worktree builds for about a minute; later ones take 15–30 s. Write the command you used to
   `.pane-shot/<panel>/shot.txt`.
3. Look at the picture yourself (read the PNG) before you write anything about it.

## Design critique

From the picture only, as a product designer: what the eye finds first and whether it should,
hierarchy, spacing and alignment, typography, color and contrast, density, affordances (what
looks pressable or disabled), wording, the empty, busy and error states it shows, and consistency
with RiWork's other panels. Write `design.md` in the round's folder: findings ranked by impact,
each with what you see, why it matters, and the change you propose.

## Logic critique

From the code, not the picture: state that can go stale or arrive out of order (late async
results, subscriptions left open, effects that track too much), loading, empty, error and busy
states and whether each can be reached and is shown, controls enabled or disabled when they should
not be, what long or missing text does, keyboard and focus, accessibility labels and test ids.
Confirm each finding in the code before you write it. Write `logic.md`, ranked like `design.md`.

## Improve

Fix the findings that matter most from both lists (and, from round 2 on, the person's note first):
small, focused changes in the panel's own files; leave unrelated code alone. Then check:

```sh
cd apps/riwork && "$NSH" check                       # the app type-checks
"$NSH" test apps/riwork --filter NAME                # from the repository root, each existing test name
```

`NSH` is `$NSH_CLI`, else `~/.local/share/riwork/toolchains/nsh-0.3*/bin/nsh`. Fix what fails.

## Show, then wait

Take `after.png` with the same command, compare it with `before.png`, and if a fix does not show
or broke something, improve again first. Write `summary.md`: what changed and why, what you left
for later. Then reply to the person with:

- the absolute paths of `before.png` and `after.png` (read both so they are in your transcript),
- three to six lines of what changed, and the files you touched,
- the question: **approve, or what should change?**

Stop there. On approval, you are done: list the changed files. On a note, start the next round:
copy this round's `after.png` to `round-N+1/before.png`, put the note at the top of the next
`design.md`, and go through critique, improve and show again.

## Handing it to another agent

An orchestrator can give a panel to a new chat in its own worktree:

```sh
CHAT=$(riwork chat new --provider claude --worktree WT --mode full --title "Panel: Schedules" --json | jq -r .id)
riwork chat send "$CHAT" "Use the riwork-panel-dev skill on the Schedules panel."
```

The chat replies with before and after when a round is ready; the person answers there (or tells
the orchestrator, which passes it on with `riwork chat send`).
