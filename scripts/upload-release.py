#!/usr/bin/env python3
"""Stage new assets in a draft; never overwrite an existing release asset."""
import argparse
import json
import subprocess
import tempfile
from pathlib import Path


def stage(tag, directory, run=subprocess.run):
    files = sorted(directory.iterdir())
    if not files or any(not p.is_file() for p in files):
        raise ValueError("Expected a nonempty directory of release files")

    def gh(*args, check=True):
        return run(["gh", "release", *args], check=check, capture_output=True, text=True)

    result = gh("view", tag, "--json", "isDraft,assets", check=False)
    if result.returncode:
        if tag.startswith("machine-helper-v"):
            gh("create", tag, "--verify-tag", "--draft", "--latest=false", "--title",
               f"Constellation Computer {tag.removeprefix('machine-helper-v')}", "--notes",
               "Six native computer-helper packages and their exact combined setup inventory. "
               "These helpers pair with an Observatory; this is not an Observatory application release. "
               "Preview packages are unsigned and unnotarized. Publication follows exact platform and setup qualification.")
        else:
            gh("create", tag, "--verify-tag", "--draft", "--title",
               f"Agent Observatory {tag}", "--notes",
               "Independent fleet surface with optional negotiated management. "
               "Native Windows, macOS and Linux applications and computer helpers. Preview packages are unsigned; see exact platform qualification. "
               "See docs/releases in the source for behavior and platform limitations.")
        release = {"isDraft": True, "assets": []}
    else:
        release = json.loads(result.stdout)
    existing = {asset["name"] for asset in release["assets"]}
    expected = {path.name for path in files}

    # A published release may contain independently reviewed packaging. A rebuild
    # is not permission to replace it, even when its source tree is equivalent.
    if not release["isDraft"]:
        if expected - existing:
            raise ValueError("Published release is missing expected assets; repair manually")
        return "Published release already contains all expected assets; left unchanged"

    # Validate every existing draft file before uploading any missing files.
    # This permits partial-upload retries but refuses a different build with the
    # same filename. In particular, never use gh release upload --clobber.
    with tempfile.TemporaryDirectory() as temp:
        for path in files:
            if path.name in existing:
                gh("download", tag, "--pattern", path.name, "--dir", temp)
                if (Path(temp) / path.name).read_bytes() != path.read_bytes():
                    raise ValueError(f"Draft asset differs: {path.name}; left unchanged")
        for path in files:
            if path.name not in existing:
                gh("upload", tag, str(path))
    return "Draft release assets staged; publication still requires review"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--directory", type=Path, default=Path("dist"))
    args = parser.parse_args()
    print(stage(args.tag, args.directory))
