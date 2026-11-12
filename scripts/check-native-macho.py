#!/usr/bin/env python3
"""Verify native single-architecture Mach-O imports use only macOS system libraries."""
import argparse, hashlib, json, struct
from pathlib import Path

def inspect(data, architecture):
    expected={'x86_64':0x01000007, 'aarch64':0x0100000c}.get(architecture)
    if len(data)<32 or len(data)>120*1024*1024: raise ValueError('Expected a bounded native Mach-O executable')
    magic,cpu,_subtype,filetype,count,size,_flags,_reserved=struct.unpack_from('<8I',data)
    if magic!=0xfeedfacf or cpu!=expected or filetype!=2: raise ValueError('Expected the selected native Mach-O executable architecture')
    if count>4096 or size>len(data)-32: raise ValueError('Invalid Mach-O load commands')
    at=32; imports=[]
    for _ in range(count):
        if at+8>32+size:raise ValueError('Truncated Mach-O command')
        command,length=struct.unpack_from('<II',data,at)
        if length<8 or at+length>32+size:raise ValueError('Invalid Mach-O command length')
        if command in (0xc,0x80000018,0x8000001f,0x80000023,0x20):
            if length<24:raise ValueError('Truncated Mach-O library command')
            offset=struct.unpack_from('<I',data,at+8)[0]
            if not 24<=offset<length:raise ValueError('Invalid Mach-O library name')
            raw=data[at+offset:at+length]
            if b'\0' not in raw:raise ValueError('Unterminated Mach-O library name')
            imports.append(raw.split(b'\0',1)[0].decode('utf-8'))
        at+=length
    if at!=32+size:raise ValueError('Unexpected Mach-O command bytes')
    external=[name for name in imports if not name.startswith(('/usr/lib/','/System/Library/'))]
    return {'binary_sha256':hashlib.sha256(data).hexdigest(),'architecture':architecture,
            'imports':imports,'external_libraries':external,'system_libraries_only':not external}
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',required=True,type=Path);p.add_argument('--architecture',required=True,choices=['x86_64','aarch64']);a=p.parse_args()
    try: result=inspect(a.binary.read_bytes(),a.architecture)
    except (ValueError,UnicodeError,OSError,struct.error) as e:print(json.dumps({'error':str(e)}));raise SystemExit(1)
    print(json.dumps(result,indent=2));raise SystemExit(0 if result['system_libraries_only'] else 1)
