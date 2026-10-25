#!/usr/bin/env python3
"""Check the latest commits of an administrator's trusted branch registry."""
import argparse
from datetime import datetime, timezone
from http.client import HTTPException
import json
import os
import signal
from pathlib import Path
import re
import shutil
import ssl
import subprocess
import sys
import tarfile
import tempfile
import time
from urllib.parse import quote, urlsplit
from urllib.request import ProxyHandler, HTTPSHandler, Request, build_opener


CHECK_TIMEOUT = 900
NETWORK_TIMEOUT = 120


def timestamp():
    return datetime.now(timezone.utc).isoformat()


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def repository(source):
    url = urlsplit(source)
    match = re.fullmatch(r"/([A-Za-z0-9-]+)/([A-Za-z0-9_.-]+)", url.path)
    if (url.scheme != "https" or url.netloc != "github.com" or
            url.query or url.fragment or not match):
        raise ValueError("COLA_CI_SOURCE must be https://github.com/OWNER/REPO[.git]")
    owner, repo = match.groups()
    repo = repo.removesuffix(".git")
    if repo in ("", ".", ".."):
        raise ValueError("COLA_CI_SOURCE has an invalid repository name")
    return f"{owner}/{repo}"


def check_branch(branch, repo, data, state, env, force, api_base, archive_base):
    directory = data / "results" / branch.encode().hex()
    directory.mkdir(parents=True, exist_ok=True)
    run = str(time.time_ns())
    started_at = timestamp()
    log_path = directory / f"{run}.log"
    opener = build_opener(ProxyHandler({}), HTTPSHandler(context=ssl.create_default_context()))
    try:
        request = Request(f"{api_base}/repos/{repo}/git/ref/heads/{quote(branch, safe='')}",
                          headers={"Accept": "application/vnd.github+json", "User-Agent": "cola-ci"})
        with opener.open(request, timeout=NETWORK_TIMEOUT) as response:
            ref = json.load(response)
        commit = ref.get("object") if isinstance(ref, dict) else None
        if (not isinstance(commit, dict) or commit.get("type") != "commit" or
                not isinstance(commit.get("sha"), str) or
                not re.fullmatch(r"[0-9a-f]{40}", commit["sha"]) or
                ref.get("ref") != f"refs/heads/{branch}"):
            raise ValueError("GitHub ref response does not identify the requested commit")
        sha = commit["sha"]
        if not force and state.get(branch, {}).get("sha") == sha:
            print(f"{branch}: unchanged {sha}", flush=True)
            return
        print(f"{branch}: checking {sha}", flush=True)
        with log_path.open("w") as log:
            log.write(f"{branch} {sha}\n")
            log.flush()
            with tempfile.TemporaryDirectory(dir=data / "workspaces") as workspace:
                archive = Path(workspace) / "source.tar.gz"
                with opener.open(f"{archive_base}/{repo}/tar.gz/{sha}",
                                 timeout=NETWORK_TIMEOUT) as response, archive.open("wb") as output:
                    shutil.copyfileobj(response, output)
                extracted = Path(workspace) / "source"
                extracted.mkdir()
                with tarfile.open(archive, "r:gz") as source:
                    source.extractall(extracted, filter="data")
                roots = list(extracted.iterdir())
                if len(roots) != 1 or not roots[0].is_dir() or roots[0].is_symlink():
                    raise ValueError("source archive must contain exactly one root directory")
                checkout = roots[0]
                script = checkout / "ci/check.sh"
                exit_code = None
                status = "unsupported"
                if script.is_file():
                    process = subprocess.Popen(
                        ["/bin/sh", "./ci/check.sh"], cwd=checkout, env=env,
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
    except (subprocess.SubprocessError, OSError, ValueError, tarfile.TarError,
            EOFError, HTTPException) as error:
        with log_path.open("a") as log:
            log.write(f"error: {error}\n")
        print(f"{branch}: error: {error}; see {log_path}", file=sys.stderr, flush=True)
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--force", action="store_true", help="retry completed commits")
    args = parser.parse_args()
    repo = repository(os.environ.get("COLA_CI_SOURCE", "https://github.com/ShaoxinLi/COLA.git"))
    api_base = os.environ.get("COLA_CI_API_BASE", "https://api.github.com").rstrip("/")
    archive_base = os.environ.get("COLA_CI_ARCHIVE_BASE", "https://codeload.github.com").rstrip("/")
    branches = os.environ.get("COLA_CI_BRANCHES", "main feature/domain-layer codex/server-ci").split()
    data = Path(os.environ.get("COLA_CI_DATA", "/var/lib/cola-ci"))
    workspaces = data / "workspaces"
    workspaces.mkdir(parents=True, exist_ok=True)
    for workspace in workspaces.iterdir():
        if workspace.is_dir() and not workspace.is_symlink():
            shutil.rmtree(workspace)
        else:
            workspace.unlink()
    state_file = data / "state.json"
    state = json.loads(state_file.read_text()) if state_file.exists() else {}
    env = {key: os.environ[key] for key in ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME")
           if key in os.environ}
    failed = False
    for branch in dict.fromkeys(branches):
        if check_branch(branch, repo, data, state, env, args.force, api_base, archive_base) is False:
            failed = True
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
