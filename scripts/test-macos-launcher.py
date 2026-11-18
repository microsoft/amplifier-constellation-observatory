#!/usr/bin/env python3
"""Launcher build/argument boundaries; no Mac application is executed."""
import importlib.util
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('launcher', Path(__file__).with_name('build-macos-launcher.py'))
launcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(launcher)


def inert(cpu):
    return struct.pack('<8I', 0xfeedfacf, cpu, 0, 2, 0, 0, 0, 0)


class LauncherTests(unittest.TestCase):
    def test_supplied_launcher_is_ordinary_native_exact_target_and_unchanged(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            file = root / 'launcher'
            data = inert(0x01000007)
            file.write_bytes(data)
            self.assertEqual(launcher.read_launcher(file, 'x86_64-apple-darwin'), data)
            with self.assertRaises(ValueError):
                launcher.read_launcher(file, 'aarch64-apple-darwin')
            link = root / 'link'
            link.symlink_to(file)
            with self.assertRaisesRegex(ValueError, 'ordinary'):
                launcher.read_launcher(link, 'x86_64-apple-darwin')
            file.write_bytes(b'not a native launcher')
            with self.assertRaises(ValueError):
                launcher.read_launcher(file, 'x86_64-apple-darwin')

    def test_supplied_launcher_rejects_non_system_imports(self):
        name = b'@rpath/arbitrary.dylib\0'
        size = (24 + len(name) + 7) // 8 * 8
        command = struct.pack('<6I', 0xc, size, 24, 0, 0, 0) + name + bytes(size - 24 - len(name))
        data = struct.pack('<8I', 0xfeedfacf, 0x01000007, 0, 2, 1, len(command), 0, 0) + command
        with tempfile.TemporaryDirectory() as temporary:
            file = Path(temporary) / 'launcher'
            file.write_bytes(data)
            with self.assertRaisesRegex(ValueError, 'non-system'):
                launcher.read_launcher(file, 'x86_64-apple-darwin')

    def test_build_is_fixed_native_compile_only_and_checks_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'Observatory'
            def compile_only(argv, **options):
                self.assertEqual(argv[:4], ['/usr/bin/xcrun', '--sdk', 'macosx', 'clang'])
                self.assertEqual(argv[argv.index('-arch') + 1], 'arm64')
                self.assertEqual(argv[-3:], [str(launcher.SOURCE), '-o', str(output)])
                self.assertIn('-mmacosx-version-min=11.0', argv)
                self.assertIn('AppKit', argv)
                self.assertEqual(options, {'check': True, 'stdin': subprocess.DEVNULL})
                output.write_bytes(inert(0x0100000c))
            with mock.patch.object(launcher.sys, 'platform', 'darwin'), mock.patch.object(launcher.subprocess, 'run', side_effect=compile_only) as call:
                launcher.build('aarch64-apple-darwin', output)
                self.assertEqual(call.call_count, 1)
                with self.assertRaisesRegex(ValueError, 'preserved'):
                    launcher.build('aarch64-apple-darwin', output)
                self.assertEqual(call.call_count, 1)
            self.assertEqual(output.read_bytes(), inert(0x0100000c))

    def test_non_mac_build_refuses_without_output_or_compiler(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'Observatory'
            with mock.patch.object(launcher.sys, 'platform', 'linux'), mock.patch.object(launcher.subprocess, 'run') as call:
                with self.assertRaisesRegex(ValueError, 'macOS'):
                    launcher.build('x86_64-apple-darwin', output)
            call.assert_not_called()
            self.assertFalse(output.exists())

    def test_portable_argument_parser_permits_only_fixed_startup_options(self):
        compiler = shutil.which('cc')
        if compiler is None:
            self.skipTest('Portable C compiler unavailable; no native Mac launch is implied')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'argument-check.c'
            source.write_text('#include "launcher_arguments.h"\nint main(int argc, char **argv) { const char *home; bool no_browser; return observatory_arguments(argc, argv, &home, &no_browser) ? 0 : 1; }\n')
            executable = root / 'argument-check'
            subprocess.run([compiler, '-std=c11', '-Wall', '-Wextra', '-Werror', '-I', str(launcher.SOURCE.parent), str(source), '-o', str(executable)], check=True, capture_output=True)
            accepted = [[], ['--home', '/tmp/space and café'], ['--no-browser'], ['--home', '/tmp/fresh', '--no-browser'], ['--no-browser', '--home', '/tmp/fresh']]
            refused = [['serve'], ['--home'], ['--home', 'relative'], ['--home', ''], ['--home', '/one', '--home', '/two'], ['--no-browser', '--no-browser'], ['--home=/tmp/one'], ['--executable', '/tmp/other']]
            for arguments in accepted:
                self.assertEqual(subprocess.run([str(executable), *arguments], check=False).returncode, 0, arguments)
            for arguments in refused:
                self.assertEqual(subprocess.run([str(executable), *arguments], check=False).returncode, 1, arguments)


if __name__ == '__main__':
    unittest.main(verbosity=2)
