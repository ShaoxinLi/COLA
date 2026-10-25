#!/usr/bin/env python3
import importlib.util
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
from unittest.mock import patch
from urllib.parse import unquote

POLLER = Path(__file__).with_name("poll.py")


class PollTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.origin = self.root / "origin"
        self.origin.mkdir()
        self.git("init", "--initial-branch=unsupported")
        self.git("config", "user.email", "ci@example.invalid")
        self.git("config", "user.name", "CI fixture")
        (self.origin / "README").write_text("fixture\n")
        self.git("add", ".")
        self.git("commit", "-m", "Unsupported branch")
        self.git("checkout", "-b", "feature/domain-layer")
        (self.origin / "ci").mkdir()
        self.script = self.origin / "ci/check.sh"
        self.script.write_text(
            '#!/bin/sh\n[ ! -e .git ] && [ -z "${GH_TOKEN+x}" ] && '
            '[ -z "${HTTPS_PROXY+x}" ] && [ -z "${COLA_CI_SOURCE+x}" ]\n')
        self.git("add", ".")
        self.git("commit", "-m", "Passing check")
        self.success_sha = self.git("rev-parse", "HEAD")
        self.git("checkout", "-b", "failure")
        self.script.write_text("#!/bin/sh\nexit 7\n")
        self.git("commit", "-am", "Failing check")
        self.responses = {}
        self.requests = []
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                fixture.requests.append((self.path, dict(self.headers)))
                status = 200
                if self.path in fixture.responses:
                    status, payload = fixture.responses[self.path]
                elif self.path.startswith("/repos/ShaoxinLi/COLA/git/ref/heads/"):
                    branch = unquote(self.path.split("/heads/", 1)[1])
                    try:
                        sha = fixture.git("rev-parse", "--verify", f"refs/heads/{branch}")
                        payload = json.dumps(dict(ref=f"refs/heads/{branch}",
                                                  object=dict(type="commit", sha=sha))).encode()
                    except subprocess.CalledProcessError:
                        status, payload = 404, b"missing ref"
                elif self.path.startswith("/ShaoxinLi/COLA/tar.gz/"):
                    sha = self.path.rsplit("/", 1)[1]
                    payload = subprocess.check_output([
                        "git", "-C", str(fixture.origin), "archive", "--format=tar.gz",
                        f"--prefix=COLA-{sha}/", sha])
                else:
                    status, payload = 404, b"unknown path"
                self.send_response(status)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.base = f"http://127.0.0.1:{self.server.server_port}"
        self.data = self.root / "state"
        self.env = dict(os.environ, COLA_CI_SOURCE="https://github.com/ShaoxinLi/COLA.git",
                        COLA_CI_API_BASE=self.base, COLA_CI_ARCHIVE_BASE=self.base,
                        COLA_CI_DATA=str(self.data),
                        COLA_CI_BRANCHES="feature/domain-layer failure unsupported",
                        GH_TOKEN="fixture-secret", HTTPS_PROXY="http://127.0.0.1:1",
                        HTTP_PROXY="http://127.0.0.1:1", ALL_PROXY="http://127.0.0.1:1",
                        NO_PROXY="")
        spec = importlib.util.spec_from_file_location("poll", POLLER)
        self.controller = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.controller)

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.origin), *args], text=True,
                                       stderr=subprocess.DEVNULL).strip()

    def poll(self, *args):
        return subprocess.run([sys.executable, str(POLLER), *args], env=self.env,
                              capture_output=True, text=True, timeout=20)

    def test_completed_results_and_transport_retry(self):
        self.assertEqual(self.poll().returncode, 1)
        state_file = self.data / "state.json"
        initial = state_file.read_bytes()
        state = json.loads(initial)
        self.assertEqual(state["feature/domain-layer"]["sha"], self.success_sha)
        self.assertEqual(state["feature/domain-layer"]["status"], "success")
        self.assertEqual(state["failure"]["status"], "failure")
        self.assertEqual(state["failure"]["exit_code"], 7)
        self.assertEqual(state["unsupported"]["status"], "unsupported")
        self.assertIsNone(state["unsupported"]["exit_code"])
        logs = list((self.data / "results").glob("*/*.log"))
        request_count = len(self.requests)
        orphan = self.data / "workspaces" / "orphan"
        orphan.mkdir()
        (orphan / "partial-archive").write_bytes(b"incomplete")
        readonly = orphan / "readonly"
        readonly.mkdir()
        (readonly / "artifact").write_bytes(b"interrupted")
        readonly.chmod(0o555)
        self.assertEqual(self.poll().returncode, 0)
        self.assertFalse(orphan.exists())
        self.assertEqual(state_file.read_bytes(), initial)
        self.assertEqual(list((self.data / "results").glob("*/*.log")), logs)
        self.assertEqual(len(self.requests) - request_count, 3)
        self.assertEqual(self.poll("--force").returncode, 1)
        self.assertNotEqual(state_file.read_bytes(), initial)
        self.assertEqual(len(list((self.data / "results").glob("*/*.json"))), 6)
        completed = state_file.read_bytes()
        self.env["COLA_CI_API_BASE"] = self.base + "/unavailable"
        self.assertEqual(self.poll().returncode, 1)
        self.assertEqual(state_file.read_bytes(), completed)
        self.env["COLA_CI_API_BASE"] = "http://127.0.0.1:1"
        self.assertEqual(self.poll().returncode, 1)
        self.assertEqual(state_file.read_bytes(), completed)
        self.env["COLA_CI_API_BASE"] = self.base
        self.env["COLA_CI_BRANCHES"] = "absent"
        self.assertEqual(self.poll().returncode, 1)
        self.assertEqual(state_file.read_bytes(), completed)
        self.git("branch", "absent", "feature/domain-layer")
        archive_path = f"/ShaoxinLi/COLA/tar.gz/{self.success_sha}"
        self.responses[archive_path] = 503, b"temporarily unavailable"
        self.assertEqual(self.poll().returncode, 1)
        self.assertEqual(state_file.read_bytes(), completed)
        del self.responses[archive_path]
        self.assertEqual(self.poll().returncode, 0)
        self.assertEqual(json.loads(state_file.read_text())["absent"]["status"], "success")
        self.assertTrue(any("feature%2Fdomain-layer" in path for path, _ in self.requests))
        for _, headers in self.requests:
            self.assertNotIn("Authorization", headers)
            self.assertNotIn("Proxy-Authorization", headers)

    def test_timeout_retries_same_sha(self):
        self.git("checkout", "-b", "timeout", "feature/domain-layer")
        self.script.write_text("#!/bin/sh\nsleep 0.2\n")
        self.git("commit", "-am", "Interrupted check")
        sha = self.git("rev-parse", "HEAD")
        self.env["COLA_CI_BRANCHES"] = "unsupported"
        self.assertEqual(self.poll().returncode, 0)
        state_file = self.data / "state.json"
        completed = state_file.read_bytes()
        with patch.object(self.controller, "CHECK_TIMEOUT", 0.05):
            self.assertIs(self.controller.check_branch(
                "timeout", "ShaoxinLi/COLA", self.data, json.loads(completed),
                {"PATH": os.environ["PATH"]}, False, self.base, self.base), False)
        self.assertEqual(state_file.read_bytes(), completed)
        self.env["COLA_CI_BRANCHES"] = "timeout"
        self.assertEqual(self.poll().returncode, 0)
        self.assertEqual(json.loads(state_file.read_text())["timeout"]["sha"], sha)
        self.assertEqual(list((self.data / "workspaces").iterdir()), [])

    def test_rejects_invalid_remote_data(self):
        self.env["COLA_CI_BRANCHES"] = "feature/domain-layer"
        ref_path = "/repos/ShaoxinLi/COLA/git/ref/heads/feature%2Fdomain-layer"
        for value in ([], {"object": {"type": "tag", "sha": self.success_sha}},
                      {"object": {"type": "commit", "sha": "not-a-sha"}},
                      {"ref": "refs/heads/other", "object": {
                          "type": "commit", "sha": self.success_sha}}):
            with self.subTest(value=value):
                self.responses[ref_path] = 200, json.dumps(value).encode()
                self.assertEqual(self.poll().returncode, 1)
                self.assertFalse((self.data / "state.json").exists())
        del self.responses[ref_path]
        for names in (("../escape",), ("first/check.sh", "second/check.sh")):
            archive = io.BytesIO()
            with tarfile.open(fileobj=archive, mode="w:gz") as output:
                for name in names:
                    output.addfile(tarfile.TarInfo(name))
            self.responses[f"/ShaoxinLi/COLA/tar.gz/{self.success_sha}"] = 200, archive.getvalue()
            self.assertEqual(self.poll().returncode, 1)
            self.assertFalse((self.data / "state.json").exists())
        self.assertFalse((self.data / "workspaces" / "escape").exists())

    def test_rejects_non_github_sources(self):
        for source in ("/tmp/repo", "http://github.com/a/b", "https://evil.test/a/b",
                       "https://user:secret@github.com/a/b", "https://github.com/a/..",
                       "https://github.com/a/b?token=secret", "https://github.com/a/b#ref"):
            with self.subTest(source=source), self.assertRaises(ValueError):
                self.controller.repository(source)
        self.assertEqual(self.controller.repository("https://github.com/a/b.git"), "a/b")


if __name__ == "__main__":
    unittest.main()
