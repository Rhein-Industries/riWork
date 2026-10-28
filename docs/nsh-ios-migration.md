# iOS migration to NSH

The `nsh` branch now uses `NSHMobileRoot` as its application entry UI. The
packaged NSH 0.3.0 bundle in `apps/riwork-mobile` owns the host library,
multi-host and per-host dashboards, and project search/navigation/sorting.
This is the first runnable stage, not a completed rewrite of the chat and
terminal screens.

## Ownership

| Layer | Current owner |
| --- | --- |
| Host library, dashboards, projects | NSH TSX, native `NavStack`/`NavPage` |
| iOS window, scene lifecycle, runtime loading | `NSHMobileHost.swift` and the thin app entry |
| Display snapshots and explicit actions | `NSHMobileBridge.swift` over the existing `RemoteModel` |
| Pairing, encrypted transport, Keychain, reconnects | Existing Swift model and `RiWorkCore` |
| Terminal/chat, file previews, pairing and settings forms | Existing native views, presented by `NSHMobileSurfaces.swift` |

`mobile.snapshot` and `mobile.action` are the only host calls available to the
mobile bundle. Snapshots carry a schema and revision; observation publishes
changes without a JavaScript polling loop. Pairing keys and transport tokens
stay in Swift. Dashboard row IDs resolve to their original host and target;
approval responses still go through the existing fresh-request validation and
uncertain-delivery protection.

The root overview reuses `HostDashboardStore`. Its temporary connections stop
before the interactive model connects, while native screens are shown, and
when the scene becomes inactive. Terminal and chat screens continue owning
their current visibility, subscriptions, drafts, viewport and input lifecycles.

The NSH dashboard includes All, Needs you, Working and Projects filters, saved
read markers, decision coverage and truncation notices, activity history and
Haiku summary details. Project navigation retains folder grouping, persisted
sort choices and direct terminal creation. Older hosts fall back to their
available project inventory and explicitly report missing dashboard metadata.

## Phone appearance

iPhone keeps native controls, SF Symbols and system typography across the NSH
root and retained native screens. **Display → Color palette** defaults to
**Native**, which follows the phone's system appearance. It also offers the
12 fixed desktop palettes offline from the shared
[`crates/riwork-core/color-palettes.json`](../crates/riwork-core/color-palettes.json),
plus **Follow desktop** for the selected host's cached colors, including Ghostty.
Follow desktop falls back to Native until a palette is available. The choice is
saved locally on the phone; it changes colors without changing fonts, controls
or layout. Code and terminals remain monospace. iPad retains its existing
desktop-synchronized appearance, and both devices still receive the desktop
microphone preference independently of palette selection.

## Touch and native navigation

NSH pages use the iOS navigation stack, including interactive Back. NSH route
state reconciles with the host without immediately undoing a user's pop.
Controls have at least 44-point targets, wrapping text and pressed feedback.
Scroll views support refresh and keyboard dismissal; project search keeps
content clear of the keyboard and home indicator.

Project navigation supports hardware-keyboard movement and opening. The root
also connects the existing Clicks prefix shortcuts and shortcut guide through a
native responder beneath the NSH view, without placing an input layer over the
screen. Native screens keep their own responder ownership.

The outer UIKit navigation controller pushes the existing terminal/chat view
controllers. Their touch scrolling, text selection, tab gestures, IME input,
keyboard docking and dictation code is retained. File previews use the same
navigation stack through a small presentation hook. Native sheets preserve
pairing confirmation, QR scanning, project creation and decision flows.
Deep links wait for active sheets to finish before presenting their pairing
confirmation. iPad and iPhone continue using the existing platform-specific
terminal implementations.

These are implementation guarantees about which code owns interaction, not a
claim that all device gestures have been exercised. No automated tests were
run for this migration stage.

## Build and next boundaries

See `ios/README.md` for the release build. `prepare-nsh.py` requires matching
0.3.0 CLI/runtime versions, builds into a staging directory, and replaces the
generated bundle only after success. Xcode links the static native framework
and packages the UI; the installed app needs neither a CLI nor a checkout.
The bundle identifier and Keychain ownership remain `com.riwork.remote`.

The next migration boundaries are the chat presentation/composer and the
remaining forms. Desktop NSH screens currently use local `riwork/native`
services; mobile uses the remote Swift model. Sharing those screens requires
extracting their presentation components and supplying local/remote adapters.
The terminal remains a platform-native surface while its surrounding controls
can move into NSH. The old `RemoteRootView` is retained as a source reference;
the app entry no longer selects it.
