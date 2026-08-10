# Build orchestration for the Victron BLE collector workspace.
#
# TWO ARMv6 build paths, deliberately separate (see ARMv6-BUILD.md):
#
#   check-armv6    Nix dev shell (flake.nix): `cargo check` / codegen
#                  validation ONLY. Nix artifacts link the nixpkgs ARMv6
#                  glibc 2.42 under a /nix/store loader and are NOT
#                  deployable to stock Raspberry Pi OS.
#
#   release-armv6  release/Dockerfile plus the pinned Raspberry Pi OS
#                  Bullseye ARMv6 sysroot (glibc 2.31, standard loader
#                  /lib/ld-linux-armhf.so.3): the ONLY path that produces a
#                  Pi-Zero-compatible artifact. Generic Debian armhf CRT
#                  objects are ARMv7/Thumb-2 and must never be used. Requires
#                  a docker daemon (OrbStack/Colima/Docker Desktop on macOS).
#
# `make build-armv6` intentionally refuses to run, so nobody silently
# produces an undeployable binary.

SHELL := bash
TARGET := arm-unknown-linux-gnueabihf
RELEASE_IMAGE ?= victron-armv6-release:phase0

# Absolute source dir (resolved once so docker mounts and nix both work).
WS := $(shell pwd)

.PHONY: help shell release-shell check test check-linux test-linux \
        check-armv6 release-armv6 build-armv6 verify-armv6 verify-armv6-check \
        strip-armv6 fmt clippy lock-check

help: ## Show this help
	@echo "victron workspace targets:"
	@echo "  shell             enter the Nix dev shell (check-only toolchain)"
	@echo "  release-shell     interactive shell inside the release container"
	@echo "  check             host cargo check --workspace (bluer is Linux-only; fails on macOS once integrated)"
	@echo "  test              host cargo test --workspace (same Linux-only caveat)"
	@echo "  check-linux       cargo check --workspace inside the release container (Linux)"
	@echo "  test-linux        cargo test --workspace inside the release container (Linux)"
	@echo "  check-armv6       Nix: cargo check --target $(TARGET)  [validation only, NOT deployable]"
	@echo "  release-armv6     container: pinned Raspbian ARMv6 release build (standard loader, glibc 2.31)"
	@echo "  build-armv6       REFUSES (use check-armv6 or release-armv6)"
	@echo "  verify-armv6      hard-verify a release artifact: make verify-armv6 BIN=target/$(TARGET)/release/<bin>"
	@echo "  verify-armv6-check hard-verify a Nix check artifact: BIN=<path> [check-only checks]"
	@echo "  strip-armv6       strip a release artifact with the cross strip: BIN=<path>"
	@echo "  fmt / clippy      host formatting / lint"
	@echo "  lock-check        verify flake.lock is in sync with flake.nix"

shell: ## Enter the Nix dev shell (pinned Rust + ARMv6 cross GCC; check-only)
	nix develop

release-shell: ## Interactive shell inside the release container (workspace mounted at /workspace)
	docker run --rm -it -v $(WS):/workspace -w /workspace $(RELEASE_IMAGE) bash

# ---------------------------------------------------------------------------
# Host checks. NOTE: the bluez lane (bluer) is Linux-only; once it is a
# workspace member, `cargo test --workspace` / `cargo check --workspace`
# FAIL on macOS. Use check-linux / test-linux for full-workspace runs.
# ---------------------------------------------------------------------------

check: ## Host workspace check
	cargo check --workspace

test: ## Host workspace tests (dev profile; release profile has panic=abort)
	cargo test --workspace

check-linux: ## Workspace check inside the release container (Linux; bluer works)
	docker run --rm -v $(WS):/workspace -w /workspace $(RELEASE_IMAGE) cargo check --workspace

test-linux: ## Workspace tests inside the release container (Linux host tests)
	docker run --rm -v $(WS):/workspace -w /workspace $(RELEASE_IMAGE) \
		bash -lc 'unset PKG_CONFIG_SYSROOT_DIR PKG_CONFIG_LIBDIR PKG_CONFIG_ALLOW_CROSS; cargo test --workspace'

# ---------------------------------------------------------------------------
# ARMv6 cross builds
# ---------------------------------------------------------------------------

check-armv6: ## Nix: compile/codegen validation for $(TARGET) only — NOT deployable
	nix develop --command cargo check --target $(TARGET)

release-armv6: ## Container: pinned Raspbian ARMv6 artifact (glibc 2.31, /lib/ld-linux-armhf.so.3)
	docker build -t $(RELEASE_IMAGE) -f release/Dockerfile release/
	docker run --rm -v $(WS):/workspace -w /workspace $(RELEASE_IMAGE) \
		build-armv6
	@echo "Built under target/$(TARGET)/release/. Run: make verify-armv6 BIN=target/$(TARGET)/release/<binary>"

build-armv6: ## REFUSES: split into check-armv6 (Nix, validation) and release-armv6 (deployable)
	@echo "error: 'make build-armv6' was removed deliberately." >&2
	@echo "  - Nix artifacts link glibc 2.42 with a /nix/store loader and do NOT run on stock Pi OS." >&2
	@echo "  Use 'make check-armv6' (Nix compile/codegen validation) or" >&2
	@echo "  'make release-armv6' (release/Dockerfile, Pi-OS-compatible artifact)." >&2
	@exit 2

# ---------------------------------------------------------------------------
# Artifact verification / stripping
# ---------------------------------------------------------------------------

verify-armv6: ## Hard-verify a RELEASE artifact (fails on missing/wrong attributes)
	@test -n "$(BIN)" || { echo "usage: make verify-armv6 BIN=target/$(TARGET)/release/<binary>"; exit 2; }
	docker run --rm -v $(WS):/workspace -w /workspace $(RELEASE_IMAGE) \
		bash release/verify-armv6.sh /workspace/$(BIN) --mode release

verify-armv6-check: ## Verify a NIX-built (cargo build) artifact — object/instruction checks only
	@test -n "$(BIN)" || { echo "usage: make verify-armv6-check BIN=target/$(TARGET)/debug/<binary>  (from a Nix 'cargo build', not 'cargo check')"; exit 2; }
	nix develop --command bash release/verify-armv6.sh $(BIN) --mode check

strip-armv6: ## Strip an ARMv6 artifact with the cross strip (host strip can't)
	@test -n "$(BIN)" || { echo "usage: make strip-armv6 BIN=target/$(TARGET)/release/<binary>"; exit 2; }
	docker run --rm -v $(WS):/workspace -w /workspace $(RELEASE_IMAGE) \
		arm-linux-gnueabihf-strip /workspace/$(BIN)

# ---------------------------------------------------------------------------
# Quality / lockfile
# ---------------------------------------------------------------------------

fmt: ## Formatting check
	cargo fmt --all --check

clippy: ## Host lint
	cargo clippy --workspace --all-targets -- -D warnings

lock-check: ## Verify flake.lock matches flake.nix inputs (re-lock is a no-op when in sync)
	@set -euo pipefail; \
	before=$$(cksum flake.lock | awk '{print $$1}'); \
	nix flake lock "path:$(WS)" >/dev/null; \
	after=$$(cksum flake.lock | awk '{print $$1}'); \
	if [ "$$before" != "$$after" ]; then \
		echo "flake.lock CHANGED by re-lock — inputs out of sync with flake.nix" >&2; \
		exit 1; \
	fi; \
	echo "flake.lock is up to date"
