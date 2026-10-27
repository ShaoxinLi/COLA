#!/usr/bin/env bash
set -euo pipefail

if [[ $EUID != 0 || $# != 2 ]]; then
  echo "Usage as root: $0 RUNNER_ARCHIVE RUST_TOOLCHAIN_DIRECTORY" >&2
  exit 2
fi

script_dir=$(cd "$(dirname "$0")" && pwd)
archive=$(realpath "$1")
rust_source=$(realpath "$2")
runner_version=2.338.2
runner_sha256=baebf204307bac6193480852732f0aba2ae97b6c5597c244f4d5558390147c40
rust_version=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' "$script_dir/../rust-toolchain.toml")

printf '%s  %s\n' "$runner_sha256" "$archive" | sha256sum --check
printf 'Initial runner release %s\n' "$runner_version"
[[ $("$rust_source/bin/rustc" --version) == "rustc $rust_version "* ]]
[[ -x "$rust_source/bin/cargo-clippy" && -x "$rust_source/bin/rustfmt" ]]
[[ $rust_source != /opt/cola-ci/toolchain ]]

toolchain=/opt/cola-ci/toolchains/$rust_version
install -d -m 0755 /opt/cola-ci/toolchains /var/lib/cola-runners
if [[ ! -d $toolchain ]]; then
  staging=$(mktemp -d /opt/cola-ci/toolchains/.install-XXXXXX)
  trap 'rm -rf "$staging"' EXIT
  cp -a "$rust_source/." "$staging/"
  chown -R root:root "$staging"
  chmod -R go-w "$staging"
  chmod 0755 "$staging"
  mv "$staging" "$toolchain"
fi
install -m 0644 "$script_dir/cola-runner@.service" /etc/systemd/system/
systemctl daemon-reload
systemctl stop cola-runner@1.service cola-runner@2.service
ln -sfn "$toolchain" /opt/cola-ci/toolchain.next
mv -Tf /opt/cola-ci/toolchain.next /opt/cola-ci/toolchain

registration_token=
for slot in 1 2; do
  user=cola-runner-$slot
  home=/var/lib/cola-runners/$slot
  if ! id "$user" &>/dev/null; then
    useradd --system --user-group --home-dir "$home" --shell /usr/sbin/nologin "$user"
  fi
  install -d -m 0700 -o "$user" -g "$user" "$home"
  runuser -u "$user" -- mkdir -p "$home/runner" "$home/.cargo"
  if [[ ! -f "$home/runner/.runner" ]]; then
    runuser -u "$user" -- tar -xzf - -C "$home/runner" < "$archive"
    if [[ -z ${registration_token:-} ]]; then
      read -r registration_token
    fi
    (
      cd "$home/runner"
      ACTIONS_RUNNER_INPUT_TOKEN="$registration_token" runuser -u "$user" -- \
        env http_proxy=http://127.0.0.1:7890 https_proxy=http://127.0.0.1:7890 \
        no_proxy=localhost,127.0.0.1,::1 \
        ./config.sh --unattended --url https://github.com/ShaoxinLi/COLA \
        --name "cola-ci-$slot" --labels cola-ci --work _work --replace
    )
  else
    runuser -u "$user" -- python3 - "$home/runner/.runner" "cola-ci-$slot" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8-sig") as stream:
    runner = json.load(stream)
assert runner["agentName"] == sys.argv[2], "Runner name mismatch"
assert runner["gitHubUrl"].rstrip("/") == "https://github.com/ShaoxinLi/COLA", "Repository mismatch"
runner["disableUpdate"] = False
with open(sys.argv[1], "w") as stream:
    json.dump(runner, stream, indent=2)
    stream.write("\n")
PY
    [[ -x "$home/runner/bin/Runner.Listener" ]]
  fi
  runuser -u "$user" -- rm -f "$home/runner/.path"
done

systemctl enable --now cola-runner@1.service cola-runner@2.service
