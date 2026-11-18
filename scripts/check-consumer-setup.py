#!/usr/bin/env python3
"""Bind CI-owned tools before one actual packaged Windows/macOS setup fixture.

Node, Playwright and Chromium are test drivers, never consumer prerequisites.
The invoked fixture separately gives application processes only OS tools in PATH.
This launcher installs nothing and never retries the product journey.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", required=True, type=Path)
    parser.add_argument("--evidence", required=True, type=Path)
    args = parser.parse_args()
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
    for name, path_flag, hash_flag in (("node", "--node", "--node-sha256"),
                                       ("playwright", "--playwright-module", "--playwright-sha256"),
                                       ("browser", "--browser", "--browser-sha256")):
        row = bindings["inputs"][name]
        command += [path_flag, row["path"], hash_flag, row["sha256"]]
    raise SystemExit(subprocess.call(command, cwd=ROOT))


if __name__ == "__main__":
    main()
