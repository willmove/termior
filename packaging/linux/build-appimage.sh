#!/usr/bin/env bash
# Assemble the published AppImage from the glibc 2.31 release binary.
#
# usage: packaging/linux/build-appimage.sh <binary> <arch>
#
# TERMIOR_VERSION is required (workspace version, without the v prefix); the
# output is dist/termior-<version>-linux-<arch>.AppImage plus its .sha256.
#
# Runs inside packaging/linux/Dockerfile (Ubuntu 20.04, glibc 2.31) so the
# bundled shared libraries keep the same glibc floor as the .deb and the
# tarball. The binary's ldd closure is copied into usr/lib except glibc itself
# and the graphics driver stack (libGL/libEGL/Vulkan and friends), which must
# always come from the host. Libraries the binary dlopens at runtime (Wayland,
# EGL) are not bundled: any desktop that can use them already ships them.
#
# appimagetool and the type-2 runtime are pinned release downloads verified by
# SHA-256. The runtime pin replaces appimagetool's default of fetching whatever
# runtime release is newest. Refresh both pins together when upgrading.
set -euo pipefail

appimagetool_url="https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage"
appimagetool_sha256="ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
runtime_url="https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64"
runtime_sha256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"
# Same ceiling as the other release archives (scripts/package-release.ps1).
max_appimage_mib=100

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <binary> <arch>" >&2
  exit 2
fi
binary=$1
arch=$2

if [ -z "${TERMIOR_VERSION:-}" ]; then
  echo "TERMIOR_VERSION is required (workspace version, without the v prefix)" >&2
  exit 1
fi
if [ ! -f "$binary" ]; then
  echo "error: binary not found: $binary" >&2
  exit 1
fi

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
for tool in curl ldd sha256sum mksquashfs desktop-file-validate; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required to build the AppImage (see packaging/linux/Dockerfile)" >&2
    exit 1
  fi
done

mkdir -p dist
output="dist/termior-${TERMIOR_VERSION}-linux-${arch}.AppImage"
rm -f "$output" "$output.sha256"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
appdir="$work/AppDir"

install -D -m 0755 "$binary" "$appdir/usr/bin/termior"
install -D -m 0644 "$root/packaging/linux/app.termior.Termior.desktop" \
  "$appdir/app.termior.Termior.desktop"
install -D -m 0644 "$root/packaging/linux/app.termior.Termior.desktop" \
  "$appdir/usr/share/applications/app.termior.Termior.desktop"
install -d "$appdir/usr/share/icons"
cp -r "$root/assets/icons/hicolor" "$appdir/usr/share/icons/"
# appimagetool resolves Icon= only against the AppDir root, so the 256x256 PNG
# must live there; the full hicolor tree above serves desktop integration.
install -D -m 0644 "$root/assets/icons/hicolor/256x256/apps/app.termior.Termior.png" \
  "$appdir/app.termior.Termior.png"

cat >"$appdir/AppRun" <<'EOF'
#!/bin/sh
# Keep the bundled shared libraries ahead of whatever the host provides.
HERE=$(dirname "$(readlink -f "$0")")
export LD_LIBRARY_PATH="${HERE}/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "${HERE}/usr/bin/termior" "$@"
EOF
chmod 0755 "$appdir/AppRun"

# Copy the binary's shared-library closure into usr/lib. glibc and the graphics
# driver stack are deliberately excluded: glibc must come from the host, and
# driver libraries only work against the host's GL/Vulkan stack.
deps=$(ldd "$binary")
case "$deps" in
  *"not found"*)
    echo "error: unresolved shared libraries:" >&2
    printf '%s\n' "$deps" | grep 'not found' >&2
    exit 1
    ;;
esac
bundled=0
while IFS=$'\t' read -r name path; do
  [ -n "$name" ] || continue
  case "$name" in
    ld-linux* | libc.so* | libm.so* | libpthread.so* | libdl.so* | librt.so* | \
      libresolv.so* | libutil.so* | libnsl.so* | libnss_*)
      continue
      ;;
    libGL* | libEGL* | libOpenGL* | libGLX* | libGLdispatch* | libglapi* | \
      libGLES* | libvulkan* | libcuda*)
      continue
      ;;
  esac
  install -D -m 0644 "$path" "$appdir/usr/lib/$name"
  bundled=$((bundled + 1))
  echo "bundled $name"
done <<EOF
$(printf '%s\n' "$deps" | awk '
  /=>/ { print $1 "\t" $3; next }
  /^[[:space:]]*\// { name = $1; sub(/.*\//, "", name); print name "\t" $1 }
')
EOF
echo "bundled $bundled shared libraries"

fetch() {
  # url sha256 dest — GitHub downloads flake occasionally; retry before failing.
  local attempt
  for attempt in 1 2 3 4 5; do
    if curl -fsSL --retry 3 --retry-delay 2 --retry-connrefused --connect-timeout 30 \
      -o "$3" "$1"; then
      break
    fi
    if [ "$attempt" -eq 5 ]; then
      echo "error: failed to download $1" >&2
      exit 1
    fi
    sleep 2
  done
  if ! printf '%s  %s\n' "$2" "$3" | sha256sum -c - >/dev/null; then
    echo "error: SHA-256 mismatch for $1" >&2
    exit 1
  fi
}

fetch "$appimagetool_url" "$appimagetool_sha256" "$work/appimagetool.AppImage"
fetch "$runtime_url" "$runtime_sha256" "$work/runtime-x86_64"
# Extract instead of running the tool's own AppImage: the builder container has
# no FUSE, and extraction works with every type-2 runtime.
chmod +x "$work/appimagetool.AppImage"
( cd "$work" && ./appimagetool.AppImage --appimage-extract >/dev/null )

ARCH="$arch" VERSION="$TERMIOR_VERSION" "$work/squashfs-root/AppRun" \
  --no-appstream --runtime-file "$work/runtime-x86_64" \
  "$appdir" "$output"

size_mib=$(( $(stat -c %s "$output") / 1048576 ))
if [ "$size_mib" -gt "$max_appimage_mib" ]; then
  echo "error: $output is ${size_mib} MiB, above the ${max_appimage_mib} MiB release limit" >&2
  exit 1
fi
hash=$(sha256sum "$output" | awk '{ print $1 }')
printf '%s  %s\n' "$hash" "$(basename "$output")" >"$output.sha256"
echo "AppImage: $output (${size_mib} MiB)"
echo "$hash"
