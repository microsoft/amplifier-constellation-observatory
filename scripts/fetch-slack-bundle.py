#!/usr/bin/env python3
"""Fetch the exact reviewed Slack ZIP for build-time Windows/macOS composition."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import tempfile
import urllib.parse
import urllib.request

TARGETS = {'x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc',
           'x86_64-apple-darwin', 'aarch64-apple-darwin'}
MAX_BYTES = 1024 ** 3
REDIRECT_HOSTS = {'github.com', 'release-assets.githubusercontent.com',
                  'objects.githubusercontent.com'}


def selected(target, lock_path):
    lock = json.loads(lock_path.read_text(encoding='utf-8'))
    if target not in TARGETS or lock.get('schema') != 1:
        raise ValueError('No supported Slack package target')
    row = lock.get('targets', {}).get(target)
    if not isinstance(row, dict) or set(row) != {'url', 'release'}:
        raise ValueError('No reviewed Slack package is locked for this target')
    release = row['release']
    if (not isinstance(release, dict) or release.get('target') != target
            or not re.fullmatch(r'\d+\.\d+\.\d+', str(release.get('version', '')))
            or not re.fullmatch(r'[a-f0-9]{40}', str(release.get('source', '')))
            or not re.fullmatch(r'[a-f0-9]{64}', str(release.get('archive_sha256', '')))
            or type(release.get('archive_size')) is not int
            or not 1 <= release['archive_size'] <= MAX_BYTES):
        raise ValueError('Invalid reviewed Slack package pin')
    url = urllib.parse.urlsplit(row['url'])
    prefix = f'/microsoft/amplifier-acp-slack/releases/download/v{release["version"]}/'
    member = url.path.removeprefix(prefix)
    if (url.scheme != 'https' or url.netloc != 'github.com' or url.query
            or url.fragment or not url.path.startswith(prefix)
            or not re.fullmatch(r'[A-Za-z0-9._-]+\.zip', member)):
        raise ValueError('Use the locked official Slack release ZIP')
    return row


class ReleaseRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        url = urllib.parse.urlsplit(newurl)
        if (url.scheme != 'https' or url.hostname not in REDIRECT_HOSTS
                or url.username or url.password or url.port or url.fragment):
            raise ValueError('Unexpected Slack release redirect')
        return super().redirect_request(request, fp, code, message, headers, newurl)


def fetch(target, output, lock_path=None, *, opener=None):
    row = selected(target, lock_path or Path(__file__).with_name('slack-bundle-lock.json'))
    if output.exists() or output.is_symlink():
        raise ValueError('Existing Slack archive was preserved')
    output.parent.mkdir(parents=True, exist_ok=True)
    release = row['release']
    opener = opener or urllib.request.build_opener(ReleaseRedirects())
    with tempfile.TemporaryDirectory(prefix='slack-download-', dir=output.parent) as temporary:
        downloaded = Path(temporary) / 'package.zip'
        digest, total = hashlib.sha256(), 0
        with opener.open(row['url'], timeout=60) as incoming, downloaded.open('xb') as outgoing:
            while block := incoming.read(1024 * 1024):
                total += len(block)
                if total > release['archive_size']:
                    raise ValueError('Slack package exceeds its pinned size')
                digest.update(block)
                outgoing.write(block)
            outgoing.flush()
            os.fsync(outgoing.fileno())
        if total != release['archive_size'] or digest.hexdigest() != release['archive_sha256']:
            raise ValueError('Slack package checksum or size mismatch')
        # Exclusive publication: a concurrently created destination is preserved.
        os.link(downloaded, output)
    return {'target': target, 'version': release['version'], 'source': release['source'],
            'archive_sha256': release['archive_sha256'], 'archive_size': total}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True, choices=sorted(TARGETS))
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(fetch(args.target, args.output)))
