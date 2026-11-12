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

def package(binary, target, version, output):
    if target not in TARGETS or not re.fullmatch(r'\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?', version):
        raise ValueError('Invalid target or release version')
    if not binary.is_file() or binary.is_symlink():
        raise ValueError('Use the ordinary native executable produced on this platform')
    system, architecture = TARGETS[target]
    data = binary.read_bytes()
    output.mkdir(parents=True, exist_ok=True)
    filename = f'agent-observatory-{version}-{target}' + ('.tar.gz' if system == 'linux' else '.zip')
    destination = output / filename
    if system == 'linux':
        # Existing update clients expect exactly this member and archive name.
        with destination.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                info = tarfile.TarInfo('agent-observatory')
                info.size = len(data); info.mode = 0o755; info.mtime = 0
                archive.addfile(info, io.BytesIO(data))
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
    (output / (filename + '.sha256')).write_text(digest + '  ' + filename + '\n')
    (output / (filename + '.json')).write_text(json.dumps(record, indent=2) + '\n')
    return record

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--target', required=True, choices=sorted(TARGETS))
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=Path, default=Path('dist'))
    args = parser.parse_args()
    print(json.dumps(package(args.binary, args.target, args.version, args.output), indent=2))
