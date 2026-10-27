// Each real app window mounts normally; the first timer asks the process to
// flush and quit all its windows, including one opened by the fixture CLI.
import "./App";
import { scheduleLifecycle } from "./tests/HostLifecycle.fixture";

scheduleLifecycle("app.quit");
