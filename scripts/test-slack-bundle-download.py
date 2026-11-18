import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock
import urllib.request

spec = importlib.util.spec_from_file_location('fetch', Path(__file__).with_name('fetch-slack-bundle.py'))
fetch = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetch)


class DownloadTests(unittest.TestCase):
    def fixture(self, root, data=b'exact owner archive'):
        target = 'x86_64-pc-windows-msvc'
        row = {'url': 'https://github.com/microsoft/amplifier-acp-slack/releases/download/v0.18.0/native.zip',
               'release': {'target': target, 'version': '0.18.0', 'source': 'a' * 40,
                           'archive_size': len(data), 'archive_sha256': hashlib.sha256(data).hexdigest()}}
        lock = root / 'lock.json'
        lock.write_text(json.dumps({'schema': 1, 'targets': {target: row}}))
        return target, lock, row

    def test_exact_bytes_publish_once_and_existing_destination_is_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); target, lock, row = self.fixture(root)
            opener = Mock(); opener.open.return_value = io.BytesIO(b'exact owner archive')
            output = root / 'native.zip'
            result = fetch.fetch(target, output, lock, opener=opener)
            self.assertEqual(result['archive_sha256'], row['release']['archive_sha256'])
            self.assertEqual(output.read_bytes(), b'exact owner archive')
            with self.assertRaisesRegex(ValueError, 'preserved'):
                fetch.fetch(target, output, lock, opener=opener)
            opener.open.assert_called_once_with(row['url'], timeout=60)

    def test_truncated_changed_and_excessive_downloads_never_publish(self):
        for data in (b'short', b'wrong owner archive', b'exact owner archive plus extra'):
            with self.subTest(data=data), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); target, lock, _ = self.fixture(root)
                opener = Mock(); opener.open.return_value = io.BytesIO(data)
                with self.assertRaises(ValueError):
                    fetch.fetch(target, root / 'native.zip', lock, opener=opener)
                self.assertFalse((root / 'native.zip').exists())
                self.assertEqual(list(root.glob('slack-download-*')), [])

    def test_wrong_owner_target_or_url_refuses_before_network(self):
        changes = [{'url': 'http://github.com/microsoft/amplifier-acp-slack/releases/download/v0.18.0/native.zip'},
                   {'url': 'https://github.com/other/repository/releases/download/v0.18.0/native.zip'},
                   {'url': 'https://github.com/microsoft/amplifier-acp-slack/releases/download/v0.18.0/native.zip?x=1'},
                   {'release': {'target': 'aarch64-apple-darwin'}}]
        for change in changes:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); target, lock, row = self.fixture(root)
                row.update(change); lock.write_text(json.dumps({'schema': 1, 'targets': {target: row}}))
                opener = Mock()
                with self.assertRaises(ValueError): fetch.fetch(target, root / 'out', lock, opener=opener)
                opener.open.assert_not_called()

    def test_unexpected_redirect_never_fetches_another_host_or_downgrades_tls(self):
        request = urllib.request.Request('https://github.com/locked')
        for url in ('http://release-assets.githubusercontent.com/a', 'https://example.com/a',
                    'https://user:secret@github.com/a', 'https://github.com:444/a'):
            with self.subTest(url=url), self.assertRaises(ValueError):
                fetch.ReleaseRedirects().redirect_request(request, None, 302, '', {}, url)


if __name__ == '__main__': unittest.main()
