# syntax=docker/dockerfile:1
#
# Cross-build environment for the Raspberry Pi Zero W (ARMv6, hard-float).
# This is the AUTHORITATIVE release toolchain for this workspace: every
# artifact produced here links against glibc 2.31 (Debian bullseye = the
# Raspberry Pi OS bullseye baseline) with the standard interpreter
# /lib/ld-linux-armhf.so.3, so the binary runs on stock Raspberry Pi OS
# without modification.
#
# The Nix dev shell (flake.nix) is NOT a release path: it links a
# /nix/store glibc (2.42) and must only be used for `cargo check` /
# codegen validation. See ARMv6-BUILD.md for the split.
#
# Base image pinned by digest (verified 2026-08-09):
#   debian:bullseye-slim  ->  sha256:f313b4bd62667092a59b3a664d7d3ab8b5e65f41675f48e81455a15dc5abe792
#   Debian 11.11, glibc 2.31-13+deb11u14  (ldd output verified).
#   Residual supply-chain limitation: the digest pins this exact image, but
#   the layer CONTENT underneath can still be replaced on the registry as
#   long as the content-addressed digest is published for another payload;
#   apt packages and the rust toolchain are fetched at build time and are
#   NOT byte-reproducible across dates (bullseye is in LTS, package pool
#   keeps moving). For full reproducibility, mirror the apt pool + rust
#   dist tarballs and pin those hashes; out of scope for Phase 0.
FROM debian:bullseye-slim@sha256:f313b4bd62667092a59b3a664d7d3ab8b5e65f41675f48e81455a15dc5abe792

ENV DEBIAN_FRONTEND=noninteractive

# Host-side build/cross tools plus native D-Bus headers for Linux host tests.
# Target libc/D-Bus/GCC objects come exclusively from the pinned Raspbian
# ARMv6 sysroot below; generic Debian armhf startup objects are ARMv7 and crash
# on an original Pi Zero W before main.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        file \
        xz-utils \
        pkg-config \
        build-essential \
        libdbus-1-dev \
        crossbuild-essential-armhf \
        binutils-arm-linux-gnueabihf \
    && rm -rf /var/lib/apt/lists/*

COPY install-raspbian-sysroot.sh /usr/local/bin/install-raspbian-sysroot
RUN chmod 0755 /usr/local/bin/install-raspbian-sysroot \
    && /usr/local/bin/install-raspbian-sysroot /opt/raspbian-sysroot

# Rust pinned toolchain. rust-toolchain.toml at the workspace root pins
# 1.97.1 as well; pre-installing it here (with checksums verified by
# rustup) keeps the container usable offline once the image exists and
# prevents a surprise re-download on first use.
ARG RUST_VERSION=1.97.1
ARG RUST_TARGET=arm-unknown-linux-gnueabihf
RUN curl -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y \
        --profile minimal \
        --default-toolchain "${RUST_VERSION}" \
        --target "${RUST_TARGET}" \
    && rm /tmp/rustup-init.sh

ENV PATH="/root/.cargo/bin:${PATH}"

# The build helper wires the pinned Raspbian sysroot, including ARMv6
# crtbeginS/crtendS ordering. Do not invoke raw `cargo build --target ...` for
# release artifacts.
COPY build-armv6.sh /usr/local/bin/build-armv6
RUN chmod 0755 /usr/local/bin/build-armv6
ENV RASPBIAN_SYSROOT=/opt/raspbian-sysroot

# Default working directory for `make release-armv6` (workspace is
# volume-mounted here by the Makefile).
WORKDIR /workspace
