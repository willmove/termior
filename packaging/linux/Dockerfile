# Ubuntu 20.04 / glibc 2.31 builder for published Linux artifacts.
#
# GitHub-hosted ubuntu-latest is Ubuntu 24.04 (glibc 2.39) and is moving to
# 26.04. Linking the release binary there records GLIBC_2.34+ symbol versions,
# plus a weak pidfd_spawnp/pidfd_getpid reference that still adds a GLIBC_2.39
# VERNEED. cargo-deb's default $auto depends (dpkg-shlibdeps) then emits
# libc6 (>= 2.39), which apt refuses on Kylin V10 / Ubuntu 20.04.
#
# Build and package inside this image so the linker and dpkg-shlibdeps both
# see glibc 2.31. Newer distros still run the binary: glibc and the other
# SONAMEs are backward compatible.
FROM ubuntu:20.04

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    LIBCLANG_PATH=/usr/lib/llvm-10/lib

# Focal standard support has ended. Ignore an expired Release file so the
# frozen archive can still install the linker sysroot. This image is a build
# baseline, not a runtime.
RUN printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99no-check-valid-until \
    && sed -i 's/^\(deb\(-\(src\|security\)\)\? [^ ]* [^ ]*\) main restricted$/\1 main restricted universe/' /etc/apt/sources.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        desktop-file-utils \
        dpkg-dev \
        fakeroot \
        file \
        git \
        libasound2-dev \
        libclang-dev \
        libdbus-1-dev \
        libegl1-mesa-dev \
        libfontconfig-dev \
        libfreetype-dev \
        liblzma-dev \
        libudev-dev \
        libwayland-dev \
        libx11-dev \
        libx11-xcb-dev \
        libxcb1-dev \
        libxkbcommon-dev \
        libxkbcommon-x11-dev \
        pkg-config \
        squashfs-tools \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

ARG RUST_CHANNEL=1.98.1
ARG CARGO_DEB_VERSION=3.8.0
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain "${RUST_CHANNEL}" --profile minimal --component rustfmt,clippy \
    && cargo install cargo-deb --version "${CARGO_DEB_VERSION}" --locked \
    && rm -rf /usr/local/cargo/registry /usr/local/cargo/git \
    && ldd --version \
    && cargo deb --version
