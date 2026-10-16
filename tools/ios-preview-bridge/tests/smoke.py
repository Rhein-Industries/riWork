#!/usr/bin/env python3
"""Real-device transport proof; creates and deletes only its own new Simulator.

Run with /usr/bin/python3 tools/ios-preview-bridge/tests/smoke.py.
Requires full Xcode and the requested installed Simulator runtime.
No Simulator.app, Mac screen capture, or Mac input injection is used.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / "tools/ios-preview-bridge"


def run(*arguments, **kwargs):
    return subprocess.check_output(arguments, text=True, **kwargs).strip()


def until(condition, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("Timed out waiting for real Simulator state")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", default="com.apple.CoreSimulator.SimRuntime.iOS-26-0")
    parser.add_argument("--device-type", default="com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro")
    parser.add_argument("--artifacts", type=Path)
    args = parser.parse_args()
    simulator = None
    child = None
    connection = None
    with tempfile.TemporaryDirectory(prefix="riwork-ios-proof-", dir="/tmp") as temporary:
        temporary = Path(temporary)
        try:
            helper = temporary / "bridge"
            subprocess.run(["xcrun", "clang", "-fobjc-arc", "-Wall", "-Wextra", "-O2", "-framework", "Foundation", "-framework", "CoreGraphics", "-framework", "ImageIO", "-framework", "IOSurface", str(SOURCE / "main.m"), "-o", str(helper)], check=True)
            simulator = run("xcrun", "simctl", "create", "RiWork-Bridge-Proof-" + uuid.uuid4().hex[:8], args.device_type, args.runtime)
            run("xcrun", "simctl", "boot", simulator)
            run("xcrun", "simctl", "bootstatus", simulator, "-b")
            subprocess.run(["xcodebuild", "-project", str(SOURCE / "tests/RiWorkPreviewProof.xcodeproj"), "-scheme", "RiWorkPreviewProof", "-configuration", "Debug", "-sdk", "iphonesimulator", "-destination", "id=" + simulator, "-derivedDataPath", str(temporary / "derived"), "CODE_SIGNING_ALLOWED=NO", "build", "-quiet"], check=True)
            app = temporary / "derived/Build/Products/Debug-iphonesimulator/RiWorkPreviewProof.app"
            run("xcrun", "simctl", "install", simulator, str(app))
            run("xcrun", "simctl", "launch", simulator, "dev.riwork.ios-stream-proof")
            container = Path(run("xcrun", "simctl", "get_app_container", simulator, "dev.riwork.ios-stream-proof", "data"))
            proof_path = container / "Documents/proof.json"

            def proof():
                try:
                    return json.loads(proof_path.read_text())
                except (OSError, json.JSONDecodeError):
                    return {}

            def state(predicate):
                return until(lambda: (value if predicate(value) else None) if (value := proof()) else None)

            socket_path = temporary / "preview.sock"
            server = socket.socket(socket.AF_UNIX)
            server.bind(str(socket_path))
            server.listen(1)
            server.settimeout(15)
            token = uuid.uuid4().hex
            child = subprocess.Popen([str(helper), "--udid", simulator], env=dict(os.environ, RIWORK_PREVIEW_SOCKET=str(socket_path), RIWORK_PREVIEW_TOKEN=token), stderr=subprocess.PIPE, text=True)
            connection, _ = server.accept()
            reader = connection.makefile("rb")
            hello = json.loads(reader.readline())
            assert hello == {"type": "hello", "pid": child.pid, "token": token, "protocol": 1}, hello
            frames = queue.Queue(maxsize=4)
            failure = []

            def receive():
                try:
                    for line in reader:
                        message = json.loads(line)
                        if message["type"] == "error":
                            failure.append(message["message"])
                            return
                        assert message["type"] == "frame", message
                        if frames.full():
                            frames.get_nowait()
                        frames.put_nowait(message)
                except Exception as error:
                    failure.append(str(error))

            threading.Thread(target=receive, daemon=True).start()

            def send(message):
                connection.sendall(json.dumps(message).encode() + b"\n")

            def input(kind, **fields):
                send({"type": "input", "input": dict(kind=kind, **fields)})

            def frame(timeout=10):
                try:
                    result = frames.get(timeout=timeout)
                except queue.Empty:
                    raise AssertionError("No native Simulator frame: " + repr(failure))
                png = base64.b64decode(result.pop("png"), validate=True)
                assert png.startswith(b"\x89PNG\r\n\x1a\n"), "Not an actual PNG frame"
                assert 0 < result["width"] <= 2048 and 0 < result["height"] <= 2048
                return dict(result, png=png, digest=hashlib.sha256(png).hexdigest())

            send({"type": "ready", "protocol": 1})
            before = frame()
            width, height = before["width"], before["height"]
            input("click", x=width * .4, y=height * .4, button="left")
            tapped = state(lambda value: value["taps"] >= 1 and value["ends"] >= 1)
            assert abs(tapped["x"] - tapped["width"] * .4) < 3, tapped
            assert abs(tapped["y"] - tapped["height"] * .4) < 3, tapped
            after_tap = until(lambda: (value if value["digest"] != before["digest"] else None) if (value := frame()) else None)
            input("pointer_down", x=width * .35, y=height * .55, button="left")
            for step in range(1, 13):
                time.sleep(.025)
                input("pointer_move", x=width * (.35 + .25 * step / 12), y=height * (.55 + .15 * step / 12))
            input("pointer_up", x=width * .6, y=height * .7, button="left")
            dragged = state(lambda value: value["moves"] >= 4 and value["ends"] >= 2)
            assert abs(dragged["x"] - dragged["width"] * .6) < 3, dragged
            assert abs(dragged["y"] - dragged["height"] * .7) < 3, dragged
            after_drag = until(lambda: (value if value["digest"] != after_tap["digest"] else None) if (value := frame()) else None)
            input("pointer_down", x=width * .5, y=height * .5, button="left")
            state(lambda value: value["taps"] >= 3)
            send({"type": "visibility", "visible": False})
            released = state(lambda value: value["ends"] >= 3)
            time.sleep(.2)
            while not frames.empty():
                frames.get_nowait()
            time.sleep(.3)
            assert frames.empty(), "Invisible preview continued streaming"
            send({"type": "visibility", "visible": True})
            resumed = frame()
            send({"type": "visibility", "visible": False})
            time.sleep(.2)
            while not frames.empty():
                frames.get_nowait()
            send({"type": "visibility", "visible": True})
            repeated = frame()
            assert repeated["digest"] == resumed["digest"], "Expected forced unchanged frame after resume"
            input("pointer_down", x=width * .45, y=height * .45, button="left")
            state(lambda value: value["taps"] >= 4)
            time.sleep(.2)
            while not frames.empty():
                frames.get_nowait()
            input("refresh")
            refreshed = frame()
            assert proof()["ends"] == 3, "Frame refresh released a held finger"
            input("pointer_up", x=width * .45, y=height * .45, button="left")
            state(lambda value: value["ends"] >= 4)
            input("scroll", x=width * .5, y=height * .5, delta_x=0, delta_y=-height * .2)
            scrolled = state(lambda value: value["ends"] >= 5 and value["moves"] >= 16)
            assert abs(scrolled["y"] - scrolled["height"] * .3) < 3, scrolled
            input("key", key="home")

            def in_background():
                try:
                    return json.loads((container / "Documents/activity.json").read_text())["background"]
                except (OSError, json.JSONDecodeError):
                    return False

            until(in_background)
            send({"type": "shutdown"})
            child.wait(timeout=5)
            stderr = child.stderr.read()
            assert child.returncode == 0 and not failure, (child.returncode, stderr, failure)
            if args.artifacts:
                args.artifacts.mkdir(parents=True, exist_ok=True)
                for name, value in [("before", before), ("after-tap", after_tap), ("after-drag", after_drag), ("resumed", resumed)]:
                    (args.artifacts / (name + ".png")).write_bytes(value["png"])
                (args.artifacts / "proof.json").write_text(json.dumps(released, indent=2))
            print(json.dumps({"passed": True, "runtime": args.runtime, "frame": [width, height], "tapped": tapped, "dragged": dragged, "released_on_pause": released, "resume_forced_unchanged_frame": True, "refresh_preserved_held_touch": True, "scroll": scrolled, "home_backgrounded_app": True, "bridge": stderr.strip()}, indent=2))
        finally:
            if child and child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            if connection:
                connection.close()
            if simulator:
                subprocess.run(["xcrun", "simctl", "shutdown", simulator], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                subprocess.run(["xcrun", "simctl", "delete", simulator], check=True)


if __name__ == "__main__":
    main()
