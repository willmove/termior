#!/usr/bin/env bash
# Fail when a Linux binary or .deb needs glibc newer than the release baseline.
#
# usage:
#   packaging/linux/check-glibc-baseline.sh --self-test
#   packaging/linux/check-glibc-baseline.sh <binary> [package.deb]
#
# The binary check reads both dynamic symbols (objdump -T) and the ELF version
# needs (readelf -V). Weak symbols such as pidfd_spawnp@GLIBC_2.39 still show
# up in the version-needs section and the dynamic loader rejects them on older
# glibc, so either source is enough to fail the gate.
#
# GLIBC_BASELINE overrides the ceiling (default 2.31, Ubuntu 20.04 / Kylin V10).
set -euo pipefail

baseline=${GLIBC_BASELINE:-2.31}

version_gt() {
  local a=$1 b=$2 smaller
  [ "$a" = "$b" ] && return 1
  smaller=$(printf '%s\n%s\n' "$a" "$b" | sort -V | head -n1)
  [ "$smaller" = "$b" ]
}

upstream_version() {
  local v=$1
  v=${v##*:}
  v=${v%%-*}
  v=${v%%+*}
  printf '%s' "$v"
}

self_test() {
  version_gt 2.32 2.31
  version_gt 2.31 2.3
  version_gt 2.35 2.31
  version_gt 2.39 2.31
  version_gt 2.3.4 2.3
  if version_gt 2.31 2.31; then
    echo "2.31 must not be greater than 2.31" >&2
    exit 1
  fi
  if version_gt 2.3 2.31; then
    echo "2.3 must not be greater than 2.31" >&2
    exit 1
  fi
  if version_gt 2.17 2.31; then
    echo "2.17 must not be greater than 2.31" >&2
    exit 1
  fi
  [ "$(upstream_version '2.31-0ubuntu9.16')" = "2.31" ]
  [ "$(upstream_version '1:2.39-0ubuntu8.7')" = "2.39" ]
  [ "$(upstream_version '2.31')" = "2.31" ]
  echo "glibc baseline self-test ok (ceiling $baseline)"
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit 0
fi

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 <binary> [package.deb]" >&2
  exit 2
fi

binary=$1
deb=${2:-}

for tool in objdump readelf; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required to read GLIBC_ symbol versions" >&2
    exit 1
  fi
done

if [ ! -f "$binary" ]; then
  echo "error: binary not found: $binary" >&2
  exit 1
fi

versions=$(
  {
    objdump -T "$binary" || true
    readelf -V "$binary" || true
  } | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sed 's/^GLIBC_//' | sort -Vu || true
)

if [ -z "$versions" ]; then
  echo "error: no GLIBC_ symbol versions found in $binary" >&2
  exit 1
fi

highest=$(printf '%s\n' "$versions" | sort -V | tail -n1)
echo "GLIBC_ symbol versions in $(basename "$binary"):"
printf '%s\n' "$versions" | sed 's/^/  /'
echo "highest GLIBC symbol version is $highest (baseline $baseline)"

status=0
while IFS= read -r version; do
  [ -z "$version" ] && continue
  if version_gt "$version" "$baseline"; then
    echo "error: $binary requires GLIBC_$version, newer than $baseline" >&2
    status=1
  fi
done <<EOF
$versions
EOF

if [ -n "$deb" ]; then
  if ! command -v dpkg-deb >/dev/null 2>&1; then
    echo "error: dpkg-deb is required to read $deb" >&2
    exit 1
  fi
  if [ ! -f "$deb" ]; then
    echo "error: deb not found: $deb" >&2
    exit 1
  fi
  depends=$(dpkg-deb -f "$deb" Depends)
  echo "Depends: $depends"
  found_libc=0
  IFS=',' read -ra parts <<EOF
$depends
EOF
  for part in "${parts[@]}"; do
    part=${part#"${part%%[![:space:]]*}"}
    part=${part%"${part##*[![:space:]]}"}
    case "$part" in
      libc6*)
        found_libc=1
        if [[ "$part" =~ \([[:space:]]*[\>]?=[[:space:]]*([^[:space:]]+)[[:space:]]*\) ]]; then
          required=${BASH_REMATCH[1]}
          upstream=$(upstream_version "$required")
          if version_gt "$upstream" "$baseline"; then
            echo "error: $deb depends on libc6 (>= $required), newer than glibc $baseline" >&2
            status=1
          else
            echo "libc6 constraint $required is within glibc $baseline"
          fi
        else
          echo "libc6 dependency has no minimum version ($part)"
        fi
        ;;
    esac
  done
  if [ "$found_libc" -ne 1 ]; then
    echo "error: $deb does not depend on libc6" >&2
    status=1
  fi
fi

if [ "$status" -ne 0 ]; then
  exit 1
fi

echo "glibc baseline ok: $binary stays within GLIBC_$baseline"
