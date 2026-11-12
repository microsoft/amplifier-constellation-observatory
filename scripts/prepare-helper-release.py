#!/usr/bin/env python3
"""Stage six exact helper builds and their deterministic aggregate, without app assets."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import plistlib
import re
import shutil
import zipfile


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


aggregate = module('aggregate_helpers', 'package-machine-helpers.py')
verify = module('verify_helpers', 'stage-machine-helpers.py')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def ordinary(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError('Expected an ordinary release file: ' + path.name)


def prepare(source, output, version):
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        raise ValueError('Use an exact helper release version')
    if output.exists() and (output.is_symlink() or not output.is_dir() or any(output.iterdir())):
        raise ValueError('Use a new empty helper release staging directory')
    selected = []
    for platform in aggregate.PLATFORMS:
        name = f'constellation-machine-helper-{version}-{platform}.zip'
        archive, metadata, checksum = (source / name, source / (name + '.json'), source / (name + '.sha256'))
        for path in (archive, metadata, checksum):
            ordinary(path)
        record = json.loads(metadata.read_text(encoding='utf-8'))
        expected = {'platform': platform, 'version': version, 'filename': name,
                    'bootstrap_path': 'observatory-pairing.json', 'bootstrap_included': False,
                    'signed_or_notarized': False}
        if any(record.get(key) != value for key, value in expected.items()):
            raise ValueError('Helper release metadata differs: ' + name)
        actual = digest(archive)
        if record.get('sha256') != actual or checksum.read_text(encoding='utf-8') != actual + '  ' + name + '\n':
            raise ValueError('Helper release checksum differs: ' + name)
        executable = 'constellation-machine-helper.exe' if platform.startswith('windows') else 'constellation-machine-helper'
        if platform.startswith('macos'):
            executable = 'Constellation Computer.app/Contents/MacOS/' + executable
        names = {executable, 'START-HERE.txt', 'LICENSE', 'NUNITO-LICENSE.txt'}
        if platform.startswith('macos'):
            names.add('Constellation Computer.app/Contents/Info.plist')
        with zipfile.ZipFile(archive) as bundle:
            if len(bundle.namelist()) != len(names) or set(bundle.namelist()) != names:
                raise ValueError('Unexpected helper payload or embedded pairing setup')
            binary = bundle.getinfo(executable)
            if binary.file_size > 80 * 1024 * 1024 or hashlib.sha256(bundle.read(binary)).hexdigest() != record.get('binary_sha256'):
                raise ValueError('Helper executable differs from its checked metadata')
            if platform.startswith('macos'):
                info = plistlib.loads(bundle.read('Constellation Computer.app/Contents/Info.plist'))
                if info.get('CFBundleVersion') != version or info.get('CFBundleShortVersionString') != version:
                    raise ValueError('Mac helper bundle version differs')
        selected.extend((archive, metadata, checksum))
    output.mkdir(parents=True, exist_ok=True)
    for path in selected:
        shutil.copyfile(path, output / path.name)
    bundle = aggregate.package(output, version)
    verify.verified_files(bundle, digest(bundle))
    expected = {path.name for path in selected} | {bundle.name, bundle.name + '.sha256'}
    if len(expected) != 20 or {path.name for path in output.iterdir()} != expected:
        raise ValueError('Unexpected helper release asset set')
    return {path.name: digest(path) for path in sorted(output.iterdir())}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--version', required=True)
    args = parser.parse_args()
    print(json.dumps(prepare(args.directory, args.output, args.version), indent=2))
