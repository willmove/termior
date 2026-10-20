#!/usr/bin/env bash
# Host entry point for the Linux release job.
#
# Builds packaging/linux/Dockerfile (Ubuntu 20.04, glibc 2.31) and runs
# packaging/linux/build-release.sh inside it. The container writes
# target/release/termior and dist/termior-<version>-linux-x86_64.deb.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
cd "$root"

if [ -z "${TERMIOR_VERSION:-}" ]; then
  echo "TERMIOR_VERSION is required (workspace version, without the v prefix)" >&2
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "error: docker is required to build Linux artifacts on the glibc 2.31 baseline" >&2
  exit 1
fi

channel=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
if [ -z "$channel" ]; then
  echo "error: channel not found in rust-toolchain.toml" >&2
  exit 1
fi

image="termior-linux-focal:${channel}"
cache=${TERMIOR_CARGO_CACHE:-"$root/.cache/termior-focal"}
mkdir -p "$cache/registry" "$cache/git"

docker build \
  --build-arg "RUST_CHANNEL=${channel}" \
  -f packaging/linux/Dockerfile \
  -t "$image" \
  packaging/linux

run_env=(
  -e "TERMIOR_VERSION=${TERMIOR_VERSION}"
  -e "HOST_UID=$(id -u)"
  -e "HOST_GID=$(id -g)"
)
if [ -n "${RUSTFLAGS:-}" ]; then
  run_env+=(-e "RUSTFLAGS=${RUSTFLAGS}")
fi
if [ -n "${CARGO_TERM_COLOR:-}" ]; then
  run_env+=(-e "CARGO_TERM_COLOR=${CARGO_TERM_COLOR}")
fi
if [ -n "${CARGO_BUILD_JOBS:-}" ]; then
  run_env+=(-e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}")
fi

docker run --rm \
  -v "$root:/src" \
  -w /src \
  -v "$cache/registry:/usr/local/cargo/registry" \
  -v "$cache/git:/usr/local/cargo/git" \
  "${run_env[@]}" \
  "$image" \
  bash packaging/linux/build-release.sh
