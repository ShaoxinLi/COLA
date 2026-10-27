# Operate the CI runners

CI uses two registered GitHub Actions runners, `cola-ci-1` and `cola-ci-2`, on
the Ubuntu 24.04 server. Both advertise the `cola-ci` label. Each runner processes
one job at a time. GitHub queues additional jobs and cancels older runs of the
same PR. Different PRs use different concurrency groups.

## Run the checks locally

Install Rust through rustup, then run `bash ci/check.sh`. The toolchain file pins
Rust, rustfmt, and Clippy. Clippy checks types and lints across all targets with
warnings treated as errors. Tests compile and run all targets. Separate commands
check doctests and documentation warnings. A separate build or type-check command
would repeat coverage already supplied by Clippy and tests.

The workflow uses a commit-pinned checkout action with credential persistence
disabled. For PRs, it checks GitHub's test merge commit, which includes the target
branch. The runner services use the existing rule-based HTTP proxy on
`127.0.0.1:7890`, which routes blocked domains through NaiveProxy. Localhost
bypasses the proxy so adapter tests can reach their local HTTP fixtures.
Cargo uses NaiveProxy's SOCKS listener on `127.0.0.1:1080` directly because the
rule proxy sends crates.io traffic over the unreliable direct route.

## Provision the server

The server needs Bash, Python 3, curl, tar, CA certificates, a C linker, systemd,
`network-proxy.service` with an HTTP listener on `127.0.0.1:7890`,
`naive-proxy.service` with a SOCKS listener on `127.0.0.1:1080`, and the
[official runner's Linux dependencies](https://github.com/actions/runner/blob/main/src/Misc/layoutbin/installdependencies.sh).
The rule proxy depends on NaiveProxy. Proxy outages fail network steps. CI does
not fall back to the unreliable direct route for Cargo downloads.
The provisioning script accepts the Linux x64 runner archive pinned by version
and SHA-256 in `provision.sh`, plus an installed Linux x64 Rust toolchain directory
containing Cargo, Clippy, and rustfmt. Obtain that toolchain with rustup using the
version in `rust-toolchain.toml`.

1. Set and verify the repository's external-contributor policy before registering
   either runner. Run these commands on a machine authenticated with `gh`.

   ```sh
   gh api --method PUT repos/ShaoxinLi/COLA/actions/permissions/fork-pr-contributor-approval \
     -f approval_policy=all_external_contributors
   gh api repos/ShaoxinLi/COLA/actions/permissions/fork-pr-contributor-approval
   ```

2. Copy this `ci` directory and `rust-toolchain.toml` together to an administrator
   staging directory on the server. Transfer the official runner archive from a
   machine that can reach GitHub releases, or download it through the server's
   proxy. Preserve the staging files' relative paths.

3. Generate a short-lived repository registration token and pipe it to the
   provisioning script over your administrator SSH connection. Substitute the
   server and staged paths. Keep the token out of command arguments and files.

   ```sh
   gh api --method POST repos/ShaoxinLi/COLA/actions/runners/registration-token --jq .token \
     | ssh root@SERVER 'bash /root/cola-ci-staging/ci/provision.sh /root/runner.tar.gz /path/to/rust-toolchain'
   ```

4. Confirm both services are active and both runners are online.

   ```sh
   systemctl status cola-runner@1 cola-runner@2
   gh api repos/ShaoxinLi/COLA/actions/runners --jq '.runners[] | {name,status,busy}'
   ```

Rerunning provisioning validates existing runner identities, enables automatic
updates, and restarts both services to apply configuration changes. It does not
replace registrations or need a token when both are already configured. Run
provisioning and toolchain maintenance only when both runners are idle.

Provisioning stops both services before switching the toolchain and restarts
them after configuration. Each Rust version has its own root-owned directory,
selected through an atomic symlink update. Versions never overlay one another.
If provisioning fails after stopping the services, CI stays paused. Correct the
reported error and rerun provisioning before expecting queued checks to start.

For CI infrastructure edits, run `actionlint` and `shellcheck ci/*.sh` locally.

## Maintain the runners

`cola-runner@.service` supplies each instance with a private Unix user, Cargo
home, workspace, and target directory under `/var/lib/cola-runners/1` or `2`.
Only that instance's directory is writable inside its service. The shared Rust
toolchain under `/opt/cola-ci/toolchain` belongs to root. Each service has one CPU
of quota, a 1536 MiB memory limit, and one Cargo compiler job. Systemd kills its
remaining processes when the service stops.

Targets persist outside the workspace. Before checks, a runner clears only its
own target cache if it exceeds 6 GiB. Cargo fingerprints decide which artifacts
need recompilation. Cargo's default incremental compilation remains enabled.
A toolchain or dependency change can produce a cold run.

The official runner can update itself through the proxy. The provisioner's
version and checksum pin the initial installation. Check online status after
updates. Review
[official releases](https://github.com/actions/runner/releases) when updating that
initial pin. For Rust updates, change the toolchain file, stage the matching
toolchain, and rerun provisioning while the runners are stopped.

Inspect service logs with `journalctl -u cola-runner@1 -u cola-runner@2`. Job logs
and results are in GitHub Actions. Stop both services to pause server CI without
altering the workflow.

## Admit trusted code only

Never approve fork workflows onto this server. Review a contribution and recreate
it as a repository branch before running it. The workflow's fork guard gives a
clear failure for the normal workflow. A fork can change that workflow, so the
guard does not protect the host after approval. GitHub documents this
[self-hosted runner risk](https://docs.github.com/en/actions/reference/security/secure-use).

Repository writers are trusted to execute code as the runner users. Persistent
runners retain state, and a job can read its own runner credentials and modify
its own cache. Separate users and systemd restrictions protect sibling state and
root-owned files. They do not provide disposable-machine isolation. Use disposable
machines or GitHub-hosted runners before admitting untrusted code. Do not add
provider credentials or unrelated service secrets to these accounts.

If you enable required status checks, require both `Rust checks` and `Fork policy`.
A skipped job alone does not reject a fork PR.

## Roll back

Stop both runner services and restore the previous workflow revision. The old
SSH checker configuration and its repository secrets remain available during
validation. After native CI is accepted, archive the old forced-command scripts
and SSH configuration outside their active paths and retire their secrets.
