#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

expected=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)
if [[ $(rustc --version) != "rustc $expected "* ]]; then
  echo "Expected Rust $expected" >&2
  exit 1
fi

if [[ ${CI:-} == true && -n ${CARGO_TARGET_DIR:-} && -d $CARGO_TARGET_DIR ]]; then
  if (( $(du -sm "$CARGO_TARGET_DIR" | cut -f1) > 6144 )); then
    cargo clean
  fi
fi

set -x
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps
