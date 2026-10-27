#!/usr/bin/env python3
"""Integrity failures must be fatal before an archive is compiled or linked."""
import contextlib
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "prepare_ghostty", Path(__file__).with_name("prepare_ghostty.py")
)
PREPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE)


class IntegrityTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="riwork-ghostty-integrity-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.pin = copy.deepcopy(PREPARE.PIN)
        self.patch = patch.object(PREPARE, "PIN", self.pin)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def make_install(self):
        for relative, contents in (
            ("lib/libghostty-internal.a", b"test static archive"),
            ("include/ghostty.h", b"test API header"),
        ):
            path = self.root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(contents)
            self.pin["gate_artifacts"][relative] = hashlib.sha256(contents).hexdigest()
        themes = self.root / "share/ghostty/themes"
        themes.mkdir(parents=True)
        (themes / "Test Theme").write_text("background = 123456\n")
        digest, count = PREPARE.source_manifest(themes)
        self.pin["themes"]["source_manifest_sha256"] = digest
        self.pin["themes"]["file_count"] = count
        return PREPARE.write_provenance(self.root, "import_exact_go_artifacts")

    def test_valid_import_provenance(self):
        self.make_install()
        with contextlib.redirect_stdout(io.StringIO()):
            PREPARE.verify_install(self.root)

    def test_mutated_archive_is_rejected(self):
        self.make_install()
        (self.root / "lib/libghostty-internal.a").write_bytes(b"different binary")
        with self.assertRaisesRegex(RuntimeError, "Artifact lib/libghostty-internal.a"):
            PREPARE.verify_install(self.root)

    def test_mutated_header_is_rejected(self):
        self.make_install()
        (self.root / "include/ghostty.h").write_bytes(b"different ABI")
        with self.assertRaisesRegex(RuntimeError, "Artifact include/ghostty.h"):
            PREPARE.verify_install(self.root)

    def test_changed_import_hash_cannot_override_gate_pin(self):
        data = self.make_install()
        changed = b"different binary"
        (self.root / "lib/libghostty-internal.a").write_bytes(changed)
        data["artifacts"]["lib/libghostty-internal.a"] = hashlib.sha256(changed).hexdigest()
        (self.root / "provenance.json").write_text(json.dumps(data))
        with self.assertRaisesRegex(RuntimeError, "Imported GO archive"):
            PREPARE.verify_install(self.root)

    def test_mutated_bundled_theme_is_rejected(self):
        self.make_install()
        (self.root / "share/ghostty/themes/Test Theme").write_text("command = untrusted\n")
        with self.assertRaisesRegex(RuntimeError, "Bundled theme manifest"):
            PREPARE.verify_install(self.root)

    def test_unrecognized_source_provenance_is_rejected(self):
        data = self.make_install()
        data["action"] = "system-cache-fallback"
        (self.root / "provenance.json").write_text(json.dumps(data))
        with self.assertRaisesRegex(RuntimeError, "Unrecognized preparation provenance"):
            PREPARE.verify_install(self.root)

    def make_tar(self, name, symlink=False):
        archive = self.root / "source.crate"
        with tarfile.open(archive, "w:gz") as stream:
            member = tarfile.TarInfo(self.pin["source_distribution"]["vendor_prefix"] + name)
            if symlink:
                member.type = tarfile.SYMTYPE
                member.linkname = "/tmp/forbidden-ghostty-source-link"
            stream.addfile(member)
        self.pin["source_distribution"]["sha256"] = PREPARE.sha256(archive)
        return archive

    def test_archive_traversal_is_rejected(self):
        archive = self.make_tar("../outside")
        with self.assertRaisesRegex(RuntimeError, "Unsafe source archive path"):
            PREPARE.prepare_source(self.root, archive)
        self.assertFalse((self.root / "source").exists())

    def test_archive_symlink_is_rejected(self):
        archive = self.make_tar("linked-file", symlink=True)
        with self.assertRaisesRegex(RuntimeError, "Unsupported source archive entry"):
            PREPARE.prepare_source(self.root, archive)
        self.assertFalse((self.root / "source").exists())

    def test_source_archive_hash_mismatch_prevents_extraction(self):
        archive = self.root / "wrong.crate"
        archive.write_bytes(b"untrusted archive")
        with self.assertRaisesRegex(RuntimeError, "Source distribution"):
            PREPARE.prepare_source(self.root, archive)
        self.assertFalse((self.root / "source").exists())


if __name__ == "__main__":
    unittest.main()
