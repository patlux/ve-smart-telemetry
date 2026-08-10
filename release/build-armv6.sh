#!/usr/bin/env bash
# Build deployable Raspberry Pi Zero W artifacts against the pinned Raspbian
# ARMv6 sysroot prepared by install-raspbian-sysroot.sh.

set -euo pipefail

sysroot="${RASPBIAN_SYSROOT:-/opt/raspbian-sysroot}"
target="arm-unknown-linux-gnueabihf"
gcc_dir="$sysroot/usr/lib/gcc/arm-linux-gnueabihf/10"
lib_dir="$sysroot/usr/lib/arm-linux-gnueabihf"

for required in \
  "$lib_dir/Scrt1.o" \
  "$gcc_dir/crtbeginS.o" \
  "$lib_dir/pkgconfig/dbus-1.pc"; do
  test -e "$required" || {
    echo "release build: missing Raspbian sysroot input: $required" >&2
    exit 2
  }
done

export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc
export CC_arm_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc
export CFLAGS_arm_unknown_linux_gnueabihf="--sysroot=$sysroot -isystem $sysroot/usr/include -isystem $sysroot/usr/include/arm-linux-gnueabihf"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$lib_dir/pkgconfig"

# The -B ordering is essential: generic Debian cross-GCC ships ARMv7
# crtbeginS/crtendS. Put the Raspbian ARMv6 GCC objects first, then the
# Raspbian libc startup objects and libraries.
export RUSTFLAGS="-C target-cpu=arm1176jzf-s -C link-arg=--sysroot=$sysroot -C link-arg=-B$gcc_dir -C link-arg=-B$lib_dir -C link-arg=-L$gcc_dir -C link-arg=-L$lib_dir"

cargo build --release --target "$target" --locked "$@"
