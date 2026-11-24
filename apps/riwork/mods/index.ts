import type { JSX } from "nsh";
import type { InterfaceTheme } from "../appearance";
import { minimalist } from "./minimalist/App";

/** A bundled mod contributes workspace chrome and chooses which of the
 * host's panes the workspace displays. The host owns session lifetimes. */
export interface WorkspaceMod {
  id: string;
  visibleTabs: ReadonlySet<string>;
  showStatusBar: boolean;
  Navigation: (props: WorkspaceNavigation) => JSX.Element;
}

export interface WorkspaceNavigation {
  theme: () => InterfaceTheme;
  active: (key: string) => boolean;
  open: (key: string) => void;
}

const bundled: WorkspaceMod[] = [minimalist];

export function findWorkspaceMod(id: string | null): WorkspaceMod | undefined {
  return bundled.find((mod) => mod.id === id);
}
