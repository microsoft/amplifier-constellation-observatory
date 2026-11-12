#!/usr/bin/env python3
"""Package native Observatory binaries; downloaded apps need no build toolchain."""
import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import plistlib
import re
import stat
import tarfile
import zipfile

TARGETS = {
    'x86_64-unknown-linux-gnu': ('linux', 'x86_64'),
    'aarch64-unknown-linux-gnu': ('linux', 'aarch64'),
    'x86_64-apple-darwin': ('macos', 'x86_64'),
    'aarch64-apple-darwin': ('macos', 'aarch64'),
    'x86_64-pc-windows-msvc': ('windows', 'x86_64'),
    'aarch64-pc-windows-msvc': ('windows', 'aarch64'),
}

def instance_files(root, target):
    """Preserve the Instance owner's complete package, never rebuild its setup."""
    root = root.resolve()
    manifest = json.loads((root / 'consumer-bundle.json').read_text())
    if (manifest.get('schema') != 1 or manifest.get('kind') != 'instance-consumer-bundle'
            or manifest.get('target') != target or not manifest.get('native_runtime')):
        raise ValueError('Use a complete matching Instance package with its pinned engine')
    files = manifest.get('files', {})
    if not isinstance(files, dict) or not 1 <= len(files) <= 20000:
        raise ValueError('Invalid Instance package inventory')
    entries = []
    total = 0
    for name, digest in files.items():
        parts = name.split('/')
        if not name or '\\' in name or ':' in name or any(p in ('', '.', '..') for p in parts):
            raise ValueError('Invalid Instance package member')
        path = root
        for part in parts:
            path /= part
            if path.is_symlink():
                raise ValueError('Instance package links are not supported')
        if not path.is_file():
            raise ValueError('Incomplete Instance package')
        data = path.read_bytes(); total += len(data)
        if total > 1024**3 or hashlib.sha256(data).hexdigest() != digest:
            raise ValueError('Instance package integrity check failed')
        entries.append((name, data, 0o755 if path.stat().st_mode & 0o111 else 0o644))
    entries.append(('consumer-bundle.json', (root / 'consumer-bundle.json').read_bytes(), 0o644))
    return manifest, entries


def package(binary, target, version, output, instance_bundle=None):
    if target not in TARGETS or not re.fullmatch(r'\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?', version):
        raise ValueError('Invalid target or release version')
    if not binary.is_file() or binary.is_symlink():
        raise ValueError('Use the ordinary native executable produced on this platform')
    system, architecture = TARGETS[target]
    data = binary.read_bytes()
    output.mkdir(parents=True, exist_ok=True)
    instance, bundled = instance_files(instance_bundle, target) if instance_bundle else (None, [])
    filename = f'agent-observatory-{version}-{target}' + ('-desktop' if instance else '') + ('.tar.gz' if system == 'linux' else '.zip')
    destination = output / filename
    if system == 'linux':
        # Existing update clients expect exactly this member and archive name.
        with destination.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                info = tarfile.TarInfo('agent-observatory')
                info.size = len(data); info.mode = 0o755; info.mtime = 0
                archive.addfile(info, io.BytesIO(data))
                for name, content, mode in bundled:
                    info = tarfile.TarInfo('support/instance/' + name)
                    info.size = len(content); info.mode = mode; info.mtime = 0
                    archive.addfile(info, io.BytesIO(content))
        entry = 'agent-observatory'
    else:
        entry = 'agent-observatory.exe' if system == 'windows' else 'Observatory.app/Contents/MacOS/agent-observatory'
        def put(archive, name, content, mode=0o644):
            info = zipfile.ZipInfo(name); info.create_system = 3
            info.external_attr = (stat.S_IFREG | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, content)
        with zipfile.ZipFile(destination, 'w') as archive:
            put(archive, entry, data, 0o755)
            prefix = 'support/instance/' if system == 'windows' else 'Observatory.app/Contents/MacOS/support/instance/'
            for name, content, mode in bundled:
                put(archive, prefix + name, content, mode)
            if system == 'macos':
                put(archive, 'Observatory.app/Contents/Info.plist', plistlib.dumps({
                    'CFBundleName': 'Observatory', 'CFBundleDisplayName': 'Observatory',
                    'CFBundleIdentifier': 'com.microsoft.constellation.observatory',
                    'CFBundleExecutable': 'agent-observatory', 'CFBundleVersion': version,
                    'CFBundleShortVersionString': version, 'CFBundlePackageType': 'APPL',
                    'LSMinimumSystemVersion': '11.0', 'LSUIElement': True,
                }))
            put(archive, 'START-HERE.txt', (
                'Open Observatory.app on Mac, or agent-observatory.exe on Windows.\n'
                'Observatory opens local setup in your browser. No Python, Rust, WSL or terminal is required.\n'
                'Your agents keep running independently when Observatory is closed.\n'
                'This preview is not publisher-signed or notarized. The operating system may ask you to review the download.\n'
                'Verify the published checksum and source. Do not disable system security controls.\n'
            ).encode())
            root = Path(__file__).resolve().parents[1]
            put(archive, 'LICENSE', (root / 'LICENSE').read_bytes())
            put(archive, 'NUNITO-LICENSE.txt', (root / 'machine-helper/ui/NUNITO-LICENSE.txt').read_bytes())
    digest = hashlib.sha256(destination.read_bytes()).hexdigest()
    record = {'schema': 1, 'component': 'agent-observatory', 'version': version, 'target': target,
              'os': system, 'architecture': architecture, 'filename': filename, 'entry': entry,
              'sha256': digest, 'binary_sha256': hashlib.sha256(data).hexdigest(),
              'signed_or_notarized': False, 'requires_developer_toolchain': False}
    record['includes_agent_setup'] = instance is not None
    if instance:
        record['instance_version'] = instance['version']
        record['instance_source'] = instance['source']
    (output / (filename + '.sha256')).write_text(digest + '  ' + filename + '\n')
    (output / (filename + '.json')).write_text(json.dumps(record, indent=2) + '\n')
    return record

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--target', required=True, choices=sorted(TARGETS))
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, default=Path('dist'))
    parser.add_argument('--instance-bundle', type=Path, help='Verified extracted complete Instance package for this target')
    args = parser.parse_args()
    print(json.dumps(package(args.binary, args.target, args.version, args.output, args.instance_bundle), indent=2))
