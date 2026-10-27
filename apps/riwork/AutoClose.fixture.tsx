// Exercise the actual App/LayoutSession through the host's close/flush path.
import "./App";
import { scheduleLifecycle } from "./tests/HostLifecycle.fixture";

scheduleLifecycle("window.close");
