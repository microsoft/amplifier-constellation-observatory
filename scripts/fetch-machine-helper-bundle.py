#!/usr/bin/env python3
"""Build-time download of the exact published computer-helper closure."""
import argparse
import hashlib
import importlib.util
from pathlib import Path
import urllib.request

URL = 'https://github.com/microsoft/amplifier-constellation-observatory/releases/download/v0.11.0/constellation-machine-helpers-0.2.0.zip'
SHA256 = 'cb15bc36a15ab74b34c04af8647433835bce0dfb4c13e08b46b73e6268dce6dc'
SIZE = 15_652_320

def fetch(output):
    if output.exists():
        if output.is_symlink() or not output.is_file() or hashlib.sha256(output.read_bytes()).hexdigest() != SHA256:
            raise ValueError('Existing helper bundle differs; it was preserved')
        return output
    request = urllib.request.Request(URL, headers={'User-Agent': 'Constellation-native-package'})
    with urllib.request.urlopen(request, timeout=120) as response:
        data = response.read(SIZE + 1)
    if len(data) != SIZE or hashlib.sha256(data).hexdigest() != SHA256:
        raise ValueError('Published computer-helper bundle did not match its pinned bytes')
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open('xb') as stream:
        stream.write(data)
    spec = importlib.util.spec_from_file_location('helper_stage', Path(__file__).with_name('stage-machine-helpers.py'))
    stage = importlib.util.module_from_spec(spec); spec.loader.exec_module(stage)
    stage.verified_files(output, SHA256)
    return output

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    print(fetch(args.output))
