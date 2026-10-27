import { expect, open, test } from "nsh/test";

test("P1-NAT-04: a bridge folder mutation reaches two real engines after their first snapshot", async () => {
  const first = await open("./App.tsx");
  const second = await open("./App.tsx");
  try {
    await first.waitFor("Folders: 0");
    await second.waitFor("Folders: 0");
    expect(first.shows("Modules: 7")).toBe(true);
    await first.press("Add folder");
    await first.waitFor("Folders: 1");
    await second.waitFor("Folders: 1");
    expect(second.shows("Folders: 0")).toBe(false);
  } finally { first.dispose(); second.dispose(); }
});
