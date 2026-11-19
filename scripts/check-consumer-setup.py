#!/usr/bin/env python3
"""Bind CI-owned tools before one actual packaged Windows/macOS setup fixture.

Node, Playwright and Chromium are test drivers, never consumer prerequisites.
The invoked fixture separately gives application processes only OS tools in PATH.
This launcher installs nothing and never retries the product journey.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run_fixture(command, evidence):
    if os.name != 'nt' or os.environ.get('GITHUB_ACTIONS') != 'true' or os.environ.get('RUNNER_ARCH') != 'ARM64':
        return subprocess.call(command, cwd=ROOT)
    trace = evidence.with_suffix('.hosted-process-starts')
    observer = None
    try:
        trace.mkdir()
        system = Path(os.environ['SystemRoot'])
        shell = system / 'System32/WindowsPowerShell/v1.0/powershell.exe'
        environment = {k: os.environ[k] for k in ('SystemRoot','WINDIR','TEMP','TMP','USERPROFILE','COMPUTERNAME') if k in os.environ}
        observer = subprocess.Popen([str(shell), '-NoLogo', '-NoProfile', '-NonInteractive', '-File',
                                     str(ROOT / 'scripts/observe-hosted-wsl.ps1'), str(trace)],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                    env=environment, creationflags=subprocess.CREATE_NO_WINDOW)
        deadline = time.monotonic() + 5
        while observer.poll() is None and not (trace / 'ready').exists() and time.monotonic() < deadline:
            time.sleep(.05)
    except OSError as error:
        print('Hosted WSL origin observer unavailable:', type(error).__name__, file=sys.stderr)
    try:
        return subprocess.call(command, cwd=ROOT)
    finally:
        if observer is not None:
            try:
                (trace / 'stop').touch(exist_ok=False)
                observer.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired) as error:
                print('Hosted WSL origin observation incomplete:', type(error).__name__, file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--mac-launch-services", action="store_true",
                        help="Launch the exact Mac application through LaunchServices")
    args = parser.parse_args()
    if args.mac_launch_services and sys.platform != "darwin":
        raise ValueError("LaunchServices qualification requires macOS")
    metadata = args.metadata.resolve(strict=True)
    evidence = args.evidence.resolve()
    if evidence.exists() or evidence.with_suffix(".inputs.json").exists():
        raise ValueError("Use an unused evidence destination; no fixture replay")
    node = shutil.which("node")
    if not node:
        raise ValueError("CI test driver Node is unavailable")
    node = Path(node).resolve(strict=True)
    module = (ROOT / "node_modules/playwright/index.mjs").resolve(strict=True)
    script = ("import {chromium} from 'playwright'; "
              "console.log(JSON.stringify({browser:chromium.executablePath(),version:process.versions.node}));")
    tools = json.loads(subprocess.check_output(
        [str(node), "--input-type=module", "-e", script],
        cwd=ROOT, text=True, encoding="utf-8", timeout=15))
    major, minor, *_ = map(int, tools["version"].split("."))
    if (major, minor) < (22, 13):
        raise ValueError("The fixture requires Node 22.13 or later for its read-only SQLite observer")
    browser = Path(tools["browser"]).resolve(strict=True)
    bindings = {"schema": 1, "role": "qualification tools only", "node_version": tools["version"],
                "inputs": {name: {"path": str(file), "sha256": digest(file)}
                           for name, file in (("metadata", metadata), ("node", node),
                                              ("playwright", module), ("browser", browser))}}
    evidence.parent.mkdir(parents=True, exist_ok=True)
    with evidence.with_suffix(".inputs.json").open("x", encoding="utf-8") as stream:
        json.dump(bindings, stream, indent=2)
        stream.write("\n")
    command = [sys.executable, str(ROOT / "tests/consumer_setup_live.py"), "--run",
               "--metadata", str(metadata), "--metadata-sha256", bindings["inputs"]["metadata"]["sha256"],
               "--evidence", str(evidence)]
    if args.mac_launch_services:
        command.append("--mac-launch-services")
    for name, path_flag, hash_flag in (("node", "--node", "--node-sha256"),
                                       ("playwright", "--playwright-module", "--playwright-sha256"),
                                       ("browser", "--browser", "--browser-sha256")):
        row = bindings["inputs"][name]
        command += [path_flag, row["path"], hash_flag, row["sha256"]]
    raise SystemExit(run_fixture(command, evidence))


if __name__ == "__main__":
    main()
