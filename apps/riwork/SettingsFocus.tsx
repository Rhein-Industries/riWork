import { View as NativeView, cleanup, context, signal, untracked, type ViewProps } from "nsh";
import { Button as NativeButton, Input as NativeInput, Switch as NativeSwitch, type ButtonProps as NativeButtonProps, type InputProps, type SwitchProps as NativeSwitchProps } from "nsh/base";

/** NSH focus events are direct, not bubbling. Keep the Settings controls in one
 * scope instead of guessing containment from a subset of form-field signals. */
export function settingsFocusScope() {
  const focused = signal<object | null>(null);
  return {
    containsFocus: () => focused() !== null,
    enter: (owner: object) => focused.set(owner),
    leave: (owner: object) => { if (untracked(focused) === owner) focused.set(null); },
  };
}
export const SettingsFocus = context<ReturnType<typeof settingsFocusScope> | null>(null);

interface FocusEvents { onFocus?: () => void; onBlur?: () => void }
// The pinned CLI's bundled declarations predate these opt-in vendor events.
type ButtonProps = NativeButtonProps & FocusEvents;
type SwitchProps = NativeSwitchProps & FocusEvents;
function tracked<T extends FocusEvents>(props: T): T {
  const scope = SettingsFocus();
  // Copy descriptors, not values: retain reactive getters and ref/child owners.
  const descriptors = Object.getOwnPropertyDescriptors(props);
  // The 0.3 host View call enumerates props before inserting children. Do not
  // let that props effect also evaluate the child getter and mount it twice.
  if (descriptors.children) descriptors.children.enumerable = false;
  if (!scope) return Object.defineProperties({}, descriptors) as T;
  const owner = {};
  cleanup(() => scope.leave(owner));
  return Object.defineProperties({}, { ...descriptors,
    onFocus: { enumerable: true, value: () => { scope.enter(owner); props.onFocus?.(); } },
    onBlur: { enumerable: true, value: () => { scope.leave(owner); props.onBlur?.(); } },
  }) as T;
}
export const View = (props: ViewProps) => NativeView(tracked(props));
export const Button = (props: ButtonProps) => NativeButton(tracked(props));
export const Input = (props: InputProps) => NativeInput(tracked(props));
export const Switch = (props: SwitchProps) => NativeSwitch(tracked(props));
