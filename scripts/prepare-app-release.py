#!/usr/bin/env python3
"""Select the twelve checked Observatory apps and the unchanged published helpers.

Build/release tooling only. Consumer installs retain their packaged downloads and
never run this script. Existing published helper assets are not rebuilt/replaced.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import tarfile
import zipfile


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


packaging = module('observatory_packaging', 'package-observatory.py')
helpers = module('published_helpers', 'fetch-machine-helper-bundle.py')
helper_stage = module('helper_verifier', 'stage-machine-helpers.py')
HELPER_NAME = 'constellation-machine-helpers-0.2.0.zip'


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def ordinary(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError('Expected an ordinary release file: ' + path.name)


def archive_support(path, metadata, expected):
    """Check actual binary bytes and embedded support against independent pins."""
    archive = tarfile.open(path) if path.name.endswith('.tar.gz') else zipfile.ZipFile(path)
    prefix = str(Path(metadata['entry']).parent).replace('\\', '/')
    prefix = '' if prefix == '.' else prefix + '/'
    try:
        def read(name, limit):
            if isinstance(archive, zipfile.ZipFile):
                info = archive.getinfo(name)
                if info.file_size > limit:
                    raise ValueError('Oversized package member: ' + name)
                stream = archive.open(info)
            else:
                info = archive.getmember(name)
                if not info.isfile() or info.size > limit:
                    raise ValueError('Invalid package member: ' + name)
                stream = archive.extractfile(info)
            with stream:
                data = stream.read(limit + 1)
            if len(data) > limit:
                raise ValueError('Oversized package member: ' + name)
            return data
        binary = read(metadata['entry'], 256 * 1024 * 1024)
        if hashlib.sha256(binary).hexdigest() != metadata['binary_sha256']:
            raise ValueError('Packaged application bytes differ')
        manifest = json.loads(read(prefix + 'observatory-package.json', 65536))
        if manifest['binary_sha256'] != metadata['binary_sha256'] or manifest['target'] != metadata['target'] or manifest['version'] != metadata['version']:
            raise ValueError('Package inventory differs from release metadata')
        if metadata['includes_agent_setup']:
            if manifest.get('helpers_manifest_sha256') != hashlib.sha256(expected['manifest.json']).hexdigest():
                raise ValueError('Desktop helper inventory differs from the published pin')
            for name, data in sorted(expected.items()):
                actual = read(prefix + 'support/machine-helpers/' + name, len(data))
                if actual != data:
                    raise ValueError('Desktop helper bytes differ from the published pin')
    finally:
        archive.close()


def prepare(source, output, version, helper_bundle=None):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Use an exact release version')
    if output.exists() and (output.is_symlink() or not output.is_dir() or any(output.iterdir())):
        raise ValueError('Use a new empty release staging directory')
    output.mkdir(parents=True, exist_ok=True)
    # A local immutable copy is useful to the maintainer; it has the same pin.
    if helper_bundle is None:
        helper_bundle = helpers.fetch(output / HELPER_NAME)
    ordinary(helper_bundle)
    _, expected_support = helper_stage.verified_files(helper_bundle, helpers.SHA256)
    selected = []
    for target, (system, architecture) in packaging.TARGETS.items():
        extension = 'tar.gz' if system == 'linux' else 'zip'
        for desktop in (False, True):
            name = f'agent-observatory-{version}-{target}{"-desktop" if desktop else ""}.{extension}'
            path = source / name
            metadata_path = source / (name + '.json')
            checksum_path = source / (name + '.sha256')
            for file in (path, metadata_path, checksum_path):
                ordinary(file)
            metadata = json.loads(metadata_path.read_text(encoding='utf-8'))
            wanted = {'schema':1,'component':'agent-observatory','version':version,'target':target,
                'os':system,'architecture':architecture,'filename':name,
                'requires_developer_toolchain':False,'includes_agent_setup':desktop,
                'includes_computer_setup':desktop}
            if any(metadata.get(key) != value for key, value in wanted.items()):
                raise ValueError('Application release metadata differs: ' + name)
            entry = ('Observatory.app/Contents/MacOS/' if system == 'macos' else '') + ('agent-observatory.exe' if system == 'windows' else 'agent-observatory')
            if metadata.get('entry') != entry or not re.fullmatch(r'[0-9a-f]{64}', metadata.get('binary_sha256','')):
                raise ValueError('Unexpected native entrypoint or digest')
            actual = digest(path)
            if metadata.get('sha256') != actual or checksum_path.read_text(encoding='utf-8') != actual + '  ' + name + '\n':
                raise ValueError('Application release checksum differs: ' + name)
            archive_support(path, metadata, expected_support)
            selected.extend((path, metadata_path, checksum_path))
    for file in selected:
        shutil.copyfile(file, output / file.name)
    destination = output / HELPER_NAME
    if helper_bundle.resolve() != destination.resolve():
        shutil.copyfile(helper_bundle, destination)
    (output / (HELPER_NAME + '.sha256')).write_text(helpers.SHA256 + '  ' + HELPER_NAME + '\n', encoding='utf-8')
    expected = {file.name for file in selected} | {HELPER_NAME, HELPER_NAME + '.sha256'}
    if len(expected) != 38 or {file.name for file in output.iterdir()} != expected:
        raise ValueError('Unexpected application release asset set')
    return {file.name:digest(file) for file in sorted(output.iterdir())}


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--directory', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--version', required=True)
    p.add_argument('--helpers', type=Path)
    a = p.parse_args()
    print(json.dumps(prepare(a.directory, a.output, a.version, a.helpers), indent=2))
