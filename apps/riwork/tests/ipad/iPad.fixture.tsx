/** The iPhone and iPad app (apps/riwork-mobile) drawn by the Mac's NSH host, for pictures of its screens:
 * `ipad_host.ts` plays the Swift host (relay.*, platform.*, nav.*, mobile.*) from sample data and is
 * imported first, so the app's first host calls reach it. Nothing reaches a device, a relay or a Mac.
 * The iOS-only native views (SF Symbols, the capture terminal, the chat) draw empty here.
 * `scripts/ipad-shot.sh` takes the usual pictures; `scripts/pane-shot.sh` takes any other. */
import "./ipad_host";
import "../../../riwork-mobile/App";
