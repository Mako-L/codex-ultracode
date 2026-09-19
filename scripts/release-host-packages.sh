#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="$ROOT/.release-cache"
TAG="${1:-v0.155.0-workflow.2}"
VERSION="0.155.0-workflow.2"
NODE_VER="24.14.0"
DIST="$ROOT/dist"

export PATH="/usr/local/bin:$HOME/.orbstack/bin:/Applications/OrbStack.app/Contents/MacOS/xbin:/opt/homebrew/bin:$PATH"
if [ -x /usr/local/bin/docker ]; then
  DOCKER=/usr/local/bin/docker
elif [ -x "$HOME/.orbstack/bin/docker" ]; then
  DOCKER="$HOME/.orbstack/bin/docker"
else
  DOCKER=docker
fi

export CODEX_REPO_ROOT="$ROOT"

mkdir -p "$CACHE/node" "$CACHE/cargo" "$DIST"

ensure_node() {
  local arch="$1"
  local dir="$CACHE/node/node-v${NODE_VER}-linux-${arch}"
  if [ ! -x "$dir/bin/node" ]; then
    curl --retry 3 --retry-delay 2 --max-time 180 -fsSL \
      -o "$CACHE/node/node-v${NODE_VER}-linux-${arch}.tar.gz" \
      "https://nodejs.org/dist/v${NODE_VER}/node-v${NODE_VER}-linux-${arch}.tar.gz"
    tar -xzf "$CACHE/node/node-v${NODE_VER}-linux-${arch}.tar.gz" -C "$CACHE/node"
  fi
}

upload_asset() {
  local asset="$1"
  if ! command -v gh >/dev/null 2>&1; then
    return 0
  fi
  if [ "${GITHUB_ACTIONS:-}" != "true" ]; then
    local gh_user
    gh_user="$(gh api user --jq .login 2>/dev/null || true)"
    if [ "$gh_user" != "Mako-L" ]; then
      gh auth switch --user Mako-L
    fi
  fi
  gh release upload "$TAG" "$asset" --clobber --repo Mako-L/codex-ultracode || true
}

build_linux() {
  local platform="$1"
  local rust_target="$2"
  local node_arch="$3"
  local image="codex-ultracode-linux-${node_arch}"
  local target_cache="$CACHE/linux-${node_arch}-target"
  local archive="$DIST/codex-package-${rust_target}.tar.gz"

  mkdir -p "$target_cache"
  ensure_node "$node_arch"

  "$DOCKER" build \
    --platform "$platform" \
    --build-arg RUST_TARGET="$rust_target" \
    -t "$image" \
    "$ROOT/scripts/docker/linux-gnu"

  local node_root="/src/.release-cache/node/node-v${NODE_VER}-linux-${node_arch}"

  "$DOCKER" run --rm \
    --platform "$platform" \
    -e CODEX_REPO_ROOT=/src \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/src/codex-rs/target \
    -e PATH="${node_root}/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    -v "$ROOT:/src" \
    -v "$CACHE/cargo:/cargo" \
    -v "$target_cache:/src/codex-rs/target" \
    "$image" \
    python3 /src/scripts/build_codex_package.py \
      --target "$rust_target" \
      --variant codex \
      --cargo-profile release \
      --package-version "$VERSION" \
      --node-bin "${node_root}/bin/node" \
      --npm "${node_root}/bin/npm" \
      --package-dir "/src/dist/${rust_target}" \
      --archive-output "/src/dist/codex-package-${rust_target}.tar.gz" \
      --force

  upload_asset "$archive"
}

if [ "${RELEASE_LINUX_ARM:-1}" = "1" ]; then
  build_linux linux/arm64 aarch64-unknown-linux-gnu arm64
fi

if [ "${RELEASE_LINUX_X86:-1}" = "1" ]; then
  build_linux linux/amd64 x86_64-unknown-linux-gnu x64
fi
