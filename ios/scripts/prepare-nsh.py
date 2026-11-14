#!/usr/bin/python3
"""Build the pinned mobile UI and locate its matching native iOS runtime."""
from pathlib import Path
import os
import plistlib
import re
import shutil
import subprocess
import tempfile


IOS = Path(__file__).resolve().parent.parent
ROOT = IOS.parent
VERSION = "0.5.1"
remembered = IOS / ".nsh-cli"
candidate = os.environ.get("NSH_CLI")
if not candidate and remembered.exists():
    candidate = remembered.read_text().strip()
if not candidate:
    candidate = shutil.which("nsh")
if not candidate:
    raise SystemExit(f"Set NSH_CLI to the absolute path of the NSH {VERSION} executable, then run ios/scripts/prepare-nsh.py.")
cli = Path(candidate).expanduser().resolve()
if not cli.is_file() or not os.access(cli, os.X_OK):
    raise SystemExit(f"NSH_CLI is not executable: {cli}")
version = subprocess.check_output([str(cli), "--version"], text=True).strip()
if not re.match(r"^nsh " + re.escape(VERSION) + r"(?:\s|$)", version):
    raise SystemExit(f"Somtus iOS requires NSH {VERSION}; selected {version}. Set NSH_CLI to the matching release.")
# Since NSH 0.5 the CLI archive has no `embed` folder: the iOS embedding SDK is the release's own asset,
# nsh-embed-ios.zip. Unpacked beside the CLI's `bin` it is found here; anywhere else, NSH_IOS_SDK names it.
beside_cli = cli.parent.parent / "embed/ios/NativeShell.xcframework"
sdk = Path(os.environ.get("NSH_IOS_SDK") or beside_cli).expanduser().resolve()
obtain = (f"Download nsh-embed-ios.zip and nsh-embed-ios.zip.sha256 from "
          f"https://github.com/longbridge/nsh/releases/tag/v{VERSION}, check the archive against the checksum, and put its "
          f"NativeShell.xcframework at {beside_cli}, or set NSH_IOS_SDK to where it is.")
if not sdk.is_dir():
    raise SystemExit(f"The NSH {VERSION} iOS SDK is not at {sdk}. {obtain}")
for platform in ("ios-arm64", "ios-arm64-simulator"):
    if not (sdk / platform / "NativeShell.framework/NativeShell").is_file():
        raise SystemExit(f"The NSH {VERSION} iOS SDK is missing {platform}: {sdk}. {obtain}")
    with (sdk / platform / "NativeShell.framework/Info.plist").open("rb") as file:
        sdk_version = plistlib.load(file).get("CFBundleShortVersionString")
    if sdk_version != VERSION:
        raise SystemExit(f"Somtus iOS requires the NSH {VERSION} runtime; {platform} of {sdk} contains {sdk_version}. {obtain}")

generated = IOS / "Generated"
generated.mkdir(exist_ok=True)
stage = Path(tempfile.mkdtemp(prefix=".nsh-stage-", dir=generated))
try:
    args = [str(cli), "build", str(ROOT / "apps/riwork-mobile"), "--out", str(stage / "nsh")]
    if os.environ.get("CONFIGURATION", "Release") != "Debug":
        args.append("--release")
    subprocess.run(args, cwd=ROOT, check=True)
    if not (stage / "nsh/bundle.nshc").is_file():
        raise SystemExit("NSH did not produce the mobile bytecode bundle; refusing to package an older UI.")
    # The shared screens (apps/riwork) reach data through ports; the Mac's native module is not served on
    # iOS, and a bundle that imports it fails as it loads. `nsh check apps/riwork-mobile` catches the types;
    # this catches what the bundler kept.
    bundle_js = stage / "nsh/bundle.js"
    if not bundle_js.is_file():
        raise SystemExit("NSH did not produce the mobile bundle.js; refusing to package a UI that cannot be checked.")
    if re.search(r"""["']riwork/native["']""", bundle_js.read_text(encoding="utf-8", errors="replace")):
        raise SystemExit("The mobile bundle imports riwork/native, the Mac's native module, which iOS does not have. "
                         "A file apps/riwork-mobile reaches imports Mac-only code; give that screen its data through "
                         "apps/riwork/client ports instead.")
    vendor = IOS / "Vendor"
    vendor.mkdir(exist_ok=True)
    link = vendor / "NativeShell.xcframework"
    if link.exists() and not link.is_symlink():
        raise SystemExit(f"{link} is not a generated symlink; move it aside before preparing the pinned SDK.")
    staged_link = stage / "NativeShell.xcframework"
    staged_link.symlink_to(sdk, target_is_directory=True)
    os.replace(staged_link, link)
    output = generated / "nsh"
    if output.exists():
        output.rename(stage / "previous-nsh")
    try:
        (stage / "nsh").rename(output)
    except BaseException:
        if (stage / "previous-nsh").exists():
            (stage / "previous-nsh").rename(output)
        raise
    remembered.write_text(str(cli) + "\n")
finally:
    shutil.rmtree(stage)
print(f"Prepared Somtus mobile UI with NSH {VERSION}.")
