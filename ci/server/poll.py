#!/usr/bin/env python3
"""Check the latest commits of an administrator's trusted branch registry."""
import argparse
from datetime import datetime, timezone
import json
import os
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time


CHECK_TIMEOUT = 900


def timestamp():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def check_branch(branch, source, data, state, env, force):
    directory = data / "results" / branch.encode().hex()
    directory.mkdir(parents=True, exist_ok=True)
    run = str(time.time_ns())
    started_at = timestamp()
    with (directory / f"{run}.log").open("w") as log:
        def git(*args, cwd=None):
            return subprocess.run(
                ["git", "-c", "credential.helper=", "-c", "core.hooksPath=/dev/null", *args],
                cwd=cwd, env=env, check=True, stdout=subprocess.PIPE,
                stderr=log, text=True, timeout=120,
            ).stdout.strip()

        try:
            git("check-ref-format", f"refs/heads/{branch}")
            with tempfile.TemporaryDirectory(dir=data / "workspaces") as workspace:
                git("init", "--quiet", workspace)
                git("fetch", "--quiet", "--depth=1", "--no-tags", source,
                    f"refs/heads/{branch}", cwd=workspace)
                sha = git("rev-parse", "FETCH_HEAD^{commit}", cwd=workspace)
                if not force and state.get(branch, {}).get("sha") == sha:
                    print(f"{branch}: unchanged {sha}", flush=True)
                    return
                print(f"{branch}: checking {sha}", flush=True)
                log.write(f"{branch} {sha}\n")
                log.flush()
                git("checkout", "--quiet", "--detach", sha, cwd=workspace)
                script = Path(workspace) / "ci/check.sh"
                exit_code = None
                status = "unsupported"
                if script.is_file():
                    process = subprocess.Popen(
                        ["/bin/sh", "./ci/check.sh"], cwd=workspace, env=env,
                        stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                    )
                    try:
                        exit_code = process.wait(timeout=CHECK_TIMEOUT)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
                        raise
                    if exit_code < 0:
                        raise subprocess.CalledProcessError(exit_code, "ci/check.sh")
                    status = "success" if exit_code == 0 else "failure"
                else:
                    log.write("unsupported: ci/check.sh is absent\n")
                record = dict(sha=sha, status=status, exit_code=exit_code,
                              started_at=started_at, finished_at=timestamp())
                write_json(directory / f"{run}.json", record)
                state[branch] = record
                write_json(data / "state.json", state)
                print(f"{branch}: {status} {sha}", flush=True)
                return status != "failure"
        except (subprocess.SubprocessError, OSError) as error:
            log.write(f"error: {error}\n")
            print(f"{branch}: error: {error}; see {directory / (run + '.log')}",
                  file=sys.stderr, flush=True)
            return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--force", action="store_true", help="retry completed commits")
    args = parser.parse_args()
    source = os.environ.get("COLA_CI_SOURCE", "https://github.com/ShaoxinLi/COLA.git")
    branches = os.environ.get("COLA_CI_BRANCHES", "main feature/domain-layer codex/server-ci").split()
    data = Path(os.environ.get("COLA_CI_DATA", "/var/lib/cola-ci"))
    (data / "workspaces").mkdir(parents=True, exist_ok=True)
    state_file = data / "state.json"
    state = json.loads(state_file.read_text()) if state_file.exists() else {}
    env = {key: os.environ[key] for key in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME")
           if key in os.environ}
    env.update(GIT_TERMINAL_PROMPT="0", GIT_CONFIG_NOSYSTEM="1",
               GIT_CONFIG_GLOBAL="/dev/null", GIT_SSH_COMMAND="/bin/false")
    failed = False
    for branch in dict.fromkeys(branches):
        if check_branch(branch, source, data, state, env, args.force) is False:
            failed = True
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
