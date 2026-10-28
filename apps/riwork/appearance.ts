import { cleanup, colorScheme, effect, host, setColorScheme, signal } from "nsh";
import { Appearance, Settings, type AppearanceDto, type SettingsDto } from "riwork/native";
import type { HostParams } from "./model";

/** Native light, the context before the native owner's first delivery. Never a resolver. */
export const NATIVE_PALETTE = {
  bg: "#ffffff", panel: "#f5f5f7", panelActive: "#e8e8ed", divider: "#d2d2d7",
  cyan: "#000000", magenta: "#3a3a3c", gold: "#b34000", text: "#1d1d1f",
  muted: "#636366", focus: "#000000", working: "#b34000",
};

export interface InterfaceTheme {
  palette: typeof NATIVE_PALETTE;
  scale: number;
  family: string;
  monoFamily: string;
}

export const DEFAULT_THEME: InterfaceTheme = {
  palette: NATIVE_PALETTE, scale: 1, family: "system-ui", monoFamily: "Menlo",
};

export function space(base: number, scale: number): number {
  return Math.round(base * Math.max(scale, 1));
}

export function interfaceTheme(settings: SettingsDto, appearance: AppearanceDto, families: string[]): InterfaceTheme {
  const face = settings.text.face;
  return {
    palette: appearance.palette,
    scale: settings.text.scale,
    family: "system-ui",
    monoFamily: face === "system" ? "system-ui" : families.includes("SF Mono") ? "SF Mono" : "Menlo",
  };
}

/** One window's subscriptions; resolution is native, revision gaps are snapshots. */
export function useAppearance(params: HostParams) {
  const requestedSettings = signal<SettingsDto | null>(null);
  // Install one resolved pair: a new font/casing choice must never be drawn
  // beside the previous choice's palette while its native request is pending.
  const resolved = signal<{ settings: SettingsDto; appearance: AppearanceDto } | null>(null);
  const families = signal<string[]>([]);
  const notice = signal("");
  const ready = signal(false);
  let alive = true;
  let generation = 0;
  let settingsRev = -1;
  let appearanceRev = -1;
  if (params.appearance && params.appearance !== "system") setColorScheme(params.appearance);
  function error(reason: unknown) { if (alive) notice.set(String(reason)); }
  function installSettings(next: SettingsDto) {
    if (!alive) return;
    requestedSettings.set(next);
    if (next.error) notice.set(next.error);
  }
  function resolve() {
    const current = requestedSettings();
    const dark = colorScheme() === "dark";
    if (!current) return;
    const request = ++generation;
    void Appearance.resolve(current.theme, dark).then((next) => {
      if (!alive || request !== generation || next.selected !== current.theme) return;
      resolved.set({ settings: current, appearance: next });
      // Readiness is deliberately independent of later pair changes, so the
      // Workspace/DockArea owner remains mounted after its first correct paint.
      ready.set(true);
      if (next.error) notice.set(next.error);
    }).catch(error);
  }
  // Effects run only when their inputs change; there are no timers or frames.
  effect(resolve);
  const settingsSubscription = Settings.subscribe((event) => {
    if (event.rev <= settingsRev) return;
    settingsRev = event.rev;
    installSettings(event.data);
  });
  const appearanceSubscription = Appearance.subscribe((event) => {
    if (event.rev <= appearanceRev) return;
    appearanceRev = event.rev;
    // A process snapshot cannot pick this window's system appearance.
    resolve();
  });
  cleanup(host.on("appearance", (event) => {
    const value = event as { appearance: "light" | "dark" | "system" };
    setColorScheme(params.appearance && params.appearance !== "system" ? params.appearance : value.appearance);
    resolve();
  }));
  cleanup(host.on("command", (event) => {
    const command = (event as { id: string }).id;
    const action = command === "biggerText" ? "bigger" : command === "smallerText" ? "smaller" : command === "actualSize" ? "reset" : null;
    if (action) void Settings.changeTextSize(action).catch(error);
  }));
  void host.call("system.fonts", {}).then((value) => {
    if (alive) families.set((value as { families: string[] }).families);
  }).catch(() => { /* Menlo is the established fallback. */ });
  cleanup(() => { alive = false; generation++; settingsSubscription.close(); appearanceSubscription.close(); });
  return {
    settings: () => resolved()?.settings ?? null,
    ready,
    theme: () => {
      const current = resolved();
      return current ? interfaceTheme(current.settings, current.appearance, families()) : DEFAULT_THEME;
    },
    notice,
  };
}
