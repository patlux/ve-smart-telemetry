{
  description = "Victron BLE collector: Rust workspace for Raspberry Pi Zero W (ARMv6)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # ------------------------------------------------------------------
        # IMPORTANT — this dev shell is a COMPILE/CODEGEN VALIDATION
        # environment only, never a release path. The nixpkgs ARMv6 cross
        # sysroot links glibc 2.42 under a /nix/store loader; artifacts built
        # here do NOT run on stock Raspberry Pi OS. The authoritative release
        # toolchain is release/Dockerfile plus its pinned Raspberry Pi OS
        # Bullseye ARMv6 sysroot (glibc 2.31, /lib/ld-linux-armhf.so.3),
        # driven by `make release-armv6`.
        # ------------------------------------------------------------------

        # Pinned toolchain. Must match rust-toolchain.toml AND the
        # RUST_VERSION argument in release/Dockerfile.
        rustToolchain = pkgs.rust-bin.stable."1.97.1".default.override {
          targets = [ "arm-unknown-linux-gnueabihf" ];
        };

        # ARMv6 hard-float cross toolchain. nixpkgs exposes it as
        # `pkgsCross.raspberryPi`; GNU triple armv6l-unknown-linux-gnueabihf.
        armv6Pkgs = pkgs.pkgsCross.raspberryPi;
        armv6Cc = armv6Pkgs.stdenv.cc;
        # Absolute path to the actual cross GCC wrapper.
        armv6Gcc = "${armv6Cc}/bin/${armv6Cc.targetPrefix}gcc";

        # Cargo's linker lookup and the `cc` crate expect the conventional
        # name `arm-linux-gnueabihf-gcc` (Rust target triple name), while
        # nixpkgs names its wrapper with the full GNU triple prefix. Provide
        # a shim of the conventional name. (The release container ships the
        # real Debian tool of that exact name.)
        armv6GccShim = pkgs.writeShellScriptBin "arm-linux-gnueabihf-gcc" ''
          exec "${armv6Gcc}" "$@"
        '';

        # Cross binutils (strip/readelf/objdump for armv6). The host macOS
        # strip cannot process ARM ELF, so artifact verification must use the
        # target-prefixed tools.
        armv6Binutils = armv6Pkgs.binutils;

        # Target ARMv6 D-Bus development files.
        #
        # bluer's `bluetoothd` feature (pinned in the workspace root) pulls
        # in the `dbus` crate, whose libdbus-sys build script runs a
        # pkg-config probe for dbus-1 >= 1.6 against the *target*. The
        # nixpkgs cross package provides those files as store outputs; this
        # runCommand gathers every dbus-1.pc across all outputs into one
        # pkg-config directory so PKG_CONFIG_LIBDIR stays a single path.
        #
        # VERIFIED LIMITATION (2026-08-09, Darwin host): the armv6l cross
        # closure of dbus includes tcl, and nixpkgs' tcl cross build breaks
        # on Darwin hosts (mach/mach_time.h from the macOS SDK leaks into
        # the armv6l-linux compile). The full dbus build therefore fails
        # here, so dbusArmv6Pc is deliberately NOT in the default shell's
        # packages: entering the shell must not require it. On Linux hosts
        # the same closure is expected to build (one-time ~10-20 min); add
        # `dbusArmv6Pc` to packages to enable the Nix-side bluez check
        # there. On any host, the authoritative path for anything needing
        # libdbus is the release container (make check-linux).
        dbusArmv6Pc = pkgs.runCommand "dbus-armv6-pkgconfig" { } ''
          mkdir -p $out/lib/pkgconfig
          for d in ${armv6Pkgs.dbus}/lib/pkgconfig \
                   ${armv6Pkgs.dbus.dev}/lib/pkgconfig \
                   ${armv6Pkgs.dbus.lib}/lib/pkgconfig \
                   ${armv6Pkgs.dbus.out}/lib/pkgconfig; do
            if [ -d "$d" ]; then cp -L "$d"/*.pc "$out/lib/pkgconfig/" 2>/dev/null || true; fi
          done
          if [ -z "$(ls -A "$out/lib/pkgconfig")" ]; then
            echo "dbus-armv6-pkgconfig: no .pc files found in any dbus output" >&2
            exit 1
          fi
        '';
      in
      {
        devShells.default = pkgs.mkShell {
          name = "victron-workspace-armv6";

          packages = [
            rustToolchain
            armv6GccShim
            armv6Binutils
            pkgs.pkg-config
            pkgs.file
            pkgs.llvm # llvm-readelf fallback for artifact verification
          ];
          # NOTE: dbusArmv6Pc is intentionally NOT in packages on Darwin
          # (verified nixpkgs tcl cross bug blocks the dbus closure there).
          # Linux hosts enable the bluez check in-shell with the documented
          # PKG_CONFIG_LIBDIR export (see env block below).

          env = {
            # cargo: linker for the ARMv6 target (takes precedence over .cargo/config.toml)
            CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABIHF_LINKER = armv6Gcc;
            # cc crate (rusqlite bundled): C compiler for the ARMv6 target
            CC_arm_unknown_linux_gnueabihf = armv6Gcc;
            # Cross pkg-config preconditions. PKG_CONFIG_LIBDIR is NOT set
            # here because referencing dbusArmv6Pc would force the (broken
            # on Darwin) dbus closure to build on every `nix develop`; Linux
            # hosts enable the bluez check with:
            #   nix develop --command bash -c 'export
            #     PKG_CONFIG_LIBDIR=$(nix build --no-link --print-out-paths
            #     .#dbus-armv6-pc)/lib/pkgconfig; cargo check -p <bluez-crate>
            #     --target arm-unknown-linux-gnueabihf'
            PKG_CONFIG_ALLOW_CROSS = "1";
            PKG_CONFIG_SYSROOT_DIR = "/";
          };

          shellHook = ''
            echo "victron-workspace-armv6: CHECK-ONLY shell (cargo check / codegen validation)."
            echo "  Nix artifacts link glibc 2.42 with a /nix/store loader - NOT Pi-OS deployable."
            echo "  Deployable artifacts: make release-armv6 (pinned Raspbian ARMv6 sysroot, glibc 2.31)."
          '';
        };

        # Buildable on Linux hosts; on Darwin the nixpkgs armv6l tcl cross
        # bug blocks the dbus closure (see comment above).
        packages = { dbus-armv6-pc = dbusArmv6Pc; };

        formatter = pkgs.nixpkgs-fmt;
      });
}
