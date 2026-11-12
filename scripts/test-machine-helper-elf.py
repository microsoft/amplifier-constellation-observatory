#!/usr/bin/env python3
"""Synthetic ELF boundaries; these bytes are never executed."""
import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location("elf_guard", Path(__file__).with_name("check-machine-helper-elf.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def elf(machine=62, kind=2, interpreter=None, needed=False, dynamic=False):
    data = bytearray(0x500)
    data[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHI", data, 16, kind, machine, 1)
    struct.pack_into("<Q", data, 32, 64)
    segments = [(1, 0, len(data))]
    if interpreter:
        text = interpreter.encode() + b"\0"
        data[0x300:0x300 + len(text)] = text
        segments.append((3, 0x300, len(text)))
    if dynamic or needed:
        struct.pack_into("<qQ", data, 0x400, 1 if needed else 5, 100)
        segments.append((2, 0x400, 32))
    struct.pack_into("<HHH", data, 52, 64, 56, len(segments))
    for index, (segment, offset, size) in enumerate(segments):
        struct.pack_into("<IIQQQQQQ", data, 64 + index * 56,
                         segment, 5, offset, 0, 0, size, size, 4096)
    return data


class ElfTests(unittest.TestCase):
    def test_native_static_binaries_for_both_architectures_pass(self):
        for platform, machine in guard.MACHINES.items():
            with self.subTest(platform=platform):
                self.assertTrue(guard.inspect(elf(machine), platform)["standalone_c_runtime"])

    def test_dynamic_loader_refuses_even_without_needed_table(self):
        result = guard.inspect(elf(interpreter="/lib64/ld-linux-x86-64.so.2"), "linux-x86_64")
        self.assertEqual(result["interpreters"], ["/lib64/ld-linux-x86-64.so.2"])
        self.assertFalse(result["standalone_c_runtime"])

    def test_needed_entry_refuses_even_without_interpreter(self):
        result = guard.inspect(elf(needed=True), "linux-x86_64")
        self.assertEqual(result["needed_entries"], 1)
        self.assertFalse(result["standalone_c_runtime"])

    def test_static_pie_relocation_metadata_is_allowed(self):
        result = guard.inspect(elf(kind=3, dynamic=True), "linux-x86_64")
        self.assertEqual(result["elf_type"], "position-independent")
        self.assertTrue(result["standalone_c_runtime"])

    def test_unused_loader_string_is_not_a_dependency(self):
        data = elf()
        data[0x300:0x310] = b"/lib/ld-musl.so\0"
        self.assertTrue(guard.inspect(data, "linux-x86_64")["standalone_c_runtime"])

    def test_architecture_and_malformed_headers_fail_closed(self):
        candidates = [b"not ELF", elf()[:80], elf(machine=183), elf(kind=1)]
        bad_table = elf()
        struct.pack_into("<Q", bad_table, 32, 0xFFFF)
        candidates.append(bad_table)
        bad_segment = elf()
        struct.pack_into("<Q", bad_segment, 64 + 32, 0xFFFF)
        candidates.append(bad_segment)
        for data in candidates:
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                guard.inspect(data, "linux-x86_64")

    def test_unterminated_dynamic_table_fails_closed(self):
        data = elf(dynamic=True)
        struct.pack_into("<qQ", data, 0x410, 5, 100)
        with self.assertRaises(ValueError):
            guard.inspect(data, "linux-x86_64")


if __name__ == "__main__":
    unittest.main(verbosity=2)
