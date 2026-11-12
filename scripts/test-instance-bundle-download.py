import importlib.util
from pathlib import Path
import stat
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('fetch', Path(__file__).with_name('fetch-instance-bundle.py'))
fetch = importlib.util.module_from_spec(spec); spec.loader.exec_module(fetch)


class ExtractionTests(unittest.TestCase):
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
