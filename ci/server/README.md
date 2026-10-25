# Run COLA checks on a server

GitHub Actions runs `ci/check.sh` for pushes and pull requests. The server also
checks the latest commit of each branch in a root-owned registry. It resolves
each branch through the public GitHub ref API and downloads the exact commit
from GitHub codeload without credentials. It does not accept jobs from GitHub.

## Install the timer

Use a Linux server with systemd, Python 3.12 or newer, Git, curl, and a C linker.
Ubuntu 24.04 supplies Python 3.12. On Debian or Ubuntu, install the prerequisites
with these commands.

```sh
sudo apt-get update
sudo apt-get install -y python3 git curl build-essential ca-certificates
sudo ./ci/server/install.sh
```

The installer creates `cola-ci` with home `/home/cola-ci` and a disabled login
shell. It removes membership in `sudo` and `wheel`, and refuses an account with
remaining sudo grants. It installs Rust 1.98.1 through rustup. Existing config and
state survive repeated installs.

If Cargo needs a registry mirror on your server, configure it for `cola-ci` in
`/home/cola-ci/.cargo/config.toml`. Preserve any existing Cargo configuration.
The installer does not choose a mirror for hosted or local environments.

The installed controller is `/usr/local/lib/cola-ci/poll.py`, owned by root.
`/etc/cola-ci.conf` is also owned by root. The service runs as `cola-ci`, with
filesystem protections that allow writes to `/home/cola-ci`, `/var/lib/cola-ci`,
and its private temporary directory. These protections do not exhaust all writable
locations, such as `/dev/shm`. The unit limits memory to 3 GiB and CPU to two cores.

## Inspect results

```sh
sudo systemctl start cola-ci.service
systemctl status cola-ci.timer cola-ci.service
sudo journalctl -u cola-ci.service -n 100
sudo cat /var/lib/cola-ci/state.json
sudo find /var/lib/cola-ci/results -type f
```

`state.json` maps each trusted branch to its last completed result. Each record
has `sha`, `status`, `exit_code`, `started_at`, and `finished_at`. Status is
`success`, `failure`, or `unsupported`. A missing `ci/check.sh` is unsupported.
Branch directories under `results` use the hexadecimal bytes of the branch name.
Each attempt has a timestamped log. Completed attempts also have a JSON result.

The timer starts after boot and waits five minutes after each service invocation
ends. Three branches use at most 36 ref requests per hour at that interval, below
GitHub's unauthenticated limit of 60 requests per hour per source IP. Other API
users on the same IP share that limit.

A completed SHA is skipped without creating a new log, including a previous
failure. The failure stays in `state.json`. Any nonzero check exit is a completed
failure, even when an infrastructure error caused it. Use `--force` to retry that
SHA after fixing the infrastructure. Ref and archive errors, absent branches, and
timeouts fail the service and leave the last completed result unchanged.
The journal and attempt log identify the error. The next invocation retries unfinished work. At startup, the controller
removes orphaned files under `/var/lib/cola-ci/workspaces` from interrupted runs.
Use the service to serialize invocations against that directory.

## Change trusted branches

Edit `/etc/cola-ci.conf` as root. The default source is
`https://github.com/ShaoxinLi/COLA.git`. The default branch registry is
`main feature/domain-layer codex/server-ci`. Remove `codex/server-ci` after the
CI branch has merged. Remove `feature/domain-layer` after that branch has merged.
Retain `main` so the server continues to check new commits.

Set `COLA_CI_BRANCHES` to exact branch names separated by spaces. Do not add forks
or pull request refs. The service reads the config at its next invocation.
Run `sudo systemctl start cola-ci.service` to check immediately.

The controller accepts only an HTTPS `github.com/OWNER/REPO` source, with an
optional `.git` suffix. It requests
`https://api.github.com/repos/OWNER/REPO/git/ref/heads/BRANCH`, validates the commit
object and its 40-character SHA, then downloads
`https://codeload.github.com/OWNER/REPO/tar.gz/SHA`. Branch names are URL encoded.
Downloads use Python standard-library HTTPS with default certificate verification
and no environment proxy. Each network operation has a 120-second socket timeout;
each check has a 15-minute timeout. The service allows 65 minutes for the default
three branches. Increase `TimeoutStartSec` if you add enough branches to exceed
that limit. Archives use Python's `data` extraction filter and must contain one
root directory before the controller runs `ci/check.sh`.

Root configuration can override `COLA_CI_API_BASE` and `COLA_CI_ARCHIVE_BASE` for
isolated HTTP fixtures. Production uses the official HTTPS defaults above.

## Retry a completed commit

Stop the timer before the retry. Wait for any active service invocation to end.

```sh
sudo systemctl stop cola-ci.timer
```

Edit `/etc/cola-ci.conf` as root and set
`COLA_CI_ARGS=--force`. Run the same service so systemd serializes writes.

```sh
sudo systemctl start cola-ci.service
```

Clear `COLA_CI_ARGS` in `/etc/cola-ci.conf` after the invocation ends. Restart
the timer with `sudo systemctl start cola-ci.timer`.

## Trust and coverage limits

Only administrators choose source branches. A branch's code runs as `cola-ci`
and can modify that account's home and result files. The controller and registry
remain root-owned. Do not store credentials in the account's home. The controller
passes no token, SSH agent, or proxy environment to downloads or check scripts.
No self-hosted GitHub runner is installed.

The server polls the latest branch commit. Commits replaced between polls can
be missed. Server results stay local and are not GitHub required checks. GitHub
Actions supplies the pull request check UI. The server does not run code from
arbitrary pull requests. Logs and historical results are retained without an
automatic deletion policy. Inspect disk usage periodically.

Run the controller's isolated fixture checks with `python3 ci/server/test_poll.py`
on Python 3.12 or newer.
