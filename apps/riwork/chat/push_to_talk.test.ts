import { expect, test } from "nsh/test";
import { createHold, holdCommand, type HoldTimers } from "./push_to_talk";

function fakeTimers() {
  let next = 1;
  const pending = new Map<number, () => void>();
  const timers: HoldTimers = {
    set: (run) => { const id = next++; pending.set(id, run); return id; },
    clear: (handle) => { pending.delete(handle as number); },
  };
  const elapse = () => { const runs = [...pending.values()]; pending.clear(); for (const run of runs) run(); };
  return { timers, elapse, pending };
}

function harness(canBegin = true) {
  const { timers, elapse, pending } = fakeTimers();
  const log: string[] = [];
  const hold = createHold({
    begin: () => { log.push("begin"); return canBegin; },
    release: (send) => log.push(send ? "release+send" : "release"),
    abort: () => log.push("abort"),
    timers,
  });
  return { hold, log, elapse, pending };
}

test("push-to-talk: a held press dictates until it is released, then sends", () => {
  const { hold, log, elapse } = harness();
  hold.down();
  expect(log).toEqual([]);
  elapse();
  expect(hold.holding()).toBe(true);
  hold.down(); // auto-repeat or a second edge changes nothing
  expect(hold.up()).toBe(true);
  expect(log).toEqual(["begin", "release+send"]);
  expect(hold.holding()).toBe(false);
});

test("push-to-talk: a tap is not push-to-talk", () => {
  const { hold, log, pending } = harness();
  hold.down();
  expect(hold.up()).toBe(false);
  expect(pending.size).toBe(0);
  expect(log).toEqual([]);
});

test("push-to-talk: a key joining the press voids it, before or after dictation began", () => {
  const early = harness();
  early.hold.down();
  early.hold.cancel();
  early.elapse();
  expect(early.log).toEqual([]);
  expect(early.hold.up()).toBe(false);

  const late = harness();
  late.hold.down();
  late.elapse();
  late.hold.cancel();
  expect(late.log).toEqual(["begin", "abort"]);
  expect(late.hold.up()).toBe(false);
});

test("push-to-talk: losing the keyboard ends a hold without sending, and a hold that could not begin is nothing", () => {
  const lost = harness();
  lost.hold.down();
  lost.elapse();
  expect(lost.hold.up(false)).toBe(true);
  expect(lost.log).toEqual(["begin", "release"]);

  const refused = harness(false);
  refused.hold.down();
  refused.elapse();
  expect(refused.hold.holding()).toBe(false);
  expect(refused.hold.up()).toBe(false);
  expect(refused.log).toEqual(["begin"]);
});

test("push-to-talk: host commands name the edge and the key", () => {
  expect(holdCommand("pushToTalkDown:right_command")).toEqual({ edge: "down", key: "right_command" });
  expect(holdCommand("pushToTalkChord:right_option")).toEqual({ edge: "chord", key: "right_option" });
  expect(holdCommand("pushToTalkUp:")).toBeNull();
  expect(holdCommand("newChat")).toBeNull();
});
