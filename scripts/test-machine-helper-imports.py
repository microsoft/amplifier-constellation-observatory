#!/usr/bin/env python3
"""Synthetic PE table boundaries; these bytes are never executed."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location("imports_guard", Path(__file__).with_name("check-machine-helper-imports.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def pe(ordinary="KERNEL32.dll", delayed=None):
    data = bytearray(0xA00)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HH", data, 0x84, 0x8664, 1)
    struct.pack_into("<H", data, 0x94, 240)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<Q", data, optional + 24, 0x140000000)
    struct.pack_into("<I", data, optional + 60, 0x200)
    struct.pack_into("<I", data, optional + 108, 16)
    section = optional + 240
    data[section:section + 8] = b".rdata\0\0"
    struct.pack_into("<IIII", data, section + 8, 0x800, 0x1000, 0x800, 0x200)
    if ordinary:
        struct.pack_into("<II", data, optional + 112 + 8, 0x1000, 40)
        struct.pack_into("<IIIII", data, 0x200, 0, 0, 0, 0x1100, 0)
        data[0x300:0x300 + len(ordinary) + 1] = ordinary.encode() + b"\0"
    if delayed:
        struct.pack_into("<II", data, optional + 112 + 13 * 8, 0x1200, 64)
        struct.pack_into("<8I", data, 0x400, 1, 0x1300, 0, 0, 0, 0, 0, 0)
        data[0x500:0x500 + len(delayed) + 1] = delayed.encode() + b"\0"
    return data


class ImportTests(unittest.TestCase):
    def test_native_arm_is_verified_only_with_matching_platform(self):
        data=pe();struct.pack_into("<H",data,0x84,0xAA64)
        result=guard.inspect(data,"windows-aarch64")
        self.assertEqual(result['architecture'],'aarch64')
        self.assertTrue(result['standalone_vc_runtime'])
        with self.assertRaises(ValueError):guard.inspect(pe(),'windows-aarch64')
        dynamic=pe("VCRUNTIME140.dll");struct.pack_into("<H",dynamic,0x84,0xAA64)
        self.assertFalse(guard.inspect(dynamic,'windows-aarch64')['standalone_vc_runtime'])

    def test_dynamic_vcruntime_import_is_rejected(self):
        result = guard.inspect(pe("VCRUNTIME140.dll"))
        self.assertEqual(result["external_vc_runtime"], ["VCRUNTIME140.dll"])
        self.assertFalse(result["standalone_vc_runtime"])

    def test_delay_loaded_msvcp_is_also_rejected_case_insensitively(self):
        result = guard.inspect(pe(delayed="mSvCp140_2.DLL"))
        self.assertEqual(result["delay_imports"], ["mSvCp140_2.DLL"])
        self.assertFalse(result["standalone_vc_runtime"])

    def test_windows_and_ucrt_os_imports_are_allowed(self):
        for name in ["KERNEL32.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "ucrtbase.dll"]:
            with self.subTest(name=name):
                self.assertTrue(guard.inspect(pe(name))["standalone_vc_runtime"])

    def test_unused_strings_do_not_become_imports(self):
        data = pe()
        data[0x800:0x810] = b"VCRUNTIME140.dll"
        self.assertTrue(guard.inspect(data)["standalone_vc_runtime"])

    def test_malformed_or_out_of_bounds_import_metadata_fails_closed(self):
        for data in [b"not PE", pe()[:100], pe()[:0x210]]:
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                guard.inspect(data)
        data = pe()
        struct.pack_into("<I", data, 0x200 + 12, 0x90000000)
        with self.assertRaises(ValueError):
            guard.inspect(data)

    def test_wrong_architecture_is_not_certified_as_x64(self):
        data = pe()
        struct.pack_into("<H", data, 0x84, 0xAA64)
        with self.assertRaises(ValueError):
            guard.inspect(data)


if __name__ == "__main__":
    unittest.main(verbosity=2)
