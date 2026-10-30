/** Push-to-talk apart from keys, buttons and the microphone, so its rules can be tested: a press
 * held past `delay` starts dictation, and its release ends it; a shorter press is a tap and does
 * nothing; another key joining the press (a shortcut, an Option character) voids it. */
export interface HoldTimers { set(run: () => void, ms: number): unknown; clear(handle: unknown): void }

/** Long enough for ⌘C or ⌥G (@ on Swiss and German keyboards) to stay a chord. */
export const HOLD_DELAY = 250;

export const HOLD_KEY_LABELS: Record<string, string> = { right_command: "right ⌘", right_option: "right ⌥" };

const realTimers: HoldTimers = {
  set: (run, ms) => setTimeout(run, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export function createHold(options: {
  /** The hold is long enough: start dictating. False when it cannot (none is started). */
  begin: () => boolean;
  /** A started hold ended: `send` unless the window lost the keyboard mid-hold. */
  release: (send: boolean) => void;
  /** A started hold was voided: discard it. */
  abort: () => void;
  delay?: number;
  timers?: HoldTimers;
}) {
  const timers = options.timers ?? realTimers;
  let timer: unknown;
  let held = false;
  function clearTimer() {
    if (timer === undefined) return false;
    timers.clear(timer);
    timer = undefined;
    return true;
  }
  return {
    /** The key or button went down. */
    down() {
      if (held || timer !== undefined) return;
      timer = timers.set(() => { timer = undefined; held = options.begin(); }, options.delay ?? HOLD_DELAY);
    },
    /** It came up. True when that ended a hold, false for a tap. */
    up(send = true): boolean {
      if (clearTimer() || !held) return false;
      held = false;
      options.release(send);
      return true;
    },
    /** Another key joined it: no hold, and a started one is discarded. */
    cancel() {
      clearTimer();
      if (!held) return;
      held = false;
      options.abort();
    },
    holding: () => held,
  };
}

/** The host's push-to-talk edges: `pushToTalkDown:right_command`, `…Up:…`, `…Chord:…`. */
export function holdCommand(id: string): { edge: "down" | "up" | "chord"; key: string } | null {
  const match = /^pushToTalk(Down|Up|Chord):([a-z_]+)$/.exec(id);
  return match ? { edge: match[1].toLowerCase() as "down" | "up" | "chord", key: match[2] } : null;
}
