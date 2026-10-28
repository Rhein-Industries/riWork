import { nativeComponent } from "nsh";

const SystemSymbol = nativeComponent<{ name: string; color: string; size: number }>("riwork.mobile.symbol", {
  props: ["name", "color", "size"],
});

/** UIKit's immutable SF Symbol with view-local tint; it never takes touches. */
export function Symbol(props: { name: string; color: string; size?: number }) {
  return <SystemSymbol name={props.name} color={props.color} size={props.size ?? 20}
    pointerEvents="none" accessible={false}
    style={{ width: props.size ?? 20, height: props.size ?? 20, flexShrink: 0 }} />;
}
