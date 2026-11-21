#!/usr/bin/env bash
set -euo pipefail

resolve_path() {
  local target="$1"
  while [[ -L "${target}" ]]; do
    local dir
    dir="$(cd -P "$(dirname "${target}")" && pwd)"
    target="$(readlink "${target}")"
    [[ "${target}" != /* ]] && target="${dir}/${target}"
  done
  echo "${target}"
}

SCRIPT_PATH="$(resolve_path "${BASH_SOURCE[0]}")"
SCRIPT_DIR="$(cd -P "$(dirname "${SCRIPT_PATH}")" && pwd)"
REPO_ROOT="$(cd -P "${SCRIPT_DIR}/../.." && pwd)"
DATA_DIR="${1:-}"

export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:${PATH}"

BIN=""
if [[ -x "${SCRIPT_DIR}/informal" ]]; then
  BIN="${SCRIPT_DIR}/informal"
elif [[ -x "${REPO_ROOT}/target/release/informal" ]]; then
  BIN="${REPO_ROOT}/target/release/informal"
elif [[ -x "${REPO_ROOT}/target/debug/informal" ]]; then
  BIN="${REPO_ROOT}/target/debug/informal"
else
  BIN="$(command -v informal)"
fi

REAL_BIN="$(resolve_path "${BIN}")"
BIN_REPO_ROOT="$(cd -P "$(dirname "${REAL_BIN}")/../.." 2>/dev/null && pwd || true)"

ENGINES_ARGS=()
if [[ -f "config/engines.toml" ]]; then
  ENGINES_ARGS=(--engines "config/engines.toml")
elif [[ -f "${REPO_ROOT}/config/engines.toml" ]]; then
  ENGINES_ARGS=(--engines "${REPO_ROOT}/config/engines.toml")
elif [[ -n "${BIN_REPO_ROOT}" && -f "${BIN_REPO_ROOT}/config/engines.toml" ]]; then
  ENGINES_ARGS=(--engines "${BIN_REPO_ROOT}/config/engines.toml")
fi

# If attached to a running standalone InFormal.app window socket, bridge to it
# when INFORMAL_MCP_CONNECT=1; otherwise run a dedicated MCP session with UI App support.
if [[ "${INFORMAL_MCP_CONNECT:-0}" == "1" ]]; then
  exec "${BIN}" mcp --connect
fi

WORKDIR_ARGS=()
if [[ -n "${DATA_DIR}" ]]; then
  mkdir -p "${DATA_DIR}/informal_work"
  WORKDIR_ARGS=(--workdir "${DATA_DIR}/informal_work")
fi

exec "${BIN}" mcp --mode "${INFORMAL_AI_MODE:-propose}" "${ENGINES_ARGS[@]}" "${WORKDIR_ARGS[@]}"
