// Exercise the production entry. The ping only orders the test's host events.
import "./App";
import { host } from "nsh";
host.on("app.fixture.ping", () => host.post("app.fixture.pong", {}));
