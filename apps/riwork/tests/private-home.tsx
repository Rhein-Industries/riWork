/** Test support: fixtures that save RiWork settings, layouts or projects run only in a
 * private /tmp RiWork home, never in the person's own. scripts/pane-test.sh and
 * scripts/pane-shot.sh give every run one (scripts/pane-env.sh); a verification runner
 * that brings its own /tmp home sets RIWORK_NSH_FILES_FIXTURE=1 too. The check is the one
 * Files.fixtureDirectory already makes: a debug build, RIWORK_NSH_FILES_FIXTURE=1 and a
 * RIWORK_HOME under /tmp. */
import { Text, View, mount, signal, type Component } from "nsh";
import { Files } from "riwork/native";

/** The refusal names what this run lacks, from Files.fixtureDirectory's reason. */
function privateHomeRefusal(reason: unknown): string {
  const text = String(reason);
  const missing = /Files fixtures are off/.test(text)
    ? "RIWORK_NSH_FILES_FIXTURE=1 is not set (or this is not a debug build)"
    : /isolated \/tmp RIWORK_HOME/.test(text) ? "RIWORK_HOME is not under /tmp" : text;
  return "Refused: this fixture saves RiWork settings, layouts or projects, so it needs a private "
    + `fixture home (a debug build, RIWORK_NSH_FILES_FIXTURE=1 and a RIWORK_HOME under /tmp). Missing: ${missing}. `
    + "Run its tests with scripts/pane-test.sh NAME (pictures: scripts/pane-shot.sh), never with plain `nsh test`.";
}

/** Files.fixtureDirectory, whose refusal says what is missing and how to run the fixture instead. */
export async function privateFixtureDirectory(name: string): Promise<string> {
  try {
    return await Files.fixtureDirectory(name);
  } catch (reason) {
    const message = privateHomeRefusal(reason);
    console.error(message);
    throw new Error(message);
  }
}

let checked: Promise<void> | undefined;
/** Resolves once RiWork's home is a private /tmp fixture home, else rejects with the
 * refusal. A fixture awaits it before its first Settings, Store or Layouts write. */
export function requirePrivateHome(): Promise<void> {
  return checked ??= privateFixtureDirectory("private-home").then(() => undefined);
}

/** `mount(Fixture)` for a fixture that writes from its first render: nothing of it runs
 * until the home is private, and a refusal is all the screen shows. */
export function mountInPrivateHome(Fixture: Component) {
  mount(() => {
    const allowed = signal(false), refusal = signal("");
    void requirePrivateHome().then(() => allowed.set(true), (reason: unknown) => refusal.set(String(reason)));
    return <View style={{ flex: 1 }}>{allowed() ? <Fixture /> : <Text testID="private-home-refusal">{refusal()}</Text>}</View>;
  });
}
