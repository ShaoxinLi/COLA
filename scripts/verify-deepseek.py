#!/usr/bin/env python3
"""Exercise COLA's real CLI and HTTP APIs against configured DeepSeek models."""

import argparse
import json
import pathlib
import queue
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--config", required=True, type=pathlib.Path)
    parser.add_argument("--evidence", required=True, type=pathlib.Path)
    parser.add_argument("--expect-restart-failure", action="store_true")
    args = parser.parse_args()
    binary = args.binary.resolve()
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=False)
    config = json.loads(args.config.read_text())
    if urllib.parse.urlsplit(config["base_url"]).hostname != "api.deepseek.com":
        raise ValueError("verification requires the real api.deepseek.com endpoint")
    if config["protocol"] != "openai-responses":
        raise ValueError("verification requires DeepSeek's Responses protocol")
    models = {role: config[role]["model"] for role in ("assistant", "reviewer")}
    if not all(model.startswith("deepseek-") for model in models.values()):
        raise ValueError("both roles must use real DeepSeek models")
    key = config["api_key"]
    if not key:
        raise ValueError("the local configuration has no API key")
    events = []

    def record(event, **fields):
        item = dict(event=event, **fields)
        encoded = json.dumps(item, ensure_ascii=False)
        if key in encoded:
            raise RuntimeError("credential found in verification output")
        events.append(item)
        with (evidence / "events.jsonl").open("a") as file:
            file.write(encoded + "\n")
        print(encoded, flush=True)

    def cli(config_path, text, label):
        result = subprocess.run([str(binary), "cli", "--config", str(config_path)],
                                input=text, text=True, capture_output=True, timeout=240)
        replies = [json.loads(line) for line in result.stdout.splitlines() if line]
        if key in result.stdout or key in result.stderr:
            raise RuntimeError("credential found in CLI diagnostics")
        (evidence / (label + ".json")).write_text(json.dumps(replies, indent=2))
        record(label, exit=result.returncode, kinds=[item.get("kind") for item in replies])
        return result.returncode, replies

    def answer(reply):
        return "\n".join(message.get("text", "") for message in reply.get("messages", [])
                         if message.get("kind") == "assistant_answer")

    def tool_calls(reply):
        return [call for message in reply.get("messages", [])
                if message.get("kind") == "assistant_tool_requests" for call in message["calls"]]

    def decoded_values(value):
        yield value
        if isinstance(value, dict):
            for child in value.values():
                yield from decoded_values(child)
        elif isinstance(value, list):
            if value and all(isinstance(item, int) and not isinstance(item, bool) and 0 <= item <= 255 for item in value):
                try:
                    decoded = bytes(value).decode("utf-8")
                    yield decoded
                    yield from decoded_values(json.loads(decoded))
                except (ValueError, UnicodeError):
                    pass
            else:
                for child in value:
                    yield from decoded_values(child)
        elif isinstance(value, str) and value.lstrip().startswith(("{", "[")):
            try:
                yield from decoded_values(json.loads(value))
            except ValueError:
                pass

    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(url, body=None):
        req = urllib.request.Request(url, data=None if body is None else json.dumps(body).encode(),
                                     headers={"Content-Type": "application/json"})
        try:
            with opener.open(req, timeout=240) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def start_web(config_path):
        process = subprocess.Popen([str(binary), "web", "--config", str(config_path), "--bind", "127.0.0.1:0"],
                                   text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        lines = queue.Queue()
        def read_startup():
            for line in process.stderr:
                lines.put(line)
        threading.Thread(target=read_startup, daemon=True).start()
        try:
            startup = lines.get(timeout=15).strip()
            if not startup.startswith("Listening on http://127.0.0.1:"):
                raise RuntimeError("Web listener did not start")
            return process, startup.removeprefix("Listening on ")
        except BaseException:
            process.kill()
            process.wait(timeout=5)
            raise

    def stop_web(process):
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()

    started = time.time()
    with tempfile.TemporaryDirectory(prefix="cola-deepseek-live-") as directory:
        root = pathlib.Path(directory)
        tools = root / "tools"
        store = root / "sessions"
        tools.mkdir()
        store.mkdir()
        config.update(tool_cwd=str(tools), store_root=str(store), timeout_ms=120000, max_iterations=12)
        for role in models:
            config[role]["max_output_tokens"] = 8192
        config_path = root / "config.json"
        config_path.touch(mode=0o600)
        config_path.write_text(json.dumps(config))
        token = "deepseek-live-" + uuid.uuid4().hex
        port = str(20000 + int(uuid.uuid4().hex[:4], 16) % 30000)
        seed = tools / "live-seed.txt"
        seed.write_text(f"TOKEN={token}\nPORT={port}\n")
        prompt = "Use read_file to read live-seed.txt. Report its exact TOKEN and PORT. Do not write or delete files."
        code, replies = cli(config_path, "/new\n" + prompt + "\n/exit\n", "cli-initial")
        if code != 0 or len(replies) != 2 or replies[1].get("kind") != "chatted":
            raise AssertionError("real DeepSeek initial CLI chat failed; inspect the saved reply")
        session_id = replies[0]["session"]["id"]
        if not any(call["name"] == "read_file" for call in tool_calls(replies[1])):
            raise AssertionError("initial chat did not use the real read_file tool")
        if token not in answer(replies[1]) or port not in answer(replies[1]):
            raise AssertionError("initial chat did not report the actual file contents")
        seed.unlink()
        followup = "What exact TOKEN was previously read from live-seed.txt? Use recorded history only, without tools. Reply with the token."
        code, replies = cli(config_path, f"/use {session_id}\n{followup}\n/exit\n", "cli-restarted")
        if args.expect_restart_failure:
            if code != 1 or replies[1].get("error", {}).get("code") != "model_failure":
                raise AssertionError("baseline did not reproduce model_failure")
            record("baseline-cli", expected_error="model_failure", actual_error=replies[1]["error"]["code"])
        else:
            if code != 0 or replies[1].get("kind") != "chatted" or token not in answer(replies[1]):
                raise AssertionError("cold CLI did not recover the file fact from disk")
            if tool_calls(replies[1]):
                raise AssertionError("cold CLI unexpectedly used tools instead of recorded history")
            record("cold-cli-history", exact_token=True, seed_absent=not seed.exists(), tool_calls=0)
        process, address = start_web(config_path)
        try:
            status, reply = request(address + f"/sessions/{session_id}/messages", dict(text="What exact PORT was previously read? Use recorded history only, without tools. Reply with the port."))
            (evidence / "web-after-cli.json").write_text(json.dumps(reply, indent=2))
            if args.expect_restart_failure:
                if status != 502 or reply.get("error", {}).get("code") != "model_failure":
                    raise AssertionError("baseline Web did not reproduce model_failure")
                record("baseline-web", status=status, error=reply["error"]["code"])
            else:
                if status != 200 or port not in answer(reply) or tool_calls(reply):
                    raise AssertionError("fresh Web process did not recover CLI facts")
                record("cold-web-after-cli", status=status, exact_port=True, tool_calls=0)
                second = "deepseek-http-" + uuid.uuid4().hex
                web_seed = tools / "web-seed.txt"
                web_seed.write_text(second)
                status, created = request(address + "/sessions", {})
                if status != 201:
                    raise AssertionError("Web session creation failed")
                web_id = created["session"]["id"]
                status, reply = request(address + f"/sessions/{web_id}/messages", dict(text="Use read_file to read web-seed.txt and report its exact contents. Do not modify any files."))
                (evidence / "web-initial.json").write_text(json.dumps(reply, indent=2))
                if status != 200 or second not in answer(reply) or not tool_calls(reply):
                    raise AssertionError("real DeepSeek Web tool chat failed")
                web_seed.unlink()
                record("web-initial", status=status, real_file_read=True, exact_token=True)
        finally:
            stop_web(process)
        if not args.expect_restart_failure:
            process, address = start_web(config_path)
            try:
                status, reply = request(address + f"/sessions/{web_id}/messages", dict(text="Repeat the exact contents previously read from web-seed.txt. Use recorded history only, without tools."))
                (evidence / "web-restarted.json").write_text(json.dumps(reply, indent=2))
                if status != 200 or second not in answer(reply) or tool_calls(reply):
                    raise AssertionError("restarted Web did not recover recorded tool facts")
                status, listing = request(address + "/sessions")
                if status != 200:
                    raise AssertionError("Web listing failed after recovery")
                (evidence / "sessions.json").write_text(json.dumps(listing, indent=2))
                record("cold-web-history", status=200, exact_token=True, seed_absent=True, tool_calls=0)
            finally:
                stop_web(process)
        archive_dir = evidence / "archives"
        shutil.copytree(store, archive_dir)
        if not args.expect_restart_failure:
            stored = [json.loads(line) for file in archive_dir.glob("*.jsonl")
                      for line in file.read_text().splitlines() if line]
            requests = [value for value in decoded_values(stored) if isinstance(value, dict)
                        and value.get("model") in models.values() and isinstance(value.get("input"), list)]
            if not requests:
                raise AssertionError("no durable DeepSeek request records were found")
            for body in requests:
                prose = json.dumps(dict(input=body["input"], instructions=body.get("instructions")), ensure_ascii=False)
                if any(model in prose for model in models.values()):
                    raise AssertionError("historical model audit metadata leaked into model context")
            if any("version" in item or "schema_version" in item for item in stored if isinstance(item, dict)):
                raise AssertionError("the archive contains a format-version identifier")
            record("durable-audit", recorded_requests=len(requests), historical_model_metadata_in_prose=False,
                   format_version_identifier=False)
        for file in evidence.rglob("*"):
            if not file.is_file():
                continue
            if key.encode() in file.read_bytes():
                raise RuntimeError("credential found in evidence")
            if file.suffix in (".json", ".jsonl"):
                documents = ([json.loads(line) for line in file.read_text().splitlines() if line]
                             if file.suffix == ".jsonl" else [json.loads(file.read_text())])
                if any(isinstance(value, str) and key in value for value in decoded_values(documents)):
                    raise RuntimeError("decoded credential found in evidence")
        record("complete", target="api.deepseek.com", models=models,
               expected_baseline_failure=args.expect_restart_failure,
               elapsed_seconds=round(time.time() - started, 1), archives=len(list(archive_dir.glob("*.jsonl"))))


if __name__ == "__main__":
    main()
