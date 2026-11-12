#!/usr/bin/env python3
"""Build-time download of the exact published computer-helper closure."""
import argparse
import hashlib
import importlib.util
from pathlib import Path
import urllib.request

URL = 'https://github.com/microsoft/amplifier-constellation-observatory/releases/download/machine-helper-v0.2.1/constellation-machine-helpers-0.2.1.zip'
SHA256 = '212aa69c7ec3db9a682a5f7457d6fdefac5ba7b636598210d84bae001363fa91'
SIZE = 15_659_517

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
