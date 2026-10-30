---
name: verify
description: Verify COLA's Rust library through its public integration tests. Use after changes to agent chats, session persistence, host tools, provider adapters, or conversation contracts, and when runtime proof of a library behavior is needed.
---

# Verify COLA

COLA's user is a Rust caller. Drive the exported library through the integration
tests in `tests/app.rs`, `tests/adapters.rs`, and `tests/domain.rs`.
Read [the feature map](features/README.md) and the relevant feature before starting.

## Launch

Assemble the five Bash blocks below into one script in section order. Run it
once with `bash` from the checkout being verified. For another feature, replace
the Drive selection before running the script. Separate shell tool calls lose
the variables and trigger cleanup as soon as Launch exits. Prerequisites are
a Rust toolchain supporting edition 2024, Cargo, Python 3, Git, and `shasum`.
Host tool journeys also need Bash, `tr`, and Unix `mkfifo` for the FIFO case.
Cargo may download locked dependencies during launch. The drive uses local
fixtures with dummy credentials and needs no provider keys or seed database.

```bash
set -euo pipefail
COLA_VERIFY_ROOT=$(git rev-parse --show-toplevel)
cd "$COLA_VERIFY_ROOT"
mkdir -p .scratch/verify
COLA_VERIFY_EVIDENCE=$(mktemp -d "$COLA_VERIFY_ROOT/.scratch/verify/run.XXXXXXXX")
printf 'Evidence directory %s\n' "$COLA_VERIFY_EVIDENCE"
COLA_VERIFY_STATE=$(mktemp -d "${TMPDIR:-/tmp}/cola-verify.XXXXXXXX")
export COLA_VERIFY_EVIDENCE COLA_VERIFY_STATE
trap 'rm -rf -- "$COLA_VERIFY_STATE"' EXIT
export CARGO_TARGET_DIR="$COLA_VERIFY_STATE/target"
export TMPDIR="$COLA_VERIFY_STATE/tmp/"
mkdir -p "$CARGO_TARGET_DIR" "$TMPDIR"
printf '%s\n' "$COLA_VERIFY_STATE" > "$COLA_VERIFY_EVIDENCE/state.path"
git rev-parse HEAD > "$COLA_VERIFY_EVIDENCE/revision.txt"
git status --short > "$COLA_VERIFY_EVIDENCE/worktree.txt"
git diff HEAD -- Cargo.toml Cargo.lock src tests > "$COLA_VERIFY_EVIDENCE/source.diff"
find Cargo.toml Cargo.lock src tests -type f | LC_ALL=C sort \
    > "$COLA_VERIFY_EVIDENCE/source-files.txt"
while IFS= read -r COLA_VERIFY_SOURCE; do
    shasum -a 256 "$COLA_VERIFY_SOURCE"
done < "$COLA_VERIFY_EVIDENCE/source-files.txt" > "$COLA_VERIFY_EVIDENCE/sources.sha256"
rustc --version > "$COLA_VERIFY_EVIDENCE/toolchain.txt"
cargo --version >> "$COLA_VERIFY_EVIDENCE/toolchain.txt"
cargo metadata --locked --no-deps --format-version 1 > "$COLA_VERIFY_EVIDENCE/metadata.json"
cargo test --locked --all-targets --no-run --message-format=json \
    > "$COLA_VERIFY_EVIDENCE/build.jsonl" 2> "$COLA_VERIFY_EVIDENCE/build.stderr"
python3 - <<'PY'
import json
import os
from pathlib import Path

evidence = Path(os.environ["COLA_VERIFY_EVIDENCE"])
artifacts = [json.loads(line) for line in (evidence / "build.jsonl").read_text().splitlines()]
for target in ("adapters", "app", "domain"):
    paths = [item["executable"] for item in artifacts
             if item.get("reason") == "compiler-artifact"
             and item["target"]["name"] == target
             and item["target"]["kind"] == ["test"]
             and item.get("executable")]
    assert len(paths) == 1, (target, paths)
    (evidence / (target + ".path")).write_text(paths[0] + "\n")
PY
read -r COLA_VERIFY_ADAPTERS < "$COLA_VERIFY_EVIDENCE/adapters.path"
read -r COLA_VERIFY_APP < "$COLA_VERIFY_EVIDENCE/app.path"
read -r COLA_VERIFY_DOMAIN < "$COLA_VERIFY_EVIDENCE/domain.path"
shasum -a 256 "$COLA_VERIFY_ADAPTERS" "$COLA_VERIFY_APP" "$COLA_VERIFY_DOMAIN" \
    > "$COLA_VERIFY_EVIDENCE/binaries.sha256"
```

Ready means the build exits zero, all three executable paths exist, and Doctor
passes. No server remains running. Tests construct fresh sessions, temporary
files, and HTTP listeners as needed. There are no migrations or login steps.
On launch failure, let the script's exit trap run. Keep its logs and start a fresh run.

## Doctor

Run this check before a drive and whenever behavior looks wrong. It reads the
checkout and exact built executables and writes a fresh evidence record. It then
lists the public test entry points. A mismatch requires a fresh launch.

```bash
COLA_VERIFY_DOCTOR=$(mktemp -d "$COLA_VERIFY_EVIDENCE/doctor.XXXXXXXX")
(
    cd "$COLA_VERIFY_ROOT"
    find Cargo.toml Cargo.lock src tests -type f | LC_ALL=C sort \
        > "$COLA_VERIFY_DOCTOR/source-files.txt"
    cmp "$COLA_VERIFY_EVIDENCE/source-files.txt" "$COLA_VERIFY_DOCTOR/source-files.txt"
    shasum -a 256 -c "$COLA_VERIFY_EVIDENCE/sources.sha256"
    shasum -a 256 -c "$COLA_VERIFY_EVIDENCE/binaries.sha256"
    test -x "$COLA_VERIFY_ADAPTERS"
    test -x "$COLA_VERIFY_APP"
    test -x "$COLA_VERIFY_DOMAIN"
    "$COLA_VERIFY_ADAPTERS" --list
    "$COLA_VERIFY_APP" --list
    "$COLA_VERIFY_DOMAIN" --list
) > "$COLA_VERIFY_DOCTOR/doctor.log" 2>&1
cat "$COLA_VERIFY_DOCTOR/doctor.log"
```

Each run has its own build and temporary directories. HTTP fixtures bind
`127.0.0.1:0`, so concurrent runs do not share ports. The host tool path-anchor
test also creates a temporary directory in the checkout and removes it on drop.
Use one service writer per session when adding journeys. Storage has no stale
write detection. Never point a test at a user's `.cola` directory.

## Drive

Use the compiled Rust test runner. Filters are stable module paths from this
checkout, not substring guesses about behavior. The following complete journey
verifies JSONL persistence through `SessionStore::save`, `load`, and `list`.

```bash
COLA_VERIFY_FEATURE=session-persistence
(
    set -x
    "$COLA_VERIFY_ADAPTERS" jsonl:: --nocapture --test-threads=1
) > "$COLA_VERIFY_EVIDENCE/$COLA_VERIFY_FEATURE.log" 2>&1 && COLA_VERIFY_RESULT=0 || COLA_VERIFY_RESULT=$?
printf '%s\n' "$COLA_VERIFY_RESULT" > "$COLA_VERIFY_EVIDENCE/$COLA_VERIFY_FEATURE.exit-code"
cat "$COLA_VERIFY_EVIDENCE/$COLA_VERIFY_FEATURE.log"
test "$COLA_VERIFY_RESULT" -eq 0
grep -Eq '^test result: ok\. [1-9][0-9]* passed; 0 failed;' \
    "$COLA_VERIFY_EVIDENCE/$COLA_VERIFY_FEATURE.log"
```

For another feature, use its feature ID and replace the runner invocation with
the exact command in its map. Keep the same capture block. Select the whole
mapped group for feature-level proof. For a narrower investigation, append
`--exact` to a full test name from Doctor and report that single entry only.
A zero-test run is a failure even when the runner exits zero.

Read the relevant test bodies before reporting coverage. Integration tests are
external crate consumers of `cola`, so public session transitions are user
operations. Most service cases substitute collaborators through production
traits. The native provider service cases combine the real JSONL store and HTTP
adapters with local provider fixtures and a tool double. Adapter tests separately
verify real disk, subprocess, and HTTP effects. No journey combines live
providers with real host tools.

## Evidence

Retain `$COLA_VERIFY_EVIDENCE`, printed at launch and cleanup, under `.scratch/verify/`.
This location is ignored by Git and survives temporary-state removal.
For each feature retain its action trace, combined output, exit code, Doctor
inventory, build log, revision, source diff, and source and binary hashes.
Copy the relevant test sources into the evidence before cleanup.

```bash
mkdir -p "$COLA_VERIFY_EVIDENCE/test-sources"
cp -R tests/. "$COLA_VERIFY_EVIDENCE/test-sources/"
```

Prove the action and resulting state through assertions on public return values
and a second view of side effects. JSONL tests read raw bytes and load and list
stored sessions. Tool tests read actual files, check rejected writes leave bytes
unchanged, and inspect real subprocess output. Provider tests inspect captured
HTTP requests and public responses. A final screen or a successful build alone
does not prove a journey.

Mocks belong at the library's existing `ModelClient`, `SessionStore`, and `Tool`
ports. Local provider fixtures exercise real HTTP encoding and decoding.
Even tests named `real_providers` use local fixtures. Report live provider auth,
model availability, quotas, and current wire compatibility as unverified.
Temporary files disappear on normal test completion. Their read-back assertions
and retained source are the evidence, not a retained raw session archive.

There is no dry-run mode in this recipe. Tests perform real temporary writes,
loopback network requests, and host commands. If introducing a dry-run path,
observe its files, network requests, and Git refs to establish what it skips.
Report a missing fixture or inaccessible entry point as failed or unverified.
Do not replace a required journey with an easier passing entry point.

## Cleanup

The exit trap removes state after every attempt, including failures. The block
below confirms cleanup and retained evidence after a successful drive. A failed
step exits before these confirmation checks. Use the evidence path printed at
launch to inspect the failure. Confirm the directory named in `state.path` no
longer exists.
Normal test completion drops its temporary directories and joins fixture
threads. All commands run in the
foreground. If interrupted, stop only this shell's foreground job and wait for
it to exit before removing state. Do not kill by process name. Inspect any
leftover checkout temporary directory for ownership before removing it.

```bash
rm -rf -- "$COLA_VERIFY_STATE"
test ! -e "$COLA_VERIFY_STATE"
test -s "$COLA_VERIFY_EVIDENCE/build.jsonl"
test -s "$COLA_VERIFY_DOCTOR/doctor.log"
test -s "$COLA_VERIFY_EVIDENCE/$COLA_VERIFY_FEATURE.log"
test -f "$COLA_VERIFY_EVIDENCE/$COLA_VERIFY_FEATURE.exit-code"
printf 'Evidence retained at %s\n' "$COLA_VERIFY_EVIDENCE"
trap - EXIT
```

Let the script exit. Keep failed launch logs and report the failed prerequisite.
If an external hard kill prevents the trap from running, remove only the
directory recorded in that run's `state.path` after its owned processes stop.

## Helpers

No helper scripts ship with this skill. Use the repository's Cargo integration
test runner and the literal shell recipes above. For the repository's full
quality gate, run the four commands in [README.md](../../../README.md#verify-changes).
Invoke `/maintain-verification-skill` from the installed pstack skills when
public entry points, fixtures, or behaviors change.
