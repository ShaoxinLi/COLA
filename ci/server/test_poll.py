#!/usr/bin/env python3
"""Exercise the controller against an isolated local Git origin."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

POLLER = Path(__file__).with_name("poll.py")


class PollTest(unittest.TestCase):
    def test_completed_results_and_fetch_retry(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            origin = root / "origin"
            origin.mkdir()

            def git(*args):
                return subprocess.check_output(
                    ["git", "-C", str(origin), *args], text=True,
                    stderr=subprocess.DEVNULL,
                ).strip()

            git("init", "--initial-branch=unsupported")
            git("config", "user.email", "ci@example.invalid")
            git("config", "user.name", "CI fixture")
            (origin / "README").write_text("fixture\n")
            git("add", ".")
            git("commit", "-m", "Unsupported branch")
            git("checkout", "-b", "success")
            (origin / "ci").mkdir()
            script = origin / "ci/check.sh"
            script.write_text("#!/bin/sh\nexit 0\n")
            git("add", ".")
            git("commit", "-m", "Passing check")
            success_sha = git("rev-parse", "HEAD")
            git("checkout", "-b", "failure")
            script.write_text("#!/bin/sh\nexit 7\n")
            git("commit", "-am", "Failing check")
            data = root / "state"
            env = dict(os.environ, COLA_CI_SOURCE=str(origin), COLA_CI_DATA=str(data),
                       COLA_CI_BRANCHES="success failure unsupported")

            def poll(*args):
                return subprocess.run([sys.executable, str(POLLER), *args], env=env,
                                      capture_output=True, text=True, timeout=20)

            result = poll()
            self.assertEqual(result.returncode, 1, result.stderr)
            state_file = data / "state.json"
            initial = state_file.read_bytes()
            state = json.loads(initial)
            self.assertEqual(state["success"]["sha"], success_sha)
            self.assertEqual(state["success"]["status"], "success")
            self.assertEqual(state["failure"]["status"], "failure")
            self.assertEqual(state["failure"]["exit_code"], 7)
            self.assertEqual(state["unsupported"]["status"], "unsupported")
            self.assertIsNone(state["unsupported"]["exit_code"])
            self.assertEqual(poll().returncode, 0)
            self.assertEqual(state_file.read_bytes(), initial)
            self.assertEqual(poll("--force").returncode, 1)
            self.assertNotEqual(state_file.read_bytes(), initial)
            self.assertEqual(len(list((data / "results").glob("*/*.json"))), 6)
            completed = state_file.read_bytes()
            env["COLA_CI_SOURCE"] = str(root / "missing-origin")
            self.assertEqual(poll().returncode, 1)
            self.assertEqual(state_file.read_bytes(), completed)
            env["COLA_CI_SOURCE"] = str(origin)
            env["COLA_CI_BRANCHES"] = "absent"
            self.assertEqual(poll().returncode, 1)
            self.assertEqual(state_file.read_bytes(), completed)
            git("branch", "absent", "success")
            self.assertEqual(poll().returncode, 0)
            self.assertEqual(json.loads(state_file.read_text())["absent"]["status"], "success")
            self.assertTrue(list((data / "results").glob("*/*.log")))
            completed = state_file.read_bytes()
            git("checkout", "-b", "timeout", "success")
            script.write_text("#!/bin/sh\nsleep 0.2\n")
            git("commit", "-am", "Interrupted check")
            spec = importlib.util.spec_from_file_location("poll", POLLER)
            controller = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(controller)
            state = json.loads(completed)
            with patch.object(controller, "CHECK_TIMEOUT", 0.05):
                self.assertIs(controller.check_branch(
                    "timeout", str(origin), data, state, os.environ, False), False)
            self.assertEqual(state_file.read_bytes(), completed)
            env["COLA_CI_BRANCHES"] = "timeout"
            self.assertEqual(poll().returncode, 0)
            self.assertEqual(json.loads(state_file.read_text())["timeout"]["status"], "success")


if __name__ == "__main__":
    unittest.main()
