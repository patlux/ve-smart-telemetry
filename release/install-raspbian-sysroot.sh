#!/usr/bin/env bash
# Install the pinned Raspberry Pi OS Bullseye ARMv6 hard-float sysroot.
#
# Debian's generic armhf development files target ARMv7 and can produce an
# ELF that passes superficial instruction scans but crashes on a Pi Zero W
# before main. These packages come from the Raspbian Bullseye archive, whose
# crt/libgcc objects are built for ARMv6 + VFPv2.

set -euo pipefail

base_url="https://archive.raspbian.org/raspbian"
sysroot="${1:-/opt/raspbian-sysroot}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# SHA-256 values are from Bullseye's signed Packages index, observed
# 2026-08-09. Paths and hashes pin the exact sysroot inputs.
cat >"$work/packages" <<'EOF'
5a930712fe69d8a8a3bd9d91a6e988cf19f4c21612741b9d57eb75ee813bb18d pool/main/g/glibc/libc6_2.31-13+rpi1+deb11u14_armhf.deb
2e5058ca0fe147fd0fe2acc1278b899b75aa8ecb8f87bce957347c54513ae3a7 pool/main/g/glibc/libc6-dev_2.31-13+rpi1+deb11u14_armhf.deb
835e43c46a74d42ec5ede202fbdc2ee2fb30e0fa32d520ee9d2de297010bcf21 pool/main/g/gcc-10/libgcc-s1_10.2.1-6+rpi1_armhf.deb
c6adb9b390dfd4aaf8447d592dc151c5b96ad626ea80733795a9cd36863891ab pool/main/g/gcc-10/libgcc-10-dev_10.2.1-6+rpi1_armhf.deb
a272146fd89a0a7d7d4a4de8ea5973bf01f67b6e1fe18ca6eca896745dcf5642 pool/main/d/dbus/libdbus-1-3_1.12.28-0+deb11u1_armhf.deb
41f1d651e53a332ba308c02681785791a4fdb8956d9251eb19934da0ae989550 pool/main/d/dbus/libdbus-1-dev_1.12.28-0+deb11u1_armhf.deb
8da43c2adaace329d39e31eed2b13150ca672bd04f4c5a9e4cf646203f8768ae pool/main/s/systemd/libsystemd0_247.3-7+rpi1+deb11u8_armhf.deb
fff27f3ec0df7c7cf26e99ba5e6111bb4ddd292509b01bee331b1ac36aaa2675 pool/main/s/systemd/libsystemd-dev_247.3-7+rpi1+deb11u8_armhf.deb
12c01cef7f9deb53999ac0f3485f5412749a1d7b643fa8e90ff13cd8cbb12c4b pool/main/libc/libcap2/libcap2_2.44-1+deb11u1_armhf.deb
ed33e8a72774e02c98e4d92a2909b8e0f91b6aad144a8fd478bce5da30559281 pool/main/libc/libcap2/libcap-dev_2.44-1+deb11u1_armhf.deb
852ef2e5361aefafea3a995b401eedbe9dba6101d1bb1ec020868a9d3c987bd6 pool/main/x/xz-utils/liblzma5_5.2.5-2.1~deb11u2_armhf.deb
a687d7a7b9d28cfb17e62533d9338f8a5a206c14478c0a4568ca72ba48ac9aa7 pool/main/libz/libzstd/libzstd1_1.4.8+dfsg-2.1+rpi1_armhf.deb
655d822497d09015aa63a1f2436c0f745884c27cb00b8dbf57cae74e3ed881da pool/main/l/lz4/liblz4-1_1.9.3-2_armhf.deb
7ff4136baf259c59e1d81f55c35f1282784a9c13ea7f09589786079c3fb6d08d pool/main/libg/libgcrypt20/libgcrypt20_1.8.7-6_armhf.deb
44e1c8534c1ae58340194a926dd16c32d973aee38ebd8f19d673ce01595b9611 pool/main/libg/libgpg-error/libgpg-error0_1.38-2_armhf.deb
EOF

rm -rf "$sysroot"
mkdir -p "$sysroot"
while read -r expected path; do
  file="$work/${path##*/}"
  curl --fail --location --retry 5 --retry-delay 2 \
    "$base_url/$path" -o "$file"
  printf '%s  %s\n' "$expected" "$file" | sha256sum --check --status
  dpkg-deb --extract "$file" "$sysroot"
done <"$work/packages"

# Debian usrmerge linker scripts refer to /lib paths inside the sysroot.
mkdir -p "$sysroot/lib"
ln -sfn ../usr/lib/arm-linux-gnueabihf "$sysroot/lib/arm-linux-gnueabihf"
ln -sfn arm-linux-gnueabihf/ld-linux-armhf.so.3 \
  "$sysroot/lib/ld-linux-armhf.so.3"

# Debian package symlinks such as libpthread.so use absolute `/lib/...`
# targets. The kernel resolves those against the build container, not the
# linker's sysroot, causing GCC to fall back to incompatible static archives.
# Rewrite every absolute symlink so it remains inside the extracted sysroot.
while IFS= read -r -d '' link; do
  target="$(readlink "$link")"
  case "$target" in
    /*)
      # Resolve the existing target under the sysroot first, then make that
      # canonical file relative to the link. `realpath -m` is intentionally
      # avoided: it mishandles the sysroot's `/lib` usrmerge link here.
      relative="$(realpath --relative-to="$(dirname "$link")" "$sysroot$target")"
      ln -sfn "$relative" "$link"
      ;;
  esac
done < <(find "$sysroot" -type l -print0)

crt="$sysroot/usr/lib/arm-linux-gnueabihf/Scrt1.o"
attrs="$(arm-linux-gnueabihf-readelf -A "$crt")"
printf '%s\n' "$attrs" | grep -q 'Tag_CPU_arch: v6'
printf '%s\n' "$attrs" | grep -q 'Tag_THUMB_ISA_use: Thumb-1'
printf '%s\n' "$attrs" | grep -q 'Tag_FP_arch: VFPv2'
printf '%s\n' "$attrs" | grep -q 'Tag_ABI_VFP_args: VFP registers'
