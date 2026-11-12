#!/usr/bin/env python3
"""Verify a Linux helper's architecture and absence of dynamic runtime dependencies."""
import argparse
import hashlib
import json
from pathlib import Path
import struct


MACHINES = {"linux-x86_64": 62, "linux-aarch64": 183}


def inspect(data, platform):
    """Read ELF headers without executing the binary; static PIE is supported."""
    def unpack(fmt, offset):
        if offset < 0 or offset + struct.calcsize(fmt) > len(data):
            raise ValueError("Truncated ELF metadata")
        return struct.unpack_from(fmt, data, offset)

    if platform not in MACHINES:
        raise ValueError("Unsupported Linux helper platform")
    if len(data) > 80 * 1024 * 1024 or data[:7] != b"\x7fELF\x02\x01\x01":
        raise ValueError("Expected a bounded little-endian ELF64 executable")
    kind, machine, version = unpack("<HHI", 16)
    if kind not in (2, 3) or machine != MACHINES[platform] or version != 1:
        raise ValueError("Wrong ELF executable type or architecture")
    program_offset = unpack("<Q", 32)[0]
    header_size, program_size, count = unpack("<HHH", 52)
    if header_size != 64 or program_size != 56 or not 0 < count <= 1024:
        raise ValueError("Invalid ELF program header sizes")
    if program_offset < header_size or program_offset + count * program_size > len(data):
        raise ValueError("Truncated ELF program header table")
    interpreters = []
    needed = []
    for index in range(count):
        values = unpack("<IIQQQQQQ", program_offset + index * program_size)
        segment, _flags, offset, _address, _physical, size, memory_size, _align = values
        if offset + size > len(data) or (segment == 1 and memory_size < size):
            raise ValueError("Truncated ELF segment")
        if segment == 3:  # PT_INTERP: this binary needs a loader on the machine.
            if not 1 < size <= 4096 or data[offset + size - 1] != 0:
                raise ValueError("Invalid ELF interpreter path")
            interpreters.append(data[offset:offset + size - 1].decode("ascii"))
        if segment == 2:  # PT_DYNAMIC can exist in a self-contained static PIE.
            if not size or size % 16:
                raise ValueError("Invalid ELF dynamic table")
            terminated = False
            for entry in range(size // 16):
                tag, value = unpack("<qQ", offset + entry * 16)
                if tag == 0:
                    terminated = True
                    break
                if tag == 1:  # DT_NEEDED, independent of its string-table name.
                    needed.append(value)
            if not terminated:
                raise ValueError("Unterminated ELF dynamic table")
    return {"binary_sha256": hashlib.sha256(data).hexdigest(), "platform": platform,
            "elf_type": "position-independent" if kind == 3 else "executable",
            "interpreters": interpreters, "needed_entries": len(needed),
            "standalone_c_runtime": not interpreters and not needed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--platform", choices=sorted(MACHINES), required=True)
    args = parser.parse_args()
    try:
        result = inspect(args.binary.read_bytes(), args.platform)
    except (ValueError, UnicodeError, OSError) as error:
        print(json.dumps({"error": str(error)}))
        return 1
    print(json.dumps(result, indent=2))
    return 0 if result["standalone_c_runtime"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
