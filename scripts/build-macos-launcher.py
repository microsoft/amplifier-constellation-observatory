#!/usr/bin/env python3
"""Build the fixed GUI entry point on macOS; never sign or execute it."""
import argparse
import importlib.util
from pathlib import Path
import subprocess
import sys

TARGETS = {'x86_64-apple-darwin': ('x86_64', 'x86_64'),
           'aarch64-apple-darwin': ('arm64', 'aarch64')}
SOURCE = Path(__file__).with_name('macos') / 'Observatory.m'


def read_launcher(path, target):
    if target not in TARGETS:
        raise ValueError('Use an exact supported Mac target')
    if path.is_symlink() or not path.is_file() or not 32 <= path.stat().st_size <= 120 * 1024**2:
        raise ValueError('Use an ordinary bounded native Mac launcher')
    data = path.read_bytes()
    spec = importlib.util.spec_from_file_location('launcher_macho', Path(__file__).with_name('check-native-macho.py'))
    macho = importlib.util.module_from_spec(spec); spec.loader.exec_module(macho)
    report = macho.inspect(data, TARGETS[target][1])
    if not report['system_libraries_only']:
        raise ValueError('Mac launcher imports a non-system library')
    return data


def build(target, output):
    if target not in TARGETS:
        raise ValueError('Use an exact supported Mac target')
    if sys.platform != 'darwin':
        raise ValueError('Build the Mac launcher on macOS, or supply an exact compiled launcher')
    if output.exists() or output.is_symlink():
        raise ValueError('Existing launcher output was preserved')
    if not output.parent.is_dir() or output.parent.is_symlink():
        raise ValueError('Use an existing ordinary build output directory')
    subprocess.run(['/usr/bin/xcrun', '--sdk', 'macosx', 'clang',
                    '-arch', TARGETS[target][0], '-mmacosx-version-min=11.0',
                    '-fobjc-arc', '-O2', '-Wall', '-Wextra', '-Werror',
                    '-framework', 'AppKit', str(SOURCE), '-o', str(output)],
                   check=True, stdin=subprocess.DEVNULL)
    read_launcher(output, target)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--target', required=True, choices=sorted(TARGETS))
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    build(args.target, args.output)
