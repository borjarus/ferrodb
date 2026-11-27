#!/usr/bin/env bash
# Build FerroDb release packages for macOS (native), Linux (Docker), and Windows (Parallels).
#
# Usage:
#   ./xtask/release-all.sh                  # build all platforms
#   ./xtask/release-all.sh macos linux      # build selected platforms
#
# Environment overrides:
#   MACOS_TARGETS   Space-separated Rust targets (default: "aarch64-apple-darwin")
#   LINUX_ARCH      Docker arch: amd64 | arm64 (default: "amd64")
#   WIN_VM          Parallels VM name (default: "Windows 11")
#   WIN_TARGET      Windows Rust target (default: "x86_64-pc-windows-msvc")
#   STOP_WIN_VM     Set to 1 to suspend/stop the VM if this script started it (default: "0")

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT_DIR}/target/releases"

MACOS_TARGETS="${MACOS_TARGETS:-aarch64-apple-darwin}"
LINUX_ARCH="${LINUX_ARCH:-amd64}"
WIN_VM="${WIN_VM:-Windows 11}"
WIN_TARGET="${WIN_TARGET:-x86_64-pc-windows-msvc}"
STOP_WIN_VM="${STOP_WIN_VM:-0}"

log() {
  printf '\n\033[1;34m==> %s\033[0m\n' "$*"
}

die() {
  printf '\033[1;31merror:\033[0m %s\n' "$*" >&2
  exit 1
}

ensure_cargo_packager() {
  if ! command -v cargo-packager >/dev/null 2>&1; then
    log "Installing cargo-packager on host..."
    cargo install cargo-packager --locked
  fi
}

build_macos() {
  log "Building macOS packages (${MACOS_TARGETS})..."
  ensure_cargo_packager
  mkdir -p "${OUT_DIR}/macos"

  cd "${ROOT_DIR}"
  for target in ${MACOS_TARGETS}; do
    if ! rustup target list --installed | grep -qx "${target}"; then
      log "Adding Rust target ${target}..."
      rustup target add "${target}"
    fi
    log "Compiling ferrodb-app (${target})..."
    cargo build --release --locked --target "${target}" -p ferrodb-app
    log "Packaging ferrodb-app (${target})..."
    cargo packager --release --target "${target}" -p ferrodb-app --out-dir "${OUT_DIR}/macos"
  done
}

build_linux() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed"
  docker info >/dev/null 2>&1 || die "docker daemon is not running (start Docker Desktop / OrbStack / Colima)"

  log "Building Linux packages (linux/${LINUX_ARCH}) in Docker..."
  mkdir -p "${OUT_DIR}/linux"

  docker run --rm --platform "linux/${LINUX_ARCH}" \
    -v "${ROOT_DIR}:/work" \
    -v "ferrodb-linux-cargo:/root/.cargo" \
    -w /work \
    -e "APPIMAGE_EXTRACT_AND_RUN=1" \
    -e "CARGO_TARGET_DIR=/work/target/linux-${LINUX_ARCH}" \
    ubuntu:24.04 bash -lc '
      set -euo pipefail
      export DEBIAN_FRONTEND=noninteractive
      if ! command -v cargo >/dev/null 2>&1; then
        apt-get update
        apt-get install -y --no-install-recommends curl ca-certificates build-essential cmake git file
        sed "s/sudo //g" ./xtask/linux-deps.sh | bash
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.99.0 --profile minimal
      fi
      export PATH="/root/.cargo/bin:${PATH}"
      if ! command -v cargo-packager >/dev/null 2>&1; then
        cargo install cargo-packager --locked
      fi
      cargo build --release --locked -p ferrodb-app
      cargo packager --release -p ferrodb-app --out-dir /work/target/releases/linux
    '
}

build_windows() {
  command -v prlctl >/dev/null 2>&1 || die "prlctl (Parallels CLI) is not installed"

  if ! prlctl list -a --info "${WIN_VM}" >/dev/null 2>&1; then
    die "Parallels VM '${WIN_VM}' not found. Set WIN_VM to one of your VMs (see 'prlctl list -a')."
  fi

  local started_by_us=0
  local vm_status
  vm_status="$(prlctl status "${WIN_VM}" | awk '{print $NF}')"
  if [[ "${vm_status}" != "running" ]]; then
    log "Starting Parallels VM '${WIN_VM}' (current state: ${vm_status})..."
    prlctl start "${WIN_VM}"
    started_by_us=1
  fi

  log "Waiting for Parallels Tools in '${WIN_VM}'..."
  local ready=0
  for _ in $(seq 1 60); do
    if prlctl exec "${WIN_VM}" cmd /c "echo ready" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 2
  done
  [[ "${ready}" -eq 1 ]] || die "Timed out waiting for Parallels Tools in '${WIN_VM}'"

  # Convert Mac path under $HOME to Parallels UNC path (\\Mac\Home\...)
  local rel_from_home
  if [[ "${ROOT_DIR}" != "${HOME}/"* ]]; then
    die "Repository must be under \$HOME (${HOME}) for Parallels \\\\Mac\\Home sharing"
  fi
  rel_from_home="${ROOT_DIR#"${HOME}/"}"
  local win_mac_repo="\\\\Mac\\Home\\${rel_from_home//\//\\}"
  local win_mac_out="${win_mac_repo}\\target\\releases\\windows"

  mkdir -p "${OUT_DIR}/windows"

  log "Syncing source to C:\\ferrodb-build and building (${WIN_TARGET}) in '${WIN_VM}'..."
  prlctl exec "${WIN_VM}" powershell -NoProfile -ExecutionPolicy Bypass -Command "
    \$ErrorActionPreference = 'Stop'
    \$src = '${win_mac_repo}'
    \$dst = 'C:\ferrodb-build'
    \$out = 'C:\ferrodb-build\out'

    if (-not (Test-Path \$src)) {
      throw \"Cannot access \$src. Ensure Parallels Shared Folders (Home) is enabled.\"
    }

    # robocopy exit codes 0..7 indicate success
    robocopy \$src \$dst /MIR /XD target .git .github spikes /NFL /NDL /NJH /NJS | Out-Null
    if (\$LASTEXITCODE -ge 8) { throw \"robocopy failed with exit code \$LASTEXITCODE\" }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
      \$env:Path = \"\$env:USERPROFILE\.cargo\bin;\$env:Path\"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
      throw 'Rust (cargo) is not installed in the Windows VM. Install via rustup-init.exe first.'
    }

    if (-not (Get-Command cargo-packager -ErrorAction SilentlyContinue)) {
      Write-Host 'Installing cargo-packager in Windows VM...'
      cargo install cargo-packager --locked
    }

    \$installedTargets = rustup target list --installed
    if (-not (\$installedTargets -contains '${WIN_TARGET}')) {
      Write-Host 'Adding Rust target ${WIN_TARGET}...'
      rustup target add '${WIN_TARGET}'
    }

    Set-Location \$dst
    New-Item -ItemType Directory -Force -Path \$out | Out-Null
    Remove-Item -Force -Recurse \"\$out\\*\" -ErrorAction SilentlyContinue

    cargo build --release --locked --target '${WIN_TARGET}' -p ferrodb-app
    if (\$LASTEXITCODE -ne 0) { throw \"cargo build failed\" }

    cargo packager --release --target '${WIN_TARGET}' -p ferrodb-app --out-dir \$out
    if (\$LASTEXITCODE -ne 0) { throw \"cargo packager failed\" }

    New-Item -ItemType Directory -Force -Path '${win_mac_out}' | Out-Null
    Copy-Item -Force -Path \"\$out\\*\" -Destination '${win_mac_out}'
  "

  if [[ "${started_by_us}" -eq 1 && "${STOP_WIN_VM}" == "1" ]]; then
    log "Suspending Parallels VM '${WIN_VM}'..."
    prlctl suspend "${WIN_VM}"
  fi
}

main() {
  local platforms=("$@")
  if [[ ${#platforms[@]} -eq 0 ]]; then
    platforms=(macos linux windows)
  fi

  for p in "${platforms[@]}"; do
    case "${p}" in
      macos)   build_macos ;;
      linux)   build_linux ;;
      windows) build_windows ;;
      -h|--help|help)
        sed -n '2,14p' "${BASH_SOURCE[0]}"
        exit 0
        ;;
      *)
        die "Unknown platform '${p}'. Expected: macos, linux, windows"
        ;;
    esac
  done

  log "Done! Release artifacts in ${OUT_DIR}:"
  find "${OUT_DIR}" -maxdepth 2 -type f | sort || true
}

main "$@"
