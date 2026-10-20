#!/usr/bin/env bash
# Build the Linux release binary and .deb inside the Ubuntu 20.04 image.
#
# packaging/linux/ci-build.sh is the host entry point. It mounts this repo at
# /src and passes TERMIOR_VERSION. cargo-deb resolves Depends with
# dpkg-shlibdeps on this system's symbols files, so the .deb floor matches the
# binary instead of the GitHub runner's glibc.
set -euo pipefail

cd /src

bash packaging/linux/check-glibc-baseline.sh --self-test

if [ -z "${TERMIOR_VERSION:-}" ]; then
  echo "TERMIOR_VERSION is required (workspace version, without the v prefix)" >&2
  exit 1
fi

builder_glibc=$(ldd --version | awk 'NR==1 { print $NF }')
echo "builder glibc: $builder_glibc"
if [ "$builder_glibc" != "2.31" ]; then
  echo "error: Linux release artifacts must be linked on glibc 2.31, found $builder_glibc" >&2
  exit 1
fi

git config --global --add safe.directory /src || true

export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
if [ -z "${RUSTFLAGS:-}" ]; then
  export RUSTFLAGS="-D warnings"
fi

cargo build -p termior --release --locked

arch=$(rustc -vV | sed -n 's/^host: \([^-]*\)-.*/\1/p')
if [ "$arch" != "x86_64" ]; then
  echo "error: Linux release packaging supports x86_64 only (host arch: ${arch:-unknown})" >&2
  exit 1
fi

mkdir -p dist
deb="dist/termior-${TERMIOR_VERSION}-linux-${arch}.deb"
rm -f "$deb" "$deb.sha256"
cargo deb -p termior --no-build --no-strip --output "$deb"
hash=$(sha256sum "$deb" | awk '{ print $1 }')
printf '%s  %s\n' "$hash" "$(basename "$deb")" >"$deb.sha256"

bash packaging/linux/check-glibc-baseline.sh target/release/termior "$deb"

if [ -n "${HOST_UID:-}" ] && [ -n "${HOST_GID:-}" ]; then
  chown -R "${HOST_UID}:${HOST_GID}" target dist
  if [ -d /usr/local/cargo/registry ]; then
    chown -R "${HOST_UID}:${HOST_GID}" /usr/local/cargo/registry || true
  fi
  if [ -d /usr/local/cargo/git ]; then
    chown -R "${HOST_UID}:${HOST_GID}" /usr/local/cargo/git || true
  fi
fi
