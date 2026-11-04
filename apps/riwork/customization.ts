import { cleanup, host, signal } from "nsh";
import { Customization } from "riwork/native";
import { DEFAULT_CUSTOMIZATION, type CustomizationSnapshot } from "./customization_model";

export interface CustomizationService {
  snapshot(): Promise<unknown>;
  subscribe(listener: (event: { rev: number; data: unknown }) => void): { close(): void };
  select(name: string): Promise<unknown>;
  reset(): Promise<unknown>;
  initializeFiles(): Promise<unknown>;
  reveal(): Promise<unknown>;
}

// The native bridge carries the validated, versioned document as JSON.
const nativeCustomization = Customization as unknown as CustomizationService;
export function useCustomization(service: CustomizationService = nativeCustomization) {
  const initial = service === nativeCustomization ? (host.params as unknown as { customization?: CustomizationSnapshot }).customization : undefined;
  const state = signal<CustomizationSnapshot>(initial ?? DEFAULT_CUSTOMIZATION);
  const failure = signal("");
  let alive = true, revision = -1;
  const fail = (error: unknown) => { if (alive) failure.set(String(error)); };
  const subscription = service.subscribe((event) => {
    if (!alive || event.rev <= revision) return;
    revision = event.rev;
    state.set(event.data as CustomizationSnapshot);
    failure.set("");
  });
  void service.snapshot().then((next) => { if (alive && revision < 0) state.set(next as CustomizationSnapshot); }).catch(fail);
  cleanup(() => { alive = false; subscription.close(); });
  async function run(action: () => Promise<unknown>) {
    try { await action(); if (alive) failure.set(""); } catch (error) { fail(error); }
  }
  return {
    state, error: () => failure() || state().error || "",
    select: (name: string) => run(() => service.select(name)),
    reset: () => run(() => service.reset()),
    edit: () => run(async () => { await service.initializeFiles(); await service.reveal(); }),
  };
}
