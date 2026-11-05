#!/bin/sh
# Installs (or upgrades) freedomoffice-server on Ubuntu with systemd.
#
#   sudo ./install.sh [path/to/freedomoffice-server]
#
# First install: asks for the domain and how HTTPS is done, writes
# /etc/freedomoffice/server.toml, creates the first administrator (password typed at the
# prompt) and starts the service. Later runs replace the binary and units and restart.
# Non-interactive: DOMAIN=files.example.com HTTPS=builtin|proxy EMAIL=you@example.com ADMIN=admin
# (the administrator password is then read from standard input).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
BIN=${1:-"$HERE/freedomoffice-server"}
CONFIG=/etc/freedomoffice/server.toml
DATA=/var/lib/freedomoffice
BACKUPS=/var/backups/freedomoffice
UNIT_DIR=/etc/systemd/system

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this as root: sudo $0 $*" >&2
    exit 1
fi
if [ ! -x "$BIN" ]; then
    echo "The server binary $BIN wasn't found (or isn't executable)." >&2
    exit 1
fi

echo "== Service user and directories"
if ! id freedomoffice >/dev/null 2>&1; then
    useradd --system --home-dir "$DATA" --no-create-home --shell /usr/sbin/nologin freedomoffice
fi
install -d -o freedomoffice -g freedomoffice -m 0750 "$DATA"
install -d -o freedomoffice -g freedomoffice -m 0750 "$BACKUPS"
install -d -o root -g freedomoffice -m 0750 /etc/freedomoffice

echo "== Binary and systemd units"
install -o root -g root -m 0755 "$BIN" /usr/local/bin/freedomoffice-server
install -o root -g root -m 0644 "$HERE/freedomoffice.service" "$UNIT_DIR/freedomoffice.service"
install -o root -g root -m 0644 "$HERE/freedomoffice-backup.service" "$UNIT_DIR/freedomoffice-backup.service"
install -o root -g root -m 0644 "$HERE/freedomoffice-backup.timer" "$UNIT_DIR/freedomoffice-backup.timer"

if [ ! -f "$CONFIG" ]; then
    echo "== First-time setup"
    DOMAIN=${DOMAIN:-}
    HTTPS=${HTTPS:-}
    EMAIL=${EMAIL:-}
    ADMIN=${ADMIN:-}
    if [ -z "$DOMAIN" ]; then
        printf "Domain name for the server (its DNS A record must point here), e.g. files.example.com: "
        read -r DOMAIN
    fi
    [ -n "$DOMAIN" ] || { echo "A domain name is required." >&2; exit 1; }
    if [ -z "$HTTPS" ]; then
        printf "HTTPS: [b]uilt-in Let's Encrypt on port 443, or behind a [p]roxy such as Caddy? [b/p]: "
        read -r answer
        case "$answer" in p|P|proxy) HTTPS=proxy ;; *) HTTPS=builtin ;; esac
    fi
    if [ "$HTTPS" = builtin ] && [ -z "$EMAIL" ]; then
        printf "Contact e-mail for Let's Encrypt (expiry notices; may be empty): "
        read -r EMAIL
    fi
    if [ -z "$ADMIN" ]; then
        printf "Administrator username [admin]: "
        read -r ADMIN
        ADMIN=${ADMIN:-admin}
    fi
    set -- --config "$CONFIG" init --data-dir "$DATA" --public-url "https://$DOMAIN" --admin "$ADMIN"
    if [ "$HTTPS" = builtin ]; then
        set -- "$@" --listen 0.0.0.0:443 --tls-domain "$DOMAIN"
        [ -n "$EMAIL" ] && set -- "$@" --acme-email "$EMAIL"
    else
        set -- "$@" --listen 127.0.0.1:8080
    fi
    /usr/local/bin/freedomoffice-server "$@"
    chown root:freedomoffice "$CONFIG"
    chmod 0640 "$CONFIG"
    chown -R freedomoffice:freedomoffice "$DATA"
fi

# Ports below 1024 need CAP_NET_BIND_SERVICE (and nothing else).
PORT=$(sed -n 's/^[[:space:]]*listen[[:space:]]*=[[:space:]]*"\(.*\):\([0-9]*\)".*/\2/p' "$CONFIG" | head -n 1)
DROPIN="$UNIT_DIR/freedomoffice.service.d"
if [ -n "$PORT" ] && [ "$PORT" -lt 1024 ]; then
    install -d -m 0755 "$DROPIN"
    install -o root -g root -m 0644 "$HERE/bind-privileged-port.conf" "$DROPIN/bind-privileged-port.conf"
else
    rm -f "$DROPIN/bind-privileged-port.conf"
fi

systemctl daemon-reload
systemctl enable freedomoffice.service freedomoffice-backup.timer >/dev/null
systemctl restart freedomoffice.service
systemctl start freedomoffice-backup.timer

echo
echo "FreedomOffice server is running (systemctl status freedomoffice; journalctl -u freedomoffice -f)."
echo "Open the firewall for HTTPS, e.g.: ufw allow 443/tcp"
echo "Add users with: sudo -u freedomoffice freedomoffice-server --config $CONFIG user add USERNAME"
echo "Nightly backups go to $BACKUPS (freedomoffice-backup.timer)."
