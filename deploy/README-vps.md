# No-Docker VPS deploy

This path is for tiny VPS instances, for example 2 vCPU / 1 GB RAM.

Do not compile Rust on the VPS. Build the binary with GitHub Actions, download the release asset, then run it with systemd.

## 1. Build a release asset

Push a tag to run `.github/workflows/linux-binary.yml`:

```bash
git tag v2026.5.2-codex1
git push origin v2026.5.2-codex1
```

The workflow publishes:

```text
rsclaw-linux-x86_64.tar.gz
rsclaw-linux-x86_64.tar.gz.sha256
```

## 2. Install on VPS

```bash
curl -fsSL https://raw.githubusercontent.com/sonyddr666/rsclaw-codex/main/deploy/install-vps.sh | sudo bash
```

For a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/sonyddr666/rsclaw-codex/main/deploy/install-vps.sh \
  | sudo RSCLAW_VERSION=v2026.5.2-codex1 bash
```

## 3. Configure env

Edit:

```bash
sudo nano /etc/rsclaw/rsclaw.env
```

Set:

```bash
TELEGRAM_BOT_TOKEN=123456:ABCDEF
CODEX_PROXY_BASE_URL=http://127.0.0.1:8787
CODEX_PROXY_SSE_PATH=/api/chat/sse
```

## 4. Run your Codex proxy

Run your local Codex bridge on the VPS or on a private network address reachable from the VPS.

Expected endpoint:

```text
POST /api/chat/sse
```

The rsclaw provider sends JSON with `model`, `message`, `messages`, `tools`, and `stream: true`.

## 5. Start service

```bash
sudo systemctl start rsclaw
sudo systemctl status rsclaw
journalctl -u rsclaw -f
```

## Config path

The installer writes:

```text
/var/lib/rsclaw/rsclaw.json5
```

It uses:

```json5
models.providers["codex-proxy"].baseUrl = "http://127.0.0.1:8787"
agents.defaults.model.primary = "codex-proxy/gpt-5.4-mini"
channels.telegram.botToken = "${TELEGRAM_BOT_TOKEN}"
```

## Small VPS notes

Recommended:

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

Avoid running Chrome/browser automation or local LLMs on 1 GB RAM.
