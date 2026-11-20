#!/bin/sh
set -eu

# Rust embeds dependency source locations in panic messages, even in stripped builds.
# Use Cargo's encoded flags so a home directory with spaces stays one argument.
sep=$(printf '\037')
if [ -n "${RUSTFLAGS:-}" ] && [ -z "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
  echo "Use CARGO_ENCODED_RUSTFLAGS for custom release flags." >&2
  exit 1
fi
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+${CARGO_ENCODED_RUSTFLAGS}${sep}}--remap-path-prefix=$HOME=/build/home${sep}--remap-path-prefix=$(pwd -P)=/build/session-take"
export CI=true
exec npm run tauri -- build --ci --bundles dmg -- --locked
