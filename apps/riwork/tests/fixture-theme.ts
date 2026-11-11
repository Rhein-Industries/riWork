import { colorScheme, effect, host, signal } from "nsh";
import { Appearance } from "riwork/native";
import { DEFAULT_THEME, type InterfaceTheme } from "../theme";

/** The palette a panel fixture draws in. For pictures, `host.params.theme` names one
 * (`scripts/pane-shot.sh FIXTURE OUT --params '{"theme":"paper_clay"}'`), which core resolves for
 * the window's light or dark appearance as the app does. A fixture app without the native crate
 * (dashboard/, schedules/) cannot resolve one: `host.params.palette` gives its colors instead.
 * Without either it is Native light, as the fixtures always drew. Reading a palette saves nothing. */
export function fixtureTheme(): () => InterfaceTheme {
  const params = (host.params ?? {}) as { theme?: string; palette?: Partial<InterfaceTheme["palette"]> };
  const theme = signal<InterfaceTheme>(params.palette ? { ...DEFAULT_THEME, palette: { ...DEFAULT_THEME.palette, ...params.palette } } : DEFAULT_THEME);
  const id = params.theme;
  if (id && !params.palette) effect(() => {
    const dark = colorScheme() === "dark";
    void Appearance.resolve(id, dark).then((value) => theme.set({ ...DEFAULT_THEME, palette: value.palette })).catch(() => { /* Native light stays. */ });
  });
  return theme;
}
