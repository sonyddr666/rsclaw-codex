#!/usr/bin/env bash
set -euo pipefail

REPO="${RSCLAW_REPO:-sonyddr666/rsclaw-codex}"
VERSION="${RSCLAW_VERSION:-latest}"
BASE_DIR="${RSCLAW_BASE_DIR:-/var/lib/rsclaw}"
ENV_DIR="/etc/rsclaw"
BIN_PATH="/usr/local/bin/rsclaw"
SERVICE_PATH="/etc/systemd/system/rsclaw.service"
TMP_DIR="$(mktemp -d)"

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

need_root() {
  if [ "${EUID:-$(id -u)}" -ne 0 ]; then
    echo "Run as root: sudo bash deploy/install-vps.sh" >&2
    exit 1
  fi
}

install_deps() {
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y curl ca-certificates tar
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y curl ca-certificates tar
  elif command -v yum >/dev/null 2>&1; then
    yum install -y curl ca-certificates tar
  else
    echo "Install curl ca-certificates tar manually, then re-run." >&2
  fi
}

latest_url() {
  if [ "$VERSION" = "latest" ]; then
    echo "https://github.com/${REPO}/releases/latest/download/rsclaw-linux-x86_64.tar.gz"
  else
    echo "https://github.com/${REPO}/releases/download/${VERSION}/rsclaw-linux-x86_64.tar.gz"
  fi
}

create_user() {
  if ! id rsclaw >/dev/null 2>&1; then
    useradd --system --home-dir "$BASE_DIR" --create-home --shell /usr/sbin/nologin rsclaw
  fi
  mkdir -p "$BASE_DIR" "$ENV_DIR" /var/log/rsclaw
  chown -R rsclaw:rsclaw "$BASE_DIR" /var/log/rsclaw
}

install_binary() {
  local url
  url="$(latest_url)"
  echo "Downloading $url"
  curl -fL "$url" -o "$TMP_DIR/rsclaw-linux-x86_64.tar.gz"
  tar -xzf "$TMP_DIR/rsclaw-linux-x86_64.tar.gz" -C "$TMP_DIR"
  install -m 0755 "$TMP_DIR/rsclaw" "$BIN_PATH"
  "$BIN_PATH" --version || true
}

write_env() {
  if [ ! -f "$ENV_DIR/rsclaw.env" ]; then
    cat > "$ENV_DIR/rsclaw.env" <<'ENV'
# Required for Telegram channel:
# TELEGRAM_BOT_TOKEN=123456:ABCDEF

# Required when using the merged codex-proxy provider:
CODEX_PROXY_BASE_URL=http://127.0.0.1:8787
CODEX_PROXY_SSE_PATH=/api/chat/sse

RSCLAW_BASE_DIR=/var/lib/rsclaw
ENV
    chmod 0600 "$ENV_DIR/rsclaw.env"
  fi
}

write_config() {
  if [ ! -f "$BASE_DIR/rsclaw.json5" ]; then
    cat > "$BASE_DIR/rsclaw.json5" <<'JSON5'
{
  gateway: {
    port: 18888,
    bind: "loopback"
  },

  models: {
    providers: {
      "codex-proxy": {
        baseUrl: "http://127.0.0.1:8787",
        models: [
          { id: "gpt-5.4-mini", name: "Codex 5.4 Mini" },
          { id: "gpt-5.4", name: "Codex 5.4" }
        ]
      }
    }
  },

  agents: {
    defaults: {
      model: {
        primary: "codex-proxy/gpt-5.4-mini",
        fallbacks: ["codex-proxy/gpt-5.4"],
        maxTokens: 4096,
        contextTokens: 64000
      },
      timeoutSeconds: 600,
      toolset: "minimal"
    }
  },

  channels: {
    telegram: {
      botToken: "${TELEGRAM_BOT_TOKEN}",
      dmPolicy: "pairing"
    }
  }
}
JSON5
    chown rsclaw:rsclaw "$BASE_DIR/rsclaw.json5"
  fi
}

install_service() {
  cat > "$SERVICE_PATH" <<'SERVICE'
[Unit]
Description=RsClaw gateway
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=rsclaw
Group=rsclaw
EnvironmentFile=-/etc/rsclaw/rsclaw.env
Environment=RSCLAW_BASE_DIR=/var/lib/rsclaw
ExecStart=/usr/local/bin/rsclaw start --foreground
Restart=always
RestartSec=5
WorkingDirectory=/var/lib/rsclaw

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=false
ReadWritePaths=/var/lib/rsclaw /var/log/rsclaw /tmp

[Install]
WantedBy=multi-user.target
SERVICE
  systemctl daemon-reload
  systemctl enable rsclaw
}

main() {
  need_root
  install_deps
  create_user
  install_binary
  write_env
  write_config
  install_service

  echo
  echo "Installed rsclaw. Edit /etc/rsclaw/rsclaw.env and set TELEGRAM_BOT_TOKEN."
  echo "Start with: systemctl start rsclaw"
  echo "Logs: journalctl -u rsclaw -f"
}

main "$@"
