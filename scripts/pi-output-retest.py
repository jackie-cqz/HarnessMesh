"""Fresh, explicitly requested Pi regression; never replaces the running app."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("todo_control", str(ROOT / "todo-remote.py"))
control = importlib.util.module_from_spec(spec)
spec.loader.exec_module(control)


def main(action, body):
    token = body["token"]
    assert re.fullmatch(r"[a-f0-9]{32}", token)
    trial = ROOT / ("output-retest-" + token)
    if action == "prepare":
        trial.mkdir(mode=0o700)
        backend = trial / "backend"
        backend.mkdir(mode=0o700)
        (backend / "api-contract.json").write_text(body["contract"])
        # Quotes/backslashes make a normal Pi read-tool response exceed 64 KiB
        # after JSON escaping, even when Pi applies its own 50 KiB read limit.
        fixture = ('"\\' * 256 + '\n') * 512
        (backend / "large-tool-input.txt").write_text(fixture)
        rows = (ROOT / "agent.toml").read_text().splitlines()
        project = "todo-output-" + token
        rows = [
            'name="orion-pi-output-regression"' if row.startswith("name=") else
            'project_id=' + json.dumps(project) if row.startswith("project_id=") else
            'root=' + json.dumps(str(backend)) if row.startswith("root=") else row
            for row in rows
        ]
        config = trial / "agent.toml"
        config.write_text('\n'.join(rows) + '\n')
        agent = control.cli(["agent", "register", "--config", str(config)])
        verification = control.cli(["agent", "verify", agent["agent_id"], "--allow-model"])
        policy = (ROOT / "grant.toml").read_text()
        policy += '\n[[grant]]\npeer=' + json.dumps(body["peer"])
        policy += '\nagent=' + json.dumps(agent["agent_id"]) + '\nproject=' + json.dumps(project)
        policy += '\nactions=["agent.read","task.submit"]\nrequire_approval=true\n'
        (trial / "grant.toml").write_text(policy)
        control.cli(["policy", "--file", str(trial / "grant.toml")])
        return {"agent": agent, "verification": verification, "project": project,
                "remote_root": str(trial), "fixture_bytes": len(fixture.encode()),
                "fixture_sha256": hashlib.sha256(fixture.encode()).hexdigest()}
    if action in ["export", "test"]:
        control.ROOT = trial
        return control.main(action, {})
    if action == "restore-policy":
        return control.cli(["policy", "--file", str(ROOT / "grant.toml")])
    raise RuntimeError("unsupported regression action")


if __name__ == "__main__":
    try:
        body = json.loads(base64.b64decode(sys.argv[2]))
        print(json.dumps(main(sys.argv[1], body), ensure_ascii=False))
    except Exception as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        sys.exit(1)
