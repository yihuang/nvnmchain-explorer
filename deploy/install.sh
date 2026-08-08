#!/usr/bin/env bash
#
# Lightweight installer for nvnmchain-explorer:
#   builds the release binary, drops it in /opt, creates a dedicated system
#   user, and registers a hardened systemd service.
#
# Usage:
#   sudo ./deploy/install.sh            # build + install + start
#   sudo ./deploy/install.sh --uninstall
#
# Overrides:
#   PREFIX=/opt/nvnmchain-explorer      binary directory
#   DATA_DIR=/var/lib/nvnmchain-explorer
#   APP_USER=nvnmchain
set -euo pipefail

APP="nvnmchain-explorer"
PREFIX="${PREFIX:-/opt/${APP}}"
DATA_DIR="${DATA_DIR:-/var/lib/${APP}}"
APP_USER="${APP_USER:-nvnmchain}"
UNIT="/etc/systemd/system/${APP}.service"
ENV_FILE="/etc/${APP}.env"
BIN_SRC="target/release/${APP}"

if [[ $# -gt 0 && "$1" == "--uninstall" ]]; then
  systemctl disable --now "${APP}" 2>/dev/null || true
  rm -f "${UNIT}"
  systemctl daemon-reload
  echo "Removed systemd service. Binary (${PREFIX}) and data (${DATA_DIR}) left in place."
  exit 0
fi

if [[ ! -f "${BIN_SRC}" ]]; then
  echo "Release binary not found (${BIN_SRC}); run 'make build' first." >&2
  exit 1
fi

if [[ $EUID -ne 0 ]]; then
  echo "Run with sudo/root to install systemd service and system user." >&2
  exit 1
fi

# Dedicated service user (no shell, no home).
if ! id "${APP_USER}" &>/dev/null; then
  useradd --system --no-create-home --home-dir "${DATA_DIR}" \
    --shell /usr/sbin/nologin "${APP_USER}"
  echo "Created system user '${APP_USER}'"
fi

install -d -m 0755 "${PREFIX}" "${DATA_DIR}"
install -o "${APP_USER}" -g "${APP_USER}" -m 0750 -d "${DATA_DIR}"
install -o root -g root -m 0755 "${BIN_SRC}" "${PREFIX}/${APP}"
install -m 0644 "$(dirname "$0")/${APP}.service" "${UNIT}"

if [[ ! -f "${ENV_FILE}" ]]; then
  install -m 0640 "$(dirname "$0")/${APP}.env.example" "${ENV_FILE}"
  chown root:"${APP_USER}" "${ENV_FILE}"
  echo "Created ${ENV_FILE} from example (review it, then start the service)."
fi

systemctl daemon-reload
systemctl enable --now "${APP}"
systemctl --no-pager --full status "${APP}" || true

echo
echo "Installed:"
echo "  binary   ${PREFIX}/${APP}"
echo "  data     ${DATA_DIR}"
echo "  service  ${UNIT} (edit ${ENV_FILE} for overrides)"
echo "  http     $(systemctl show -p ExecMainStartTimestamp --value "${APP}" >/dev/null 2>&1 && hostname -I | awk '{print $1}' || echo localhost):8080"
