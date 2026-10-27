#!/usr/bin/env python3
"""Prepare only the pinned libghostty C library in a private worktree cache.

This does not run Cargo, build gpui-libghostty, install a toolchain, or write a
system/global cache. The distribution is used solely for its patched C/Zig
source closure. See ../BUILD.md and ../ghostty-pin.json.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
PIN = json.loads((ROOT / "ghostty-pin.json").read_text())
ARTIFACTS = ("lib/libghostty-internal.a", "include/ghostty.h")


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def require_equal(actual, expected, label):
    if actual != expected:
        raise RuntimeError(f"{label}: expected {expected}, got {actual}")


def source_manifest(directory):
    digest, count = hashlib.sha256(), 0
    for path in sorted(Path(directory).rglob("*")):
        if path.is_symlink():
            raise RuntimeError(f"Source symlink is not allowed: {path}")
        if not path.is_file():
            continue
        for data in (path.relative_to(directory).as_posix().encode(), path.read_bytes()):
            digest.update(len(data).to_bytes(8, "little"))
            digest.update(data)
        count += 1
    return digest.hexdigest(), count


def verify_source(directory):
    digest, count = source_manifest(directory)
    require_equal(digest, PIN["source_manifest"]["sha256"], "Patched Ghostty source hash")
    require_equal(count, PIN["source_manifest"]["file_count"], "Source file count")
    require_equal(sha256(directory / "VENDOR.md"), PIN["vendor_patch_document_sha256"], "Vendor patch declaration")


def prepare_source(cache, source_archive=None):
    source = cache / "source"
    if source.exists():
        verify_source(source)
        return source
    archive = cache / "ghostty-source.crate"
    if source_archive:
        require_equal(sha256(source_archive), PIN["source_distribution"]["sha256"], "Source distribution")
        shutil.copy2(source_archive, archive)
    elif not archive.exists():
        temporary = archive.with_suffix(".download")
        with urllib.request.urlopen(PIN["source_distribution"]["url"], timeout=60) as response:
            with temporary.open("wb") as destination:
                shutil.copyfileobj(response, destination)
        require_equal(sha256(temporary), PIN["source_distribution"]["sha256"], "Downloaded source distribution")
        temporary.replace(archive)
    require_equal(sha256(archive), PIN["source_distribution"]["sha256"], "Source distribution")
    prefix = PIN["source_distribution"]["vendor_prefix"]
    with tempfile.TemporaryDirectory(prefix="extract-", dir=cache) as temporary:
        extracted = Path(temporary)
        with tarfile.open(archive, "r:gz") as distribution:
            for member in distribution.getmembers():
                if not member.name.startswith(prefix):
                    continue
                relative = PurePosixPath(member.name[len(prefix):])
                if relative.is_absolute() or ".." in relative.parts:
                    raise RuntimeError(f"Unsafe source archive path: {member.name}")
                destination = extracted / relative
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True)
                elif member.isfile():
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    with distribution.extractfile(member) as data, destination.open("wb") as output:
                        shutil.copyfileobj(data, output)
                else:
                    raise RuntimeError(f"Unsupported source archive entry: {member.name}")
        verify_source(extracted)
        extracted.rename(source)
    return source


def verify_resources(install):
    digest, count = source_manifest(install / "share/ghostty/themes")
    require_equal(digest, PIN["themes"]["source_manifest_sha256"], "Bundled theme manifest")
    require_equal(count, PIN["themes"]["file_count"], "Bundled theme count")


def prepare_resources(cache, install):
    destination = install / "share/ghostty/themes"
    if destination.exists():
        verify_resources(install)
        return
    archive = cache / "ghostty-themes.tgz"
    if not archive.exists():
        temporary = archive.with_suffix(".download")
        # The dependency CDN rejects Python's default HTTP user agent. curl is
        # supplied by macOS; the fixed digest is checked before extraction.
        subprocess.run(["curl", "--fail", "--location", "--silent", "--show-error",
                        "--output", str(temporary), PIN["themes"]["url"]], check=True)
        require_equal(sha256(temporary), PIN["themes"]["sha256"], "Downloaded theme distribution")
        temporary.replace(archive)
    require_equal(sha256(archive), PIN["themes"]["sha256"], "Theme distribution")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="themes-", dir=cache) as temporary:
        staging = Path(temporary)
        prefix = PIN["themes"]["archive_prefix"]
        with tarfile.open(archive, "r:gz") as distribution:
            for member in distribution.getmembers():
                if member.isdir() and member.name.rstrip("/") == prefix.rstrip("/"):
                    continue
                if not member.name.startswith(prefix):
                    raise RuntimeError(f"Unexpected theme archive entry: {member.name}")
                relative = PurePosixPath(member.name[len(prefix):])
                if relative.is_absolute() or ".." in relative.parts or not member.isfile():
                    raise RuntimeError(f"Unsafe theme archive entry: {member.name}")
                output = staging / relative
                output.parent.mkdir(parents=True, exist_ok=True)
                with distribution.extractfile(member) as data, output.open("wb") as stream:
                    shutil.copyfileobj(data, stream)
        digest, count = source_manifest(staging)
        require_equal(digest, PIN["themes"]["source_manifest_sha256"], "Extracted theme manifest")
        require_equal(count, PIN["themes"]["file_count"], "Extracted theme count")
        staging.rename(destination)
    verify_resources(install)


def run_output(*command, env=None):
    return subprocess.check_output(command, env=env, text=True).strip()


def write_provenance(install, action, **extra):
    provenance = {
        "schema_version": 1,
        "action": action,
        "recorded_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "revision": PIN["revision"],
        "source_distribution_sha256": PIN["source_distribution"]["sha256"],
        "source_manifest_sha256": PIN["source_manifest"]["sha256"],
        "zig_version": PIN["zig"]["version"],
        "target": PIN["gate_artifacts"]["target"],
        "artifacts": {relative: sha256(install / relative) for relative in ARTIFACTS},
        "themes": {
            "distribution_sha256": PIN["themes"]["sha256"],
            "source_manifest_sha256": PIN["themes"]["source_manifest_sha256"],
            "file_count": PIN["themes"]["file_count"],
        },
        **extra,
    }
    (install / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    return provenance


def verify_install(install):
    data = json.loads((install / "provenance.json").read_text())
    require_equal(data["schema_version"], 1, "Provenance schema")
    require_equal(data["revision"], PIN["revision"], "Ghostty revision")
    require_equal(data["source_distribution_sha256"], PIN["source_distribution"]["sha256"], "Source distribution")
    require_equal(data["source_manifest_sha256"], PIN["source_manifest"]["sha256"], "Source manifest")
    require_equal(data["zig_version"], PIN["zig"]["version"], "Zig version")
    require_equal(data["target"], PIN["gate_artifacts"]["target"], "Archive target")
    if data["action"] == "import_exact_go_artifacts":
        require_equal(data["artifacts"][ARTIFACTS[0]], PIN["gate_artifacts"][ARTIFACTS[0]], "Imported GO archive")
    elif data["action"] == "built_from_verified_pinned_source":
        require_equal(data["zig_binary_sha256"], PIN["zig"]["macos_aarch64_binary_sha256"], "Build Zig binary")
    else:
        raise RuntimeError("Unrecognized preparation provenance")
    for relative in ARTIFACTS:
        require_equal(sha256(install / relative), data["artifacts"][relative], f"Artifact {relative}")
    require_equal(data["artifacts"]["include/ghostty.h"], PIN["gate_artifacts"]["include/ghostty.h"], "Frozen C header")
    verify_resources(install)
    print(json.dumps({"verified_install": str(install), **data}, indent=2))


def import_gate(cache, origin):
    for relative in ARTIFACTS:
        require_equal(sha256(origin / relative), PIN["gate_artifacts"][relative], f"GO artifact {relative}")
    install = cache / "install"
    if install.exists():
        raise RuntimeError(f"Install already exists; verify or choose a new --cache-dir: {install}")
    # Copy only the two linked build inputs, never a global cache or executable.
    with tempfile.TemporaryDirectory(prefix="import-", dir=cache) as temporary:
        staging = Path(temporary)
        for relative in ARTIFACTS:
            destination = staging / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(origin / relative, destination)
            require_equal(sha256(destination), PIN["gate_artifacts"][relative], f"Copied {relative}")
        prepare_resources(cache, staging)
        write_provenance(staging, "import_exact_go_artifacts", source_install=str(origin))
        staging.rename(install)
    verify_install(install)


def build(cache, zig, jobs, source_archive):
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise RuntimeError("This pinned production slice supports native aarch64 macOS builds only")
    if not zig:
        raise RuntimeError("Pass --zig /path/to/zig-0.16.0 (or set ZIG); no toolchain is installed automatically")
    zig = Path(zig).expanduser().resolve()
    require_equal(run_output(str(zig), "version"), PIN["zig"]["version"], "Zig version")
    require_equal(sha256(zig), PIN["zig"]["macos_aarch64_binary_sha256"], "Pinned macOS arm64 Zig binary")
    install = cache / "install"
    if install.exists():
        raise RuntimeError(f"Install already exists; verify or choose a new --cache-dir: {install}")
    source = prepare_source(cache, source_archive)
    # Zig 0.16 populates a zig-pkg directory beside build.zig. Keep the source
    # whose complete manifest we verify separate from this mutable build tree.
    build_source = cache / "build-source"
    if build_source.exists():
        raise RuntimeError(f"Build tree already exists; use a new --cache-dir: {build_source}")
    shutil.copytree(source, build_source)
    environment = os.environ.copy()
    developer_dir = environment.get("DEVELOPER_DIR") or run_output("xcode-select", "-p")
    environment["DEVELOPER_DIR"] = developer_dir
    sdk = run_output("xcrun", "--sdk", "macosx", "--show-sdk-path", env=environment)
    environment.update({
        "SDKROOT": sdk,
        "CC": run_output("xcrun", "--find", "clang", env=environment),
        "AR": run_output("xcrun", "--find", "ar", env=environment),
        "LD": run_output("xcrun", "--find", "ld", env=environment),
        "ZIG_GLOBAL_CACHE_DIR": str(cache / "zig-global"),
        "ZIG_LOCAL_CACHE_DIR": str(cache / "zig-local"),
    })
    command = [str(zig), "build", "--prefix", str(install), f"-j{jobs}", *PIN["build_options"]]
    log_path = cache / "build.log"
    print(f"Building pinned libghostty; log: {log_path}", flush=True)
    with log_path.open("w") as log:
        subprocess.run(command, cwd=build_source, env=environment, stdout=log, stderr=subprocess.STDOUT, check=True)
    verify_source(source)
    require_equal(sha256(install / "include/ghostty.h"), PIN["gate_artifacts"]["include/ghostty.h"], "Built C header")
    prepare_resources(cache, install)
    write_provenance(
        install, "built_from_verified_pinned_source", command=command,
        working_directory=str(build_source),
        zig_binary_sha256=sha256(zig),
        build_environment={key: environment[key] for key in (
            "DEVELOPER_DIR", "SDKROOT", "CC", "AR", "LD", "ZIG_GLOBAL_CACHE_DIR", "ZIG_LOCAL_CACHE_DIR")},
        build_log_sha256=sha256(log_path),
        matches_go_archive=sha256(install / ARTIFACTS[0]) == PIN["gate_artifacts"][ARTIFACTS[0]],
    )
    verify_install(install)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("fetch", "build", "import-gate", "resources", "verify"))
    parser.add_argument("--cache-dir", type=Path, default=ROOT / ".runtime/pinned")
    parser.add_argument("--install-dir", type=Path, help="verify an explicit prepared installation")
    parser.add_argument("--from-install", type=Path, help="import-gate: existing exact GO installation")
    parser.add_argument("--source-archive", type=Path, help="read an existing distribution after checking its fixed SHA-256")
    parser.add_argument("--zig", default=os.environ.get("ZIG"))
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    cache = args.cache_dir.expanduser().resolve()
    if args.jobs < 1:
        parser.error("--jobs must be positive")
    if args.action == "verify":
        verify_install((args.install_dir or cache / "install").expanduser().resolve())
        return
    cache.mkdir(parents=True, exist_ok=True)
    if args.action == "fetch":
        print(prepare_source(cache, args.source_archive))
    elif args.action == "build":
        build(cache, args.zig, args.jobs, args.source_archive)
    elif args.action == "import-gate":
        if not args.from_install:
            parser.error("import-gate requires --from-install")
        import_gate(cache, args.from_install.expanduser().resolve())
    elif args.action == "resources":
        install = (args.install_dir or cache / "install").expanduser().resolve()
        if not (install / "provenance.json").is_file():
            parser.error("resources requires an already prepared library installation")
        prepare_resources(cache, install)
        provenance_path = install / "provenance.json"
        provenance = json.loads(provenance_path.read_text())
        provenance["themes"] = {
            "distribution_sha256": PIN["themes"]["sha256"],
            "source_manifest_sha256": PIN["themes"]["source_manifest_sha256"],
            "file_count": PIN["themes"]["file_count"],
        }
        provenance_path.write_text(json.dumps(provenance, indent=2) + "\n")
        verify_install(install)


if __name__ == "__main__":
    main()
