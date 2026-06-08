#!/usr/bin/env bash
#
# Build repomap in release mode, install the binary onto your PATH, then reclaim
# disk by cleaning the build tree. The install step *copies* the binary (see
# src/install.rs), so it keeps working after `cargo clean`.

set -euo pipefail

cd "$(dirname "$0")"

echo "==> Building release binary"
cargo build --release

echo "==> Installing onto PATH"
./target/release/repomap --install

echo "==> Cleaning build tree"
cargo clean

echo "==> Done"
