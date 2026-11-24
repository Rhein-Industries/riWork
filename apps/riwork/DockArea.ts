import { DockArea as NativeDockArea, type DockAreaProps } from "nsh/base";
import type { JSX } from "nsh";

// The pinned local nsh-base patch exposes these props at runtime. The CLI ships
// its base declarations independently, so describe cannot regenerate them.
// This is the same native component value, preserving its panel mount cache.
interface WorkspaceDockAreaProps extends DockAreaProps {
  dividerSize?: number;
  fixedPanels?: string[];
  draggableTabBars?: number[];
  // Every handler takes one object since NSH 0.4. The vendored event has to be
  // `#[args(dragging)] DragChange(bool)`: left bare, its field would be `value`.
  onDragChange?: (event: { dragging: boolean }) => void;
  renderTabBarPrefix?: (groupId: number) => JSX.Element;
  renderTabBarSuffix?: (groupId: number) => JSX.Element;
}

export const DockArea = NativeDockArea as (props: WorkspaceDockAreaProps) => JSX.Element;
