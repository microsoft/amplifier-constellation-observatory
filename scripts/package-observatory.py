#!/usr/bin/env python3
"""Package native Observatory binaries; downloaded apps need no build toolchain."""
import argparse
import gzip
import hashlib
import io
import importlib.util
import json
from pathlib import Path, PurePosixPath
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
SLACK_LOCK = Path(__file__).with_name('slack-bundle-lock.json')
MAX_SLACK_BYTES = 1024**3


def slack_files(bundle, target, lock_path=None):
    """Carry the one reviewed owner archive unchanged; native setup verifies its members."""
    if 'windows' not in target and 'apple' not in target:
        raise ValueError('Bundled Slack setup currently supports Windows and macOS only')
    if bundle.is_symlink() or not bundle.is_file():
        raise ValueError('Use an ordinary reviewed Slack archive')
    lock = json.loads((lock_path or SLACK_LOCK).read_text())
    if lock.get('schema') != 1:
        raise ValueError('Invalid reviewed Slack lock')
    release = lock.get('targets', {}).get(target, {}).get('release')
    if not isinstance(release, dict):
        raise ValueError('No reviewed Slack package is locked for this target')
    required = {'schema','source','version','target','archive_size','archive_sha256','entry','native_bundle_sha256','files'}
    entry = 'acp-slack-adapter.exe' if 'windows' in target else 'Slack Connector.app/Contents/MacOS/acp-slack-adapter'
    native = 'native-bundle.json' if 'windows' in target else 'Slack Connector.app/Contents/Resources/native-bundle.json'
    if (set(release) != required or release['schema'] != 1 or release['target'] != target
            or release['entry'] != entry or not re.fullmatch(r'[0-9a-f]{40}',release['source'])
            or not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)',release['version'])
            or not re.fullmatch(r'[0-9a-f]{64}',release['archive_sha256'])
            or not re.fullmatch(r'[0-9a-f]{64}',release['native_bundle_sha256'])
            or type(release['archive_size']) is not int or not 1 <= release['archive_size'] <= MAX_SLACK_BYTES
            or not isinstance(release['files'],dict) or not 5 <= len(release['files']) <= 20000):
        raise ValueError('Slack lock does not match the reviewed package contract')
    if bundle.stat().st_size != release['archive_size']:
        raise ValueError('Slack archive size differs from reviewed lock')
    data = bundle.read_bytes()
    if len(data) != release['archive_size'] or hashlib.sha256(data).hexdigest() != release['archive_sha256']:
        raise ValueError('Slack archive checksum differs from reviewed lock')
    # Do not unpack or rebuild the connector. Check the owner manifest in the
    # exact archive, then retain all bytes for slack_package's fixed ingestion.
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)) or set(names) != set(release['files']):
            raise ValueError('Slack archive inventory differs from reviewed lock')
        if archive.getinfo(native).file_size > 8*1024**2:
            raise ValueError('Excessive Slack native manifest')
        native_bytes = archive.read(native)
        if hashlib.sha256(native_bytes).hexdigest() != release['native_bundle_sha256']:
            raise ValueError('Slack native manifest checksum differs from reviewed lock')
        manifest = json.loads(native_bytes)
        if (manifest.get('schema') != 1 or manifest.get('component') != 'acp-slack-adapter'
                or manifest.get('version') != release['version'] or manifest.get('target') != target):
            raise ValueError('Slack native manifest target/version differs from reviewed lock')
    descriptor = (json.dumps(release,indent=2,sort_keys=True)+'\n').encode()
    binding = {key:release[key] for key in ('source','version','target','archive_size','archive_sha256')}
    binding.update(schema=1,descriptor_sha256=hashlib.sha256(descriptor).hexdigest())
    return binding, [('support/slack/descriptor.json',descriptor,0o644),
                     (f'support/slack/{target}.zip',data,0o644)]

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


def helper_files(bundle, expected):
    spec = importlib.util.spec_from_file_location('observatory_helper_stage', Path(__file__).with_name('stage-machine-helpers.py'))
    stage = importlib.util.module_from_spec(spec); spec.loader.exec_module(stage)
    manifest, files = stage.verified_files(bundle, expected)
    if {a['platform'] for a in manifest['artifacts']} != stage.PLATFORMS:
        raise ValueError('Desktop setup requires the complete six-platform helper bundle')
    return files


def package(binary, target, version, output, instance_bundle=None, machine_helpers=None, helpers_sha256=None, slack_bundle=None):
    if target not in TARGETS or not re.fullmatch(r'\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?', version):
        raise ValueError('Invalid target or release version')
    if not binary.is_file() or binary.is_symlink():
        raise ValueError('Use the ordinary native executable produced on this platform')
    system, architecture = TARGETS[target]
    data = binary.read_bytes()
    instance, bundled = instance_files(instance_bundle, target) if instance_bundle else (None, [])
    helpers = helper_files(machine_helpers, helpers_sha256) if machine_helpers else {}
    slack_binding, slack = slack_files(slack_bundle,target) if slack_bundle else (None,[])
    output.mkdir(parents=True, exist_ok=True)
    package_manifest = {'schema': 1, 'component': 'agent-observatory', 'version': version,
                        'target': target, 'binary': 'agent-observatory.exe' if system == 'windows' else 'agent-observatory',
                        'binary_sha256': hashlib.sha256(data).hexdigest()}
    if instance:
        package_manifest['instance_manifest_sha256'] = hashlib.sha256((instance_bundle / 'consumer-bundle.json').read_bytes()).hexdigest()
    if helpers:
        package_manifest['helpers_manifest_sha256'] = hashlib.sha256(helpers['manifest.json']).hexdigest()
    if slack_binding:
        package_manifest['slack_bundle'] = slack_binding
    manifest_bytes = (json.dumps(package_manifest, indent=2, sort_keys=True) + '\n').encode()
    filename = f'agent-observatory-{version}-{target}' + ('-desktop' if instance else '') + ('.tar.gz' if system == 'linux' else '.zip')
    destination = output / filename
    if system == 'linux':
        # Existing update clients expect exactly this member and archive name.
        with destination.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w') as archive:
                info = tarfile.TarInfo('agent-observatory')
                info.size = len(data); info.mode = 0o755; info.mtime = 0
                archive.addfile(info, io.BytesIO(data))
                info = tarfile.TarInfo('observatory-package.json')
                info.size = len(manifest_bytes); info.mode = 0o644; info.mtime = 0
                archive.addfile(info, io.BytesIO(manifest_bytes))
                for name, content, mode in bundled:
                    info = tarfile.TarInfo('support/instance/' + name)
                    info.size = len(content); info.mode = mode; info.mtime = 0
                    archive.addfile(info, io.BytesIO(content))
                for name, content in sorted(helpers.items()):
                    info = tarfile.TarInfo('support/machine-helpers/' + name)
                    info.size = len(content); info.mode = 0o644; info.mtime = 0
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
            put(archive, str(PurePosixPath(entry).parent / 'observatory-package.json'), manifest_bytes)
            prefix = 'support/instance/' if system == 'windows' else 'Observatory.app/Contents/MacOS/support/instance/'
            for name, content, mode in bundled:
                put(archive, prefix + name, content, mode)
            helper_prefix = str(PurePosixPath(entry).parent / 'support/machine-helpers') + '/'
            for name, content in sorted(helpers.items()):
                put(archive, helper_prefix + name, content)
            for name, content, mode in slack:
                put(archive, str(PurePosixPath(entry).parent / name),content,mode)
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
    record['includes_computer_setup'] = bool(helpers)
    record['includes_slack_setup'] = bool(slack)
    if slack_binding:
        record['slack_bundle'] = slack_binding
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
    parser.add_argument('--machine-helpers', type=Path, help='Complete checksum-verified helper release bundle')
    parser.add_argument('--helpers-sha256', help='Expected SHA256 of the helper release bundle')
    parser.add_argument('--slack-bundle', type=Path, help='Unchanged target Slack archive matching the compiled reviewed Slack lock')
    args = parser.parse_args()
    print(json.dumps(package(args.binary, args.target, args.version, args.output, args.instance_bundle, args.machine_helpers, args.helpers_sha256,args.slack_bundle), indent=2))
