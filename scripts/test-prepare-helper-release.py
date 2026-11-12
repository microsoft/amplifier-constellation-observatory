"""Release-only fixtures: no executable or installation is started."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import zipfile


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


release = module('release_helpers', Path(__file__).with_name('prepare-helper-release.py'))
packager = module('individual_helpers', Path(__file__).resolve().parents[1] / 'machine-helper/packaging/package.py')


class HelperReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'artifacts'
        binary = self.root / 'inert-binary'
        binary.write_bytes(b'Fixture bytes; never executed')
        for platform in release.aggregate.PLATFORMS:
            packager.package(binary, platform, '0.2.1', self.source)

    def test_exact_twenty_assets_exclude_app_and_retain_checked_platform_bytes(self):
        (self.source / 'agent-observatory-unrelated.zip').write_bytes(b'not a helper')
        first = self.root / 'first'
        result = release.prepare(self.source, first, '0.2.1')
        self.assertEqual(len(result), 20)
        self.assertNotIn('agent-observatory-unrelated.zip', result)
        for path in self.source.glob('constellation-machine-helper-*'):
            self.assertEqual((first / path.name).read_bytes(), path.read_bytes())
        repeated = release.prepare(self.source, self.root / 'second', '0.2.1')
        self.assertEqual(result, repeated)

    def test_used_output_is_preserved(self):
        out = self.root / 'output'
        out.mkdir()
        protected = out / 'existing.zip'
        protected.write_bytes(b'previous independently qualified package')
        with self.assertRaisesRegex(ValueError, 'new empty'):
            release.prepare(self.source, out, '0.2.1')
        self.assertEqual(protected.read_bytes(), b'previous independently qualified package')

    def test_missing_platform_or_wrong_version_refuses(self):
        path = next(self.source.glob('*windows-aarch64.zip.json'))
        record = json.loads(path.read_text())
        record['version'] = '0.2.0'
        path.write_text(json.dumps(record))
        with self.assertRaisesRegex(ValueError, 'metadata differs'):
            release.prepare(self.source, self.root / 'wrong-version', '0.2.1')
        path.unlink()
        with self.assertRaisesRegex(ValueError, 'ordinary release file'):
            release.prepare(self.source, self.root / 'missing-platform', '0.2.1')

    def rewritten(self, transform):
        path = next(self.source.glob('*windows-x86_64.zip'))
        with zipfile.ZipFile(path) as archive:
            contents = {name: archive.read(name) for name in archive.namelist()}
        transform(contents)
        with zipfile.ZipFile(path, 'w') as archive:
            for name, data in contents.items():
                archive.writestr(name, data)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        metadata = path.with_suffix('.zip.json')
        record = json.loads(metadata.read_text())
        record['sha256'] = digest
        metadata.write_text(json.dumps(record))
        path.with_suffix('.zip.sha256').write_text(digest + '  ' + path.name + '\n')

    def test_changed_binary_cannot_hide_behind_updated_archive_checksum(self):
        self.rewritten(lambda files: files.update({'constellation-machine-helper.exe': b'different executable'}))
        with self.assertRaisesRegex(ValueError, 'executable differs'):
            release.prepare(self.source, self.root / 'changed-binary', '0.2.1')

    def test_platform_package_must_not_include_an_installation_bootstrap(self):
        self.rewritten(lambda files: files.update({'observatory-pairing.json': b'{}'}))
        with self.assertRaisesRegex(ValueError, 'embedded pairing'):
            release.prepare(self.source, self.root / 'bootstrap', '0.2.1')


if __name__ == '__main__':
    unittest.main()
