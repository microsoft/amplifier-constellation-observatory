#!/usr/bin/env bash
set -euo pipefail
repository_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "$(uname -s)" != Linux ]]; then
  echo "This Observatory installer currently supports Linux hosts. Its web interface works across platforms."
  exit 1
fi
if ! command -v cargo >/dev/null; then
  echo "Install Rust with rustup, then run this installer again."
  exit 1
fi
if command -v dpkg-query >/dev/null; then
  for package in libpam0g-dev libclang-dev build-essential pkg-config; do
    if ! dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q 'install ok installed'; then
      sudo apt-get update
      sudo apt-get install -y libpam0g-dev libclang-dev build-essential pkg-config
      break
    fi
  done
fi
cargo build --release --locked --manifest-path "$repository_dir/Cargo.toml"
exec python3 "$repository_dir/scripts/install.py" --binary "$repository_dir/target/release/agent-observatory" "$@"
