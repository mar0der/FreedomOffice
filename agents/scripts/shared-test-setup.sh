#!/usr/bin/env bash
# Creates (or brings up to date) the FreedomOffice SHARED TEST instance on the21th.
# Idempotent: re-running keeps existing accounts, passwords, files and data.
#
#   - separate from production: its own data dir, its own port, its own systemd user unit
#     (freedomoffice-sharedtest.service); never touches freedomoffice-server.service or :8080
#   - listens ONLY on the Tailscale address (100.x.y.z:8090): not 127.0.0.1, not the LAN, not
#     0.0.0.0. HTTPS with the tailnet certificate from `tailscale cert`, so normal TLS
#     verification works for clients that reach https://<machine>.<tailnet>.ts.net:8090
#   - org "TestOrg" (members edit by default); accounts testadmin (org admin), test-a..test-d
#     and agent-claude (members); three sample workbooks in the org folder
#   - passwords: `openssl rand` into 0600 files under $ROOT/secrets/<user>.pw. They are given to
#     tools through standard input or files only: never on a command line, never in the
#     environment, never printed.
#
# Usage: agents/scripts/shared-test-setup.sh        (from a checkout of the branch to deploy)
#   ROOT=~/freedomoffice-shared-test   instance folder
#   PORT=8090
#   SKIP_BUILD=1                       use the binaries already in target/release
set -euo pipefail
umask 077

ROOT="${ROOT:-$HOME/freedomoffice-shared-test}"
PORT="${PORT:-8090}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UNIT=freedomoffice-sharedtest
SERVER="$ROOT/bin/freedomoffice-server"
CFG="$ROOT/server.toml"
USERS=(testadmin test-a test-b test-c test-d agent-claude)

say() { printf '== %s\n' "$*"; }

# --- addresses -------------------------------------------------------------------------------
TS_IP="$(tailscale ip -4 | head -n1)"
DOMAIN="$(tailscale status --json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')"
[ -n "$TS_IP" ] && [ -n "$DOMAIN" ] || { echo "Tailscale is not up." >&2; exit 1; }
say "Tailscale address $TS_IP, name $DOMAIN"

# --- build -----------------------------------------------------------------------------------
if [ -z "${SKIP_BUILD:-}" ]; then
  say "Building the server and fsheets from $(git -C "$REPO" rev-parse --short HEAD)"
  (cd "$REPO" && cargo build --release -p freedomoffice-server -p freedomsheets-agents)
fi
mkdir -p "$ROOT"/{bin,data,secrets,tls,backup}
chmod 700 "$ROOT" "$ROOT/secrets" "$ROOT/tls"
BUILD="$(git -C "$REPO" rev-parse --short HEAD)"
RESTART=0
if ! cmp -s "$REPO/target/release/freedomoffice-server" "$SERVER"; then
  if [ -f "$SERVER" ] && [ -d "$ROOT/data/files" ]; then
    say "Backing up the data before replacing the binary"
    "$SERVER" --config "$CFG" backup "$ROOT/backup" >/dev/null
  fi
  install -m 755 "$REPO/target/release/freedomoffice-server" "$SERVER.new" && mv "$SERVER.new" "$SERVER"
  RESTART=1
fi
install -m 755 "$REPO/target/release/fsheets" "$ROOT/bin/fsheets"
echo "$BUILD" > "$ROOT/bin/BUILD"

# --- certificate and configuration -----------------------------------------------------------
if [ ! -s "$ROOT/tls/cert.pem" ]; then
  say "Fetching the tailnet certificate"
  tailscale cert --cert-file "$ROOT/tls/cert.pem" --key-file "$ROOT/tls/key.pem" "$DOMAIN"
fi
if [ ! -f "$CFG" ]; then
  cat > "$CFG" <<EOF
# FreedomOffice SHARED TEST instance (collab-365). Tailscale IP only; not production.
listen = "$TS_IP:$PORT"
data_dir = "$ROOT/data"
public_url = "https://$DOMAIN:$PORT"
tls_cert = "$ROOT/tls/cert.pem"
tls_key = "$ROOT/tls/key.pem"
allow_insecure_http = false
trust_proxy = false
max_upload_mb = 100
versions_to_keep = 25
link_access_hours = 12
allow_links = false
EOF
fi

# --- accounts, organisation -------------------------------------------------------------------
for u in "${USERS[@]}"; do
  f="$ROOT/secrets/$u.pw"
  # 24 characters from a CSPRNG; the file is the only place the password exists.
  [ -s "$f" ] || openssl rand -base64 18 | tr -d '\n' > "$f"
  chmod 600 "$f"
done
existing="$("$SERVER" --config "$CFG" user list | awk 'NR>1{print $1}')"
for u in "${USERS[@]}"; do
  if ! grep -qx "$u" <<<"$existing"; then
    say "Creating $u"
    if [ "$u" = testadmin ]; then
      "$SERVER" --config "$CFG" user add "$u" --display-name "Test Admin" --admin < "$ROOT/secrets/$u.pw"
    else
      "$SERVER" --config "$CFG" user add "$u" --display-name "$u" < "$ROOT/secrets/$u.pw"
    fi
  fi
done
"$SERVER" --config "$CFG" org list | awk 'NR>1{print $1}' | grep -qx TestOrg || "$SERVER" --config "$CFG" org add TestOrg
"$SERVER" --config "$CFG" org add-member TestOrg testadmin admin >/dev/null
for u in test-a test-b test-c test-d agent-claude; do
  "$SERVER" --config "$CFG" org add-member TestOrg "$u" member >/dev/null
done

# --- the service ------------------------------------------------------------------------------
mkdir -p "$HOME/.config/systemd/user"
cat > "$HOME/.config/systemd/user/$UNIT.service" <<EOF
# FreedomOffice SHARED TEST instance (branch collab-365). Binds the21th's Tailscale IP only
# ($TS_IP:$PORT, HTTPS with the tailnet cert). Not production; data in $ROOT.
[Unit]
Description=FreedomOffice shared TEST server (Tailscale IP only, :$PORT)
After=network-online.target tailscaled.service

[Service]
Type=simple
ExecStart=$SERVER --config $CFG serve
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info
NoNewPrivileges=true
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=true
UMask=0077

[Install]
WantedBy=default.target
EOF
systemctl --user daemon-reload
systemctl --user enable "$UNIT.service" >/dev/null 2>&1
if [ "$RESTART" = 1 ]; then systemctl --user restart "$UNIT.service"; else systemctl --user start "$UNIT.service"; fi
for _ in $(seq 1 30); do
  curl -fsS --max-time 2 "https://$DOMAIN:$PORT/api/health" >/dev/null 2>&1 && break
  sleep 1
done
curl -fsS --max-time 5 "https://$DOMAIN:$PORT/api/health" >/dev/null || { echo "The instance doesn't answer on https://$DOMAIN:$PORT" >&2; exit 1; }

# --- sample workbooks in TestOrg's folder ------------------------------------------------------
say "Seeding sample workbooks (as test-a)"
TOKEN="$ROOT/secrets/seed.token"
"$ROOT/bin/fsheets" login --server "https://$DOMAIN:$PORT" --user test-a --token-file "$TOKEN" --password-file "$ROOT/secrets/test-a.pw"
FOLDER="$("$ROOT/bin/fsheets" ls --token-file "$TOKEN" | awk -F'\t' '$1=="org TestOrg"{sub("folder ","",$2); print $2}')"
"$ROOT/bin/fsheets" seed --token-file "$TOKEN" --folder "$FOLDER"
rm -f "$TOKEN"
"$SERVER" --config "$CFG" tokens revoke test-a

say "Done: https://$DOMAIN:$PORT  (org folder $FOLDER)"
echo "Passwords: $ROOT/secrets/<user>.pw (0600)."
