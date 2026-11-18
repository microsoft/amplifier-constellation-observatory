import importlib.util
import hashlib
import io
import json
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock
import zipfile

spec = importlib.util.spec_from_file_location('fetch', Path(__file__).with_name('fetch-instance-bundle.py'))
fetch = importlib.util.module_from_spec(spec); spec.loader.exec_module(fetch)


class ExtractionTests(unittest.TestCase):
    def test_default_local_bundle_uses_its_pin_and_preserves_paired_recipe(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = 'x86_64-pc-windows-msvc'
            source = 'a' * 40
            payload = io.BytesIO()
            with zipfile.ZipFile(payload, 'w') as archive:
                archive.writestr('consumer-bundle.json', json.dumps({
                    'source': source, 'version': '0.22.14', 'target': target}))
            raw = payload.getvalue()
            bundled = {'repository': 'microsoft/amplifier-constellation-instance',
                       'version': '0.22.14', 'source': source,
                       'targets': {target: {'sha256': hashlib.sha256(raw).hexdigest()}}}
            paired = {**bundled, 'version': '0.22.12', 'source': 'b' * 40,
                      'targets': {target: {'sha256': 'c' * 64}}}
            paired_path = root / 'instance-bundle-lock.json'
            paired_bytes = (json.dumps(paired) + '\n').encode()
            paired_path.write_bytes(paired_bytes)
            (root / 'bundled-instance-lock.json').write_text(json.dumps(bundled))
            local_source = root / 'fetch-instance-bundle.py'
            local_source.write_bytes(Path(fetch.__file__).read_bytes())
            local_spec = importlib.util.spec_from_file_location('isolated_bundle_fetch', local_source)
            local = importlib.util.module_from_spec(local_spec); local_spec.loader.exec_module(local)
            def download(url, timeout):
                self.assertEqual(url, f'https://github.com/microsoft/amplifier-constellation-instance/releases/download/v0.22.14/agent-instance-consumer-{target}.zip')
                self.assertEqual(timeout, 60)
                return io.BytesIO(raw)
            with mock.patch.object(local.urllib.request, 'urlopen', side_effect=download) as opened:
                local.fetch(target, root / 'bundle')
                self.assertEqual(opened.call_count, 1)
            self.assertEqual(paired_path.read_bytes(), paired_bytes)
            self.assertEqual(json.loads((root / 'bundle/consumer-bundle.json').read_text())['version'], '0.22.14')

    def test_missing_local_target_never_uses_paired_or_network_fallback(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); lock = root / 'bundled-instance-lock.json'
            lock.write_text(json.dumps({'repository': 'microsoft/amplifier-constellation-instance',
                'version': '0.22.14', 'source': 'a' * 40, 'targets': {}}))
            with mock.patch.object(fetch.urllib.request, 'urlopen') as opened:
                with self.assertRaisesRegex(ValueError, 'No public local bundle'):
                    fetch.fetch('aarch64-apple-darwin', root / 'bundle', lock)
                opened.assert_not_called()
            self.assertFalse((root / 'bundle').exists())

    def test_verified_package_keeps_ordinary_executable_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); archive = root / 'package.zip'
            with zipfile.ZipFile(archive, 'w') as source:
                entry = zipfile.ZipInfo('support/python'); entry.external_attr = (stat.S_IFREG | 0o755) << 16
                source.writestr(entry, b'private runtime')
            fetch.unpack(archive, root / 'out')
            self.assertEqual((root / 'out/support/python').read_bytes(), b'private runtime')
            with self.assertRaisesRegex(ValueError, 'new package'):
                fetch.unpack(archive, root / 'out')

    def test_escaping_links_and_case_collisions_never_publish(self):
        for names in (['../outside'], ['C:/outside'], ['support\\outside'], ['/outside'], ['File', 'file'], ['link']):
            with self.subTest(names=names), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); archive = root / 'package.zip'
                with zipfile.ZipFile(archive, 'w') as source:
                    for name in names:
                        entry = zipfile.ZipInfo(name)
                        entry.external_attr = ((stat.S_IFLNK if name == 'link' else stat.S_IFREG) | 0o644) << 16
                        source.writestr(entry, b'no')
                with self.assertRaises(ValueError): fetch.unpack(archive, root / 'out')
                self.assertFalse((root / 'out').exists())

    def test_linux_distinct_terminfo_names_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); archive = root / 'package.zip'
            with zipfile.ZipFile(archive, 'w') as source:
                source.writestr('share/terminfo/2/2621A', b'upper')
                source.writestr('share/terminfo/2/2621a', b'lower')
            fetch.unpack(archive, root / 'out', case_sensitive=True)
            self.assertEqual((root / 'out/share/terminfo/2/2621A').read_bytes(), b'upper')
            self.assertEqual((root / 'out/share/terminfo/2/2621a').read_bytes(), b'lower')


if __name__ == '__main__': unittest.main()
