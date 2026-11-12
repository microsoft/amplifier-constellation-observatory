#!/usr/bin/env python3
"""Check an x64 or ARM64 Windows executable that needs separate Visual C++ runtime DLLs."""
import argparse
import hashlib
import json
from pathlib import Path
import struct


def inspect(data, platform="windows-x86_64"):
    """Read ordinary and delay-load import tables without executing the PE file."""
    def unpack(fmt, offset):
        if offset < 0 or offset + struct.calcsize(fmt) > len(data):
            raise ValueError("Truncated PE metadata")
        return struct.unpack_from(fmt, data, offset)

    if len(data) > 80 * 1024 * 1024 or data[:2] != b"MZ":
        raise ValueError("Expected a bounded Windows executable")
    pe = unpack("<I", 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("Missing PE signature")
    machine, count = unpack("<HH", pe + 4)
    optional_size = unpack("<H", pe + 20)[0]
    optional = pe + 24
    expected={"windows-x86_64":0x8664,"windows-aarch64":0xAA64}.get(platform)
    if expected is None or machine != expected or unpack("<H", optional)[0] != 0x20B:
        raise ValueError("Expected the selected native PE32+ architecture")
    if not 0 < count <= 96 or optional_size < 112:
        raise ValueError("Invalid PE header sizes")
    image_base = unpack("<Q", optional + 24)[0]
    header_size = unpack("<I", optional + 60)[0]
    directory_count = unpack("<I", optional + 108)[0]
    if directory_count > (optional_size - 112) // 8:
        raise ValueError("Truncated PE directory table")
    sections = []
    for index in range(count):
        start = optional + optional_size + index * 40
        virtual_size, address, raw_size, raw = unpack("<IIII", start + 8)
        if raw + raw_size > len(data):
            raise ValueError("Truncated PE section")
        sections.append((address, virtual_size, raw, raw_size))

    def offset(rva, size):
        if rva < header_size and rva + size <= min(header_size, len(data)):
            return rva
        for address, _virtual_size, raw, raw_size in sections:
            delta = rva - address
            if 0 <= delta and delta + size <= raw_size:
                return raw + delta
        raise ValueError("Import metadata points outside stored PE sections")

    def dll_name(rva):
        result = bytearray()
        for index in range(256):
            value = data[offset(rva + index, 1)]
            if value == 0:
                if not result:
                    raise ValueError("Empty DLL import")
                return result.decode("ascii")
            result.append(value)
        raise ValueError("Unterminated DLL import")

    def imports(directory, width, delayed=False):
        if directory >= directory_count:
            return []
        address, size = unpack("<II", optional + 112 + directory * 8)
        if address == 0 and size == 0:
            return []
        if address == 0 or size < width or size > 1024 * width:
            raise ValueError("Invalid import directory size")
        names = []
        for index in range(size // width):
            values = unpack("<" + "I" * (width // 4), offset(address + index * width, width))
            if not any(values):
                return names
            if delayed:
                flags, name = values[:2]
                if flags not in (0, 1):
                    raise ValueError("Unknown delay-import address mode")
                name = name if flags == 1 else name - image_base
            else:
                name = values[3]
            names.append(dll_name(name))
        raise ValueError("Unterminated import directory")

    ordinary = imports(1, 20)
    delayed = imports(13, 32, delayed=True)
    external = sorted({name for name in ordinary + delayed
                       if name.lower().startswith(("vcruntime", "msvcp", "concrt", "vcomp"))})
    return {"binary_sha256": hashlib.sha256(data).hexdigest(),
            "architecture": platform.removeprefix("windows-"), "imports": ordinary, "delay_imports": delayed,
            "external_vc_runtime": external, "standalone_vc_runtime": not external}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--platform", choices=["windows-x86_64","windows-aarch64"], default="windows-x86_64")
    args = parser.parse_args()
    try:
        result = inspect(args.binary.read_bytes(), args.platform)
    except (ValueError, UnicodeError, OSError) as error:
        print(json.dumps({"error": str(error)}))
        return 1
    print(json.dumps(result, indent=2))
    return 1 if result["external_vc_runtime"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
