#!/usr/bin/env bash
# PigIDE — one-shot release build.
#
# What it does:
#   1. Picks a Whisper backend (auto-detects GPU, or uses $PIGIDE_GPU).
#   2. Builds the frontend (pnpm) and the Tauri app (cargo tauri build).
#   3. Collects the binary + bundles into ./dist/ at the repo root.
#
# Usage:
#   ./scripts/build.sh                    # auto-detect GPU
#   PIGIDE_GPU=cuda    ./scripts/build.sh # force NVIDIA CUDA
#   PIGIDE_GPU=vulkan  ./scripts/build.sh # force Vulkan
#   PIGIDE_GPU=hipblas ./scripts/build.sh # force AMD ROCm
#   PIGIDE_GPU=metal   ./scripts/build.sh # force Apple Metal
#   PIGIDE_GPU=cpu     ./scripts/build.sh # force CPU-only

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log()  { printf '\033[1;36m[build]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[build]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[build]\033[0m %s\n' "$*" >&2; exit 1; }

detect_gpu() {
  if [[ -n "${PIGIDE_GPU:-}" ]]; then
    echo "$PIGIDE_GPU"; return
  fi
  case "$(uname -s)" in
    Darwin) echo "metal"; return ;;
  esac
  if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L 2>/dev/null | grep -q GPU; then
    if command -v nvcc >/dev/null 2>&1 || [[ -d /opt/cuda ]] || [[ -d /usr/local/cuda ]]; then
      echo "cuda"; return
    fi
    warn "NVIDIA GPU found but CUDA toolkit missing; falling back to vulkan if available"
  fi
  if command -v rocminfo >/dev/null 2>&1; then
    echo "hipblas"; return
  fi
  if command -v vulkaninfo >/dev/null 2>&1 && vulkaninfo --summary >/dev/null 2>&1; then
    echo "vulkan"; return
  fi
  echo "cpu"
}

GPU="$(detect_gpu)"
case "$GPU" in
  cuda|vulkan|hipblas|metal) FEATURES="--features gpu-${GPU}" ;;
  cpu)                        FEATURES="" ;;
  *) die "unknown PIGIDE_GPU=$GPU (expected: cuda|vulkan|hipblas|metal|cpu)" ;;
esac

# ---------- preflight: show exactly what will be compiled ----------
preflight() {
  if ! git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    warn "not a git repo — skipping preflight"
    return
  fi

  local branch commit dirty modified untracked ahead behind
  branch=$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)
  commit=$(git -C "$ROOT" rev-parse --short HEAD)
  modified=$(git -C "$ROOT" status --porcelain | grep -cE '^[ MARCD]M' || true)
  untracked=$(git -C "$ROOT" status --porcelain | grep -c '^??' || true)
  dirty=$((modified + untracked))

  printf '\n\033[1;35m=== BUILD PREFLIGHT ===\033[0m\n'
  printf '  branch:       %s\n' "$branch"
  printf '  commit:       %s — %s\n' "$commit" "$(git -C "$ROOT" log -1 --pretty=%s)"

  if [[ -n "$(git -C "$ROOT" rev-parse --verify --quiet origin/main)" ]]; then
    ahead=$(git -C "$ROOT" rev-list --count origin/main..HEAD 2>/dev/null || echo 0)
    behind=$(git -C "$ROOT" rev-list --count HEAD..origin/main 2>/dev/null || echo 0)
    printf '  vs origin/main: %s ahead, %s behind\n' "$ahead" "$behind"
  fi

  if (( dirty > 0 )); then
    printf '  \033[1;33muncommitted:  %d modified, %d untracked\033[0m\n' "$modified" "$untracked"
    printf '\n'
    git -C "$ROOT" status --short | sed 's/^/    /'
    printf '\n'
    warn "build будет включать незакоммиченные изменения — это нормально для тестов,"
    warn "но НЕ забудь закоммитить, иначе следующий 'git checkout' их сотрёт."
    if [[ -z "${PIGIDE_BUILD_DIRTY:-}" && -t 0 ]]; then
      read -r -p "  продолжить? [y/N] " ans
      [[ "$ans" =~ ^[Yy]$ ]] || die "aborted by user"
    fi
  else
    printf '  working tree: clean\n'
  fi
  printf '\033[1;35m=======================\033[0m\n\n'
}

preflight

log "GPU backend: $GPU"
log "cargo features: ${FEATURES:-<none>}"

command -v pnpm >/dev/null  || die "pnpm not found (need pnpm 9+)"
command -v cargo >/dev/null || die "cargo not found (need Rust 1.80+)"

log "installing frontend deps (pnpm install)"
pnpm --dir frontend install --frozen-lockfile

log "building Tauri app (release)"
TAURI_CLI="$ROOT/frontend/node_modules/.bin/tauri"
[[ -x "$TAURI_CLI" ]] || die "tauri CLI not found at $TAURI_CLI — run 'pnpm install' in frontend/"

(
  cd "$ROOT/src-tauri"
  # NO_STRIP=1 — AppImage bundling fails on already-stripped binaries
  # (release profile in workspace Cargo.toml has strip=true).
  # shellcheck disable=SC2086
  NO_STRIP=1 "$TAURI_CLI" build $FEATURES
)

log "collecting artifacts into ./dist/"
DIST="$ROOT/dist"
rm -rf "$DIST"
mkdir -p "$DIST/bin" "$DIST/bundle"

# workspace target lives at repo root (Cargo.toml workspace), not src-tauri/target
REL="$ROOT/target/release"
[[ -d "$REL" ]] || REL="$ROOT/src-tauri/target/release"

if [[ -x "$REL/pigide" ]]; then
  cp "$REL/pigide" "$DIST/bin/pigide"
fi
if [[ -x "$REL/pigide-cli" ]]; then
  cp "$REL/pigide-cli" "$DIST/bin/pigide-cli"
fi

if [[ -d "$REL/bundle" ]]; then
  shopt -s nullglob
  for src in "$REL/bundle"/*; do
    [[ -d "$src" ]] || continue
    name="$(basename "$src")"
    mkdir -p "$DIST/bundle/$name"
    cp -r "$src"/* "$DIST/bundle/$name/" 2>/dev/null || true
  done
  shopt -u nullglob
fi

cat > "$DIST/BUILD_INFO.txt" <<EOF
PigIDE release build
====================
date:        $(date -Iseconds)
git commit:  $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
git branch:  $(git -C "$ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)
git dirty:   $(git -C "$ROOT" status --porcelain 2>/dev/null | wc -l) uncommitted change(s)
gpu backend: $GPU
features:    ${FEATURES:-<none>}

Layout:
  bin/      raw executables (pigide, pigide-cli)
  bundle/   distributable installers (.deb / .rpm / .AppImage / .dmg / .msi)
EOF

log "done — see $DIST/"
ls -la "$DIST/bin" 2>/dev/null || true
