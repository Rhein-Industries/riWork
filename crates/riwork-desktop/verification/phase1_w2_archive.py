#!/usr/bin/env python3
"""Archive selected W2 evidence without copying fixture state or executables.

Example:
  python3 crates/riwork-desktop/verification/phase1_w2_archive.py \
    --run /tmp/riwork-w2-host/tests-EXAMPLE /tmp/riwork-w2-host/restore-EXAMPLE \
    --superseded-run /tmp/riwork-w2-host/restore-OLDER \
    --file /tmp/riwork-w2-window-check/control-tab.json

Only immediate .json/.xml/.stdout/.stderr/.png files are read. Run directories
must contain a manifest; individual checkpoint artifacts use --file. Existing
index entries are retained. Repeating unchanged inputs is a no-op; a changed
source gets another archive directory, preserving the previous raw bytes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile


SUFFIXES = frozenset({".json", ".xml", ".stdout", ".stderr", ".png"})
EXCLUDED = frozenset({
    "home", "user", "state", "runtime", "run", "tmux", "cache", "nsh-cache",
    "target", "bundle", "bundles", "fixture-bin", "auto-close", "auto-quit",
})
DEFAULT_OUTPUT = Path(__file__).resolve().with_name("w2-host-evidence")


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def checked_source(path):
    if path.is_symlink():
        raise ValueError(f"Refusing a symlink source: {path}")
    path = path.resolve(strict=True)
    if any(part in EXCLUDED for part in path.parts):
        raise ValueError(f"Refusing fixture state, cache or bundle content: {path}")
    return path


def collect(groups, path, disposition, *, directory):
    path = checked_source(path)
    if directory:
        if not path.is_dir() or not (path / "manifest.json").is_file():
            raise ValueError(f"--run needs a directory with manifest.json: {path}")
        files = [item for item in path.iterdir()
                 if item.is_file() and not item.is_symlink() and item.suffix in SUFFIXES]
        source = path
    else:
        if not path.is_file() or path.suffix not in SUFFIXES:
            raise ValueError(f"Not an allowed evidence file: {path}")
        files, source = [path], path.parent
    group = groups.setdefault(str(source), {"files": set(), "disposition": disposition})
    group["files"].update(files)
    if disposition == "superseded":
        group["disposition"] = disposition


def archive(groups, output):
    output.mkdir(parents=True, exist_ok=True)
    output = output.resolve()
    index_path = output / "artifact-index.json"
    if index_path.exists():
        index = json.loads(index_path.read_text())
        if index.get("schema_version") != 1:
            raise ValueError("Unsupported artifact index version")
    else:
        index = {"schema_version": 1, "runs": {}, "artifacts": {}}
    # Verify existing archive bytes before extending the index. Never repair or
    # replace evidence silently when a file has changed outside this helper.
    for relative, record in index["artifacts"].items():
        path = output / relative
        if not path.resolve().is_relative_to(output) or not path.is_file():
            raise ValueError(f"Missing or unsafe indexed artifact: {relative}")
        if sha256(path.read_bytes()) != record["archive_sha256"]:
            raise ValueError(f"Archived bytes were changed: {relative}")

    imported = []
    for source, group in sorted(groups.items()):
        payloads = {}
        for path in sorted(group["files"]):
            before = path.stat()
            data = path.read_bytes()
            after = path.stat()
            if (before.st_mtime_ns, before.st_size) != (after.st_mtime_ns, after.st_size):
                raise ValueError(f"Source changed while reading: {path}")
            payloads[path.name] = (path, data, sha256(data))
        base = re.sub(r"[^A-Za-z0-9_.-]", "_", Path(source).name)
        if not base or base in (".", ".."):
            raise ValueError(f"Invalid evidence directory name: {source}")
        name = base
        known = index["runs"].get(name)
        if known is not None and known["source_directory"] != source:
            name += "-" + sha256(source.encode())[:10]
        conflicts = any(
            f"{name}/{filename}" in index["artifacts"]
            and index["artifacts"][f"{name}/{filename}"]["source_sha256"] != digest
            for filename, (_, _, digest) in payloads.items()
        )
        if conflicts:
            fingerprint = json.dumps({filename: digest for filename, (_, _, digest)
                                      in payloads.items()}, sort_keys=True).encode()
            name += "-" + sha256(fingerprint)[:10]
        run = index["runs"].setdefault(name, {
            "source_directory": source,
            "disposition": group["disposition"],
            "artifacts": [],
        })
        if run["source_directory"] != source:
            raise ValueError(f"Archive directory collision: {name}")
        if group["disposition"] == "superseded":
            run["disposition"] = "superseded"
        destination = output / name
        if destination.is_symlink():
            raise ValueError(f"Refusing symlink archive directory: {destination}")
        destination.mkdir(exist_ok=True)
        for filename, (original, data, digest) in payloads.items():
            relative = f"{name}/{filename}"
            archived = output / relative
            if archived.exists():
                if archived.is_symlink() or archived.read_bytes() != data:
                    raise ValueError(f"Refusing to overwrite archived bytes: {relative}")
            else:
                with archived.open("xb") as stream:
                    stream.write(data)
            index["artifacts"][relative] = {
                "source": str(original),
                "source_sha256": digest,
                "archive_sha256": sha256(archived.read_bytes()),
                "bytes": len(data),
            }
            if relative not in run["artifacts"]:
                run["artifacts"].append(relative)
        run["artifacts"].sort()
        imported.append(name)

    encoded = (json.dumps(index, indent=2, sort_keys=True) + "\n").encode()
    if not index_path.exists() or index_path.read_bytes() != encoded:
        with tempfile.NamedTemporaryFile(dir=output, prefix=".artifact-index-", delete=False) as stream:
            stream.write(encoded)
            temporary = Path(stream.name)
        temporary.replace(index_path)
    return {"index": str(index_path), "imported_runs": imported,
            "total_runs": len(index["runs"]), "total_artifacts": len(index["artifacts"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", type=Path, nargs="+", action="append", default=[])
    parser.add_argument("--superseded-run", type=Path, nargs="+", action="append", default=[])
    parser.add_argument("--file", type=Path, nargs="+", action="append", default=[])
    parser.add_argument("--out", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    groups = {}
    for lists, disposition, directory in [
        (args.run, "selected", True),
        (args.superseded_run, "superseded", True),
        (args.file, "selected", False),
    ]:
        for paths in lists:
            for path in paths:
                collect(groups, path, disposition, directory=directory)
    if not groups:
        parser.error("Provide --run, --superseded-run or --file")
    print(json.dumps(archive(groups, args.out), indent=2))


if __name__ == "__main__":
    main()
