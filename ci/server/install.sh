#!/bin/sh
set -eu
[ "$(id -u)" -eq 0 ] || { echo "Run as root." >&2; exit 1; }
cd "$(dirname "$0")"
/usr/bin/python3 -c 'import sys; sys.exit(0 if sys.version_info >= (3, 12) else "Python 3.12 or newer is required.")'
command -v git >/dev/null
command -v curl >/dev/null
if ! id cola-ci >/dev/null 2>&1; then
    useradd --system --user-group --create-home --home-dir /home/cola-ci --shell /usr/sbin/nologin cola-ci
fi
[ "$(getent passwd cola-ci | cut -d: -f6)" = /home/cola-ci ] || {
    echo "cola-ci must have home /home/cola-ci." >&2; exit 1;
}
usermod --shell /usr/sbin/nologin cola-ci
for group in sudo wheel; do
    if id -nG cola-ci | tr ' ' '\n' | grep -qx "$group"; then
        gpasswd -d cola-ci "$group"
    fi
done
if command -v sudo >/dev/null 2>&1 && sudo -l -U cola-ci 2>/dev/null | grep -q '^[[:space:]]*('; then
    echo "Remove sudo grants for cola-ci before installing." >&2
    exit 1
fi
install -d -o cola-ci -g cola-ci -m 0750 /home/cola-ci /var/lib/cola-ci
install -d -o root -g root -m 0755 /usr/local/lib/cola-ci
install -o root -g root -m 0755 poll.py /usr/local/lib/cola-ci/poll.py
if [ ! -e /etc/cola-ci.conf ]; then
    install -o root -g root -m 0644 cola-ci.conf /etc/cola-ci.conf
fi
chown root:root /etc/cola-ci.conf
chmod 0644 /etc/cola-ci.conf
install -o root -g root -m 0644 cola-ci.service cola-ci.timer /etc/systemd/system/
if [ ! -x /home/cola-ci/.cargo/bin/rustup ]; then
    script=$(mktemp)
    trap 'rm -f "$script"' EXIT HUP INT TERM
    curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o "$script"
    chmod 0755 "$script"
    runuser -u cola-ci -- env HOME=/home/cola-ci sh "$script" -y --profile minimal --default-toolchain 1.98.1
fi
if ! runuser -u cola-ci -- env HOME=/home/cola-ci /home/cola-ci/.cargo/bin/rustup run 1.98.1 rustfmt --version >/dev/null 2>&1 ||
   ! runuser -u cola-ci -- env HOME=/home/cola-ci /home/cola-ci/.cargo/bin/rustup run 1.98.1 clippy-driver --version >/dev/null 2>&1; then
    runuser -u cola-ci -- env HOME=/home/cola-ci /home/cola-ci/.cargo/bin/rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
fi
systemctl daemon-reload
systemctl enable --now cola-ci.timer
