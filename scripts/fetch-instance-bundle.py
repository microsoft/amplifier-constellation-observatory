#!/usr/bin/env python3
"""Build-time composition of an exact published, self-contained Instance package."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import stat
import tempfile
import urllib.request
import zipfile


def unpack(archive, output, *, case_sensitive=False):
    if output.exists():
        raise ValueError('Use a new package staging directory')
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='instance-package-', dir=output.parent) as temporary:
        root = Path(temporary) / 'bundle'; root.mkdir()
        with zipfile.ZipFile(archive) as source:
            members = source.infolist()
            if len(members) > 20000 or sum(m.file_size for m in members) > 1024**3:
                raise ValueError('Excessive Instance package')
            seen = set()
            for member in members:
                parts = member.filename.split('/')
                key = member.filename if case_sensitive else member.filename.casefold()
                if (member.is_dir() or not member.filename or '\\' in member.filename
                        or ':' in member.filename or any(p in ('', '.', '..') for p in parts)
                        or key in seen
                        or stat.S_IFMT(member.external_attr >> 16) not in (0, stat.S_IFREG)):
                    raise ValueError('Invalid Instance package member')
                seen.add(key)
                target = root.joinpath(*parts); target.parent.mkdir(parents=True, exist_ok=True)
                with source.open(member) as incoming, target.open('xb') as outgoing:
                    shutil.copyfileobj(incoming, outgoing)
                target.chmod((member.external_attr >> 16) & 0o777 or 0o644)
        root.rename(output)


def fetch(target, output, lock_path=None):
    lock = json.loads((lock_path or Path(__file__).with_name('instance-bundle-lock.json')).read_text())
    if lock.get('repository') != 'microsoft/amplifier-constellation-instance':
        raise ValueError('Unexpected package owner')
    version = lock['version']; asset = lock['targets'][target]
    filename = f'agent-instance-consumer-{target}.zip'
    if not re.fullmatch(r'\d+\.\d+\.\d+', version) or not re.fullmatch(r'[a-f0-9]{64}', asset['sha256']):
        raise ValueError('Invalid package pin')
    url = f'https://github.com/{lock["repository"]}/releases/download/v{version}/{filename}'
    with tempfile.TemporaryDirectory(prefix='instance-download-') as temporary:
        archive = Path(temporary) / filename
        with urllib.request.urlopen(url, timeout=60) as source, archive.open('xb') as destination:
            total = 0
            while block := source.read(1024 * 1024):
                total += len(block)
                if total > 1024**3:
                    raise ValueError('Excessive package download')
                destination.write(block)
        with archive.open('rb') as downloaded:
            digest = hashlib.file_digest(downloaded, 'sha256').hexdigest()
        if digest != asset['sha256']:
            raise ValueError('Published Instance package checksum mismatch')
        # The Linux distributor includes distinct case-sensitive terminfo names.
        # Preserve them; Windows and default Mac filesystems reject case aliases.
        unpack(archive, output, case_sensitive=target.endswith('-unknown-linux-gnu'))
    manifest = json.loads((output / 'consumer-bundle.json').read_text())
    if manifest['source'] != lock['source'] or manifest['version'] != version or manifest['target'] != target:
        raise ValueError('Published Instance package identity mismatch')
    print(json.dumps({'target': target, 'instance_version': version, 'source': manifest['source'],
                      'archive_sha256': asset['sha256']}))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    fetch(args.target, args.output)
