#!/usr/bin/env python3
"""Verify the W3/panels merge with disposable app state and recorded commands."""
import argparse
from pathlib import Path

from phase1_integration import NSH, TSC, Verification


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--nsh", type=Path, default=NSH)
    parser.add_argument("--tsc", type=Path, default=TSC)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--stage", choices=("all", "rust", "nsh", "review"), default="all")
    args = parser.parse_args()
    args.mode = f"w3-merge-{args.stage}"
    v = Verification(args)
    try:
        v.toolchain()
        if args.stage in ("all", "rust"):
            v.command(v.cargo("test", "-p", "riwork-nsh-native", "-p", "riwork-nsh-workspace"),
                      "merged-rust-tests")
        if args.stage in ("all", "nsh"):
            v.nsh()
        if args.stage in ("all", "review"):
            v.command(v.cargo("test", "-p", "riwork-desktop", "--bin", "riwork"),
                      "merged-host-tests")
            app = "crates/riwork-nsh-native/tests/app"
            v.command([args.nsh, "check", app, "--tsc", args.tsc], "native-probe-check")
            v.command([args.nsh, "test", app, "--junit", v.logdir / "native-probe.xml"],
                      "native-probe-test")
            v.assert_no_sessions(Path(v.env["RIWORK_HOME"]))
            v.command(["cargo", "fmt", "-p", "riwork-nsh-native", "-p",
                       "riwork-nsh-native-test-support", "-p", "riwork-nsh-workspace",
                       "-p", "riwork-desktop", "--check"], "merged-format")
            v.command(["python3", "crates/riwork-desktop/verification/phase1_w3_audit.py"],
                      "merged-source-audit")
            v.command(["git", "diff", "--check"], "merged-whitespace")
        v.manifest["status"] = "complete"
    except Exception as error:
        v.manifest.update(status="failed", error=str(error))
        raise
    finally:
        v.save()


if __name__ == "__main__":
    main()
