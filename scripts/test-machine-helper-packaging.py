#!/usr/bin/env python3
"""Disposable release/staging boundaries; no executable, service or live home used."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


packaging = module("helper_package", "package-machine-helpers.py")
staging = module("helper_stage", "stage-machine-helpers.py")


def digest(data):
    return hashlib.sha256(data).hexdigest()


def platform_zip(marker=b"inert binary; never launched", entry="Constellation Computer"):
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        archive.writestr(entry, marker)
    return out.getvalue()


def snapshot(folder):
    return {str(p.relative_to(folder)): p.read_bytes() for p in sorted(folder.rglob("*")) if p.is_file()}


class PackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.home = self.root / "observatory"
        self.home.mkdir()
        (self.home / "config.json").write_text('{"id":"fixture-only"}\n')
        (self.home / "state.sqlite3").write_bytes(b"opaque retained agent/inventory fixture")
        self.protected = snapshot(self.home)

    def inputs(self, folder, marker=b"first", version="0.1.0"):
        folder.mkdir()
        for platform in packaging.PLATFORMS:
            name = f"constellation-machine-helper-{version}-{platform}.zip"
            data = platform_zip(marker + platform.encode())
            (folder / name).write_bytes(data)
            (folder / (name + ".json")).write_text(json.dumps({
                "platform": platform, "version": version, "sha256": digest(data),
            }))
            (folder / (name + ".sha256")).write_text(digest(data) + "  " + name + "\n")
        return packaging.package(folder, version)

    def bundle(self, marker=b"first", name="release"):
        return self.inputs(self.root / name, marker)

    def stage(self, bundle):
        return staging.stage(self.home, bundle, digest(bundle.read_bytes()))

    def rewritten(self, bundle, transform):
        with zipfile.ZipFile(bundle) as archive:
            members = {n: archive.read(n) for n in archive.namelist()}
        manifest = json.loads(members["manifest.json"])
        transform(manifest, members)
        members["manifest.json"] = json.dumps(manifest).encode()
        out = self.root / "rewritten.zip"
        with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            for name, data in members.items():
                archive.writestr(name, data)
        return out

    def assert_protected(self):
        for name, content in self.protected.items():
            self.assertEqual((self.home / name).read_bytes(), content)

    def test_package_binds_all_five_platform_provenance_and_checksums(self):
        bundle = self.bundle()
        with zipfile.ZipFile(bundle) as archive:
            manifest = json.loads(archive.read("manifest.json"))
            self.assertEqual(manifest["schema"], 1)
            self.assertEqual({a["platform"] for a in manifest["artifacts"]}, set(packaging.PLATFORMS))
            self.assertEqual(len(archive.namelist()), 6)
            for artifact in manifest["artifacts"]:
                self.assertEqual(artifact["sha256"], digest(archive.read(artifact["filename"])))
        self.assertEqual(bundle.with_suffix(".zip.sha256").read_text().split(), [digest(bundle.read_bytes()), bundle.name])

    def test_same_platform_archives_produce_identical_bundle_despite_filesystem_timestamps(self):
        bundle = self.bundle()
        original = bundle.read_bytes()
        with zipfile.ZipFile(bundle) as archive:
            self.assertTrue(all(member.date_time == (1980, 1, 1, 0, 0, 0) for member in archive.infolist()))
        for path in bundle.parent.glob("constellation-machine-helper-*.zip"):
            os.utime(path, (1_234_567_890, 1_234_567_890))
        repeated = packaging.package(bundle.parent)
        self.assertEqual(repeated.read_bytes(), original)
        self.assertEqual(repeated.with_suffix(".zip.sha256").read_text().split(), [digest(original), repeated.name])

    def test_inconsistent_platform_provenance_refuses_before_replacing_previous_bundle(self):
        bundle = self.bundle()
        before = bundle.read_bytes()
        sidecar = bundle.with_suffix(".zip.sha256").read_bytes()
        metadata_path = next(bundle.parent.glob("*.zip.json"))
        record = json.loads(metadata_path.read_text())
        record["platform"] = "another-platform"
        metadata_path.write_text(json.dumps(record))
        with self.assertRaises(ValueError):
            packaging.package(bundle.parent)
        self.assertEqual(bundle.read_bytes(), before)
        self.assertEqual(bundle.with_suffix(".zip.sha256").read_bytes(), sidecar)

    def test_signed_platform_is_not_silently_repackaged_with_changed_resources(self):
        bundle = self.bundle()
        before = bundle.read_bytes()
        metadata_path = next(bundle.parent.glob("*macos-aarch64.zip.json"))
        record = json.loads(metadata_path.read_text())
        record["signed_or_notarized"] = True
        metadata_path.write_text(json.dumps(record))
        with self.assertRaises(ValueError):
            packaging.package(bundle.parent)
        self.assertEqual(bundle.read_bytes(), before)

    def test_unreviewed_outer_digest_preserves_every_staged_and_installation_file(self):
        bundle = self.bundle()
        self.stage(bundle)
        before = snapshot(self.home)
        with self.assertRaises(ValueError):
            staging.stage(self.home, bundle, "0" * 64)
        self.assertEqual(snapshot(self.home), before)

    def test_nested_digest_mismatch_preserves_previous_stage(self):
        bundle = self.bundle()
        self.stage(bundle)
        before = snapshot(self.home)
        def change(manifest, members):
            members[manifest["artifacts"][0]["filename"]] = b"changed after platform build"
        corrupt = self.rewritten(bundle, change)
        with self.assertRaises(ValueError):
            self.stage(corrupt)
        self.assertEqual(snapshot(self.home), before)

    def test_outer_archive_path_traversal_is_refused_before_any_file_moves(self):
        bundle = self.bundle()
        self.stage(bundle)
        before = snapshot(self.home)
        def change(manifest, members):
            old = manifest["artifacts"][0]["filename"]
            new = "../escaped.zip"
            manifest["artifacts"][0]["filename"] = new
            members[new] = members.pop(old)
        malicious = self.rewritten(bundle, change)
        with self.assertRaises(ValueError):
            self.stage(malicious)
        self.assertEqual(snapshot(self.home), before)
        self.assertFalse((self.root / "escaped.zip").exists())

    def test_hash_consistent_but_corrupt_nested_zip_is_refused(self):
        bundle = self.bundle()
        self.stage(bundle)
        before = snapshot(self.home)
        def change(manifest, members):
            artifact = manifest["artifacts"][0]
            data = b"not a ZIP despite matching digest"
            members[artifact["filename"]] = data
            artifact["sha256"] = digest(data)
        corrupt = self.rewritten(bundle, change)
        with self.assertRaises((ValueError, zipfile.BadZipFile)):
            self.stage(corrupt)
        self.assertEqual(snapshot(self.home), before)

    def test_nested_traversal_and_embedded_pairing_setup_are_not_release_payload(self):
        bundle = self.bundle()
        self.stage(bundle)
        before = snapshot(self.home)
        for entry in ["../escape", "observatory-pairing.json", "Constellation Computer.app/Contents/Resources/observatory-pairing.json"]:
            with self.subTest(entry=entry):
                def change(manifest, members):
                    artifact = manifest["artifacts"][0]
                    data = platform_zip(b"unsafe release member", entry)
                    members[artifact["filename"]] = data
                    artifact["sha256"] = digest(data)
                unsafe = self.rewritten(bundle, change)
                with self.assertRaises(ValueError):
                    self.stage(unsafe)
                self.assertEqual(snapshot(self.home), before)

    def test_oversized_manifest_is_refused_before_staging(self):
        bundle = self.bundle()
        self.stage(bundle)
        before = snapshot(self.home)
        def change(manifest, _members):
            manifest["padding"] = "x" * (1024 * 1024)
        oversized = self.rewritten(bundle, change)
        with self.assertRaises(ValueError):
            self.stage(oversized)
        self.assertEqual(snapshot(self.home), before)

    def test_same_bundle_is_idempotent_without_touching_existing_files_or_extra_notes(self):
        bundle = self.bundle()
        self.stage(bundle)
        target = self.home / "machine-helper-artifacts"
        (target / "owner-note.txt").write_text("preserve this locally retained note")
        before = snapshot(self.home)
        identities = {p: (p.stat().st_ino, p.stat().st_mtime_ns) for p in target.iterdir()}
        result = self.stage(bundle)
        self.assertTrue(result["unchanged"])
        self.assertEqual(snapshot(self.home), before)
        self.assertEqual(identities, {p: (p.stat().st_ino, p.stat().st_mtime_ns) for p in target.iterdir()})
        self.assertFalse(list(self.home.glob("machine-helper-artifacts-backup-*")))

    def test_new_bundle_keeps_exact_prior_tree_and_unrelated_installation_state(self):
        first = self.bundle()
        self.stage(first)
        target = self.home / "machine-helper-artifacts"
        (target / "owner-note.txt").write_text("preserve this before replacing")
        before = snapshot(target)
        newer = self.bundle(b"newer", "newer")
        result = self.stage(newer)
        self.assertEqual(snapshot(Path(result["backup"])), before)
        self.assertEqual(result["pairings_created"], 0)
        self.assertEqual(result["service_operations"], 0)
        self.assertNotEqual(snapshot(target), before)
        self.assert_protected()

    def test_failed_final_replace_restores_previous_directory_exactly(self):
        first = self.bundle()
        self.stage(first)
        before = snapshot(self.home)
        newer = self.bundle(b"newer", "newer")
        actual_replace = os.replace
        def replace(source, target):
            if Path(source).parent.name.startswith(".helper-stage-"):
                raise OSError("injected final publication failure")
            return actual_replace(source, target)
        with patch.object(staging.os, "replace", side_effect=replace):
            with self.assertRaises(OSError):
                self.stage(newer)
        self.assertEqual(snapshot(self.home), before)

    def test_symlink_target_is_not_replaced_or_followed(self):
        bundle = self.bundle()
        other = self.root / "unrelated-directory"
        other.mkdir()
        (other / "keep").write_text("unchanged")
        (self.home / "machine-helper-artifacts").symlink_to(other, target_is_directory=True)
        before = snapshot(other)
        with self.assertRaises(ValueError):
            self.stage(bundle)
        self.assertEqual(snapshot(other), before)
        self.assertTrue((self.home / "machine-helper-artifacts").is_symlink())


if __name__ == "__main__":
    unittest.main(verbosity=2)
