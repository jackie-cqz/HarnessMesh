"""Orion-side smoke control. No credentials or general file contents are returned."""
import base64
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def main():
    root = Path(__file__).resolve().parent
    body = json.loads(base64.b64decode(sys.argv[2])) if len(sys.argv) > 2 else {}
    action = sys.argv[1]
    node_dir = root / "node-orion"
    binary = root / "build-target/debug/hmesh"
    project = root / "project"
    auth = Path.home() / ".pi/agent/auth.json"
    env = os.environ.copy()
    env.update({"HTTP_PROXY": "http://127.0.0.1:7892", "HTTPS_PROXY": "http://127.0.0.1:7892"})

    def cli(arguments):
        result = subprocess.run([str(binary), "--json", "--data-dir", str(node_dir)] + arguments,
                                env=env, capture_output=True, text=True, timeout=35)
        if result.returncode:
            raise RuntimeError("hmesh CLI failed: " + result.stderr[-2000:])
        return json.loads(result.stdout) if result.stdout.strip() else None

    if action == "init":
        assert not node_dir.exists(), "trial instance already exists; refusing to reuse its identity"
        card = cli(["init", "--name", "orion-ssh-smoke", "--port", str(body["port"])])
        project.mkdir(mode=0o700)
        node = Path.home() / ".nvm/versions/node/v24.20.0/bin/node"
        entry = node.parent.parent / "lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
        assert node.is_file() and entry.is_file() and auth.is_file()
        model = body["model"]
        # All secret references remain on Orion. No auth file is copied into a run.
        rows = ["[agent]", 'name = "orion-pi"', 'harness = "pi"', 'mode = "managed"',
                'project_id = "ssh-smoke"', "[project]", "root = " + json.dumps(str(project)),
                'allowed_tools = ["read", "write", "edit"]', 'artifact_paths = ["out/health.json"]',
                "require_remote_task_approval = true", "[adapter]",
                "executable = " + json.dumps(str(entry)), "node_executable = " + json.dumps(str(node)),
                'provider = "deepseek"', "model = " + json.dumps(model),
                "models_file = " + json.dumps(str(Path.home() / ".pi/agent/models.json")),
                "credential_source = " + json.dumps(str(auth)), 'env_allowlist = ["HTTP_PROXY", "HTTPS_PROXY"]']
        (root / "agent.toml").write_text("\n".join(rows) + "\n")
        (root / "auth-before.sha256").write_text(hashlib.sha256(auth.read_bytes()).hexdigest())
        return {"card": card, "project": str(project), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "cargo_lock_sha256": hashlib.sha256((root / "source/Cargo.lock").read_bytes()).hexdigest(),
                "source_archive_sha256": hashlib.sha256((root / "source.zip").read_bytes()).hexdigest(),
                "os": os.uname().sysname, "arch": os.uname().machine}
    if action == "start":
        assert not (root / "daemon.pid").exists(), "trial daemon already started"
        with (node_dir / "daemon.log").open("wb") as log:
            process = subprocess.Popen([str(binary), "--data-dir", str(node_dir), "daemon", "start"],
                                       env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        (root / "daemon.pid").write_text(str(process.pid))
        for _ in range(100):
            if process.poll() is not None:
                raise RuntimeError("trial Daemon exited during startup; inspect its private log")
            if (node_dir / "daemon.json").is_file():
                return {"pid": process.pid, "status": cli(["daemon", "status"])}
            time.sleep(0.05)
        raise RuntimeError("trial Daemon startup deadline exceeded")
    if action == "cli":
        return cli(body["arguments"])
    if action == "artifact":
        path = project / "out/health.json"
        assert str(path.resolve()).startswith(str(project.resolve()) + "/")
        data = path.read_bytes()
        assert len(data) < 65536
        return {"json": json.loads(data), "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
    if action == "stop":
        pid = int((root / "daemon.pid").read_text())
        command = Path("/proc") / str(pid) / "cmdline"
        owned = command.exists() and str(node_dir).encode() in command.read_bytes() and str(binary).encode() in command.read_bytes()
        forced = False
        if owned:
            try:
                cli(["daemon", "stop"])
            except (RuntimeError, subprocess.TimeoutExpired):
                pass
            for _ in range(100):
                if not command.exists() or not command.read_bytes():
                    break
                time.sleep(0.05)
            if command.exists() and str(node_dir).encode() in command.read_bytes() and os.getpgid(pid) == pid:
                os.killpg(pid, signal.SIGTERM)
                forced = True
        stopped = not command.exists() or not command.read_bytes()
        unchanged = hashlib.sha256(auth.read_bytes()).hexdigest() == (root / "auth-before.sha256").read_text()
        return {"daemon_stopped": stopped, "forced": forced, "personal_auth_unchanged": unchanged}
    raise RuntimeError("unknown smoke action")


if __name__ == "__main__":
    try:
        print(json.dumps(main(), ensure_ascii=False))
    except Exception as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        sys.exit(1)
