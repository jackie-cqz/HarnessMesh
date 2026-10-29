"""Scoped Orion controller for the user-authorized Pi/DSH Todo example."""
import base64
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent
NODE = Path.home() / ".nvm/versions/node/v24.20.0/bin/node"
FILES = ["server.mjs", "server.test.mjs", "package.json", "README.md", "api-contract.json"]


def read_config():
    return json.loads((ROOT / "runtime.json").read_text())


def cli(arguments):
    config = read_config()
    env = os.environ.copy()
    env.update({"HTTP_PROXY": "http://127.0.0.1:7892", "HTTPS_PROXY": "http://127.0.0.1:7892"})
    result = subprocess.run([config["hmesh"], "--json", "--data-dir", str(ROOT / "node")]+arguments,
                            env=env, capture_output=True, text=True, timeout=35)
    if result.returncode:
        raise RuntimeError(result.stderr[-3000:])
    return json.loads(result.stdout) if result.stdout.strip() else None


def start_component(name, command, cwd, env):
    record = ROOT / (name + ".process.json")
    if record.exists():
        old = json.loads(record.read_text())
        cmd = Path("/proc") / str(old["pid"]) / "cmdline"
        if cmd.exists() and cmd.read_bytes():
            raise RuntimeError("owned component is already running: " + name)
    with (ROOT / (name + ".log")).open("ab") as log:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                   stdout=log, stderr=log, start_new_session=True)
    time.sleep(0.25)
    if process.poll() is not None:
        raise RuntimeError("component exited at startup: " + name)
    state = {"pid": process.pid, "command": command, "start_ticks": (Path("/proc")/str(process.pid)/"stat").read_text().split()[21]}
    record.write_text(json.dumps(state))
    return state


def stop_component(name):
    record = ROOT / (name + ".process.json")
    if not record.exists():
        return {"stopped": True}
    state = json.loads(record.read_text())
    proc = Path("/proc") / str(state["pid"])
    if not proc.exists() or not (proc / "cmdline").read_bytes():
        return {"stopped": True}
    assert (proc / "stat").read_text().split()[21] == state["start_ticks"], "PID was reused"
    command = (proc / "cmdline").read_bytes()
    assert str(ROOT).encode() in command and os.getpgid(state["pid"]) == state["pid"], "not this trial's process"
    if name == "daemon":
        try:
            cli(["daemon", "stop"])
        except Exception:
            pass
    else:
        os.killpg(state["pid"], signal.SIGTERM)
    for _ in range(100):
        if not proc.exists() or not (proc / "cmdline").read_bytes():
            return {"stopped": True}
        time.sleep(0.05)
    os.killpg(state["pid"], signal.SIGKILL)
    return {"stopped": True, "forced": True}


def main(action, body):
    if action == "init":
        assert not (ROOT/"runtime.json").exists()
        (ROOT/"backend").mkdir(mode=0o700)
        (ROOT/"data").mkdir(mode=0o700)
        (ROOT/"backend/api-contract.json").write_text(body["contract"])
        build = Path(body["build_root"])
        binary = build / "build-target/debug/hmesh"
        assert binary.is_file() and NODE.is_file()
        (ROOT/"runtime.json").write_text(json.dumps({"hmesh":str(binary)}))
        card=cli(["init","--name","orion-todo","--port","38881"])
        entry=NODE.parent.parent/"lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
        rows=['[agent]','name="orion-todo-pi"','harness="pi"','mode="managed"','project_id="todo-backend"',
              '[project]','root='+json.dumps(str(ROOT/'backend')),'allowed_tools=["read","write","edit"]',
              'artifact_paths='+json.dumps(FILES),'require_remote_task_approval=true','[adapter]',
              'executable='+json.dumps(str(entry)),'node_executable='+json.dumps(str(NODE)),
              'provider="deepseek"','model="deepseek-v4-flash"',
              'models_file='+json.dumps(str(Path.home()/'.pi/agent/models.json')),
              'credential_source='+json.dumps(str(Path.home()/'.pi/agent/auth.json')),
              'env_allowlist=["HTTP_PROXY","HTTPS_PROXY"]']
        (ROOT/'agent.toml').write_text('\n'.join(rows)+'\n')
        return {"card":card,"root":str(ROOT)}
    if action == "daemon-start":
        env=os.environ.copy();env.update({"HTTP_PROXY":"http://127.0.0.1:7892","HTTPS_PROXY":"http://127.0.0.1:7892"})
        state=start_component("daemon",[read_config()["hmesh"],"--data-dir",str(ROOT/'node'),"daemon","start"],ROOT,env)
        for _ in range(100):
            if (ROOT/'node/daemon.json').is_file():return {"process":state,"status":cli(["daemon","status"])}
            time.sleep(0.05)
        raise RuntimeError("Daemon startup deadline exceeded")
    if action == "cli":return cli(body["arguments"])
    if action == "grant":
        rows=['[default]','action="deny"','[[grant]]','peer='+json.dumps(body['peer']),
              'agent='+json.dumps(body['agent']),'project="todo-backend"',
              'actions=["agent.read","task.submit"]','require_approval=true']
        (ROOT/'grant.toml').write_text('\n'.join(rows)+'\n')
        return cli(['policy','--file',str(ROOT/'grant.toml')])
    if action == "export":
        result={}
        for name in FILES:
            path=ROOT/'backend'/name
            assert path.is_file() and str(path.resolve()).startswith(str((ROOT/'backend').resolve())+'/')
            data=path.read_bytes();assert len(data)<200000
            result[name]={"text":data.decode(),"sha256":hashlib.sha256(data).hexdigest()}
        return result
    if action == "test":
        env={"PATH":str(NODE.parent)+":/usr/bin:/bin","HOME":str(Path.home()),"LANG":"C.UTF-8"}
        process=subprocess.Popen([str(NODE),'--test','server.test.mjs'],cwd=ROOT/'backend',env=env,
                                 stdout=subprocess.PIPE,stderr=subprocess.STDOUT,start_new_session=True)
        try:output,_=process.communicate(timeout=60)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid,signal.SIGKILL);output,_=process.communicate()
            raise RuntimeError('backend tests exceeded deadline: '+output.decode(errors='replace')[-1500:])
        return {"exit":process.returncode,"output":output.decode(errors='replace')[-7000:]}
    if action == "backend-start":
        env={"PATH":str(NODE.parent)+":/usr/bin:/bin","HOME":str(Path.home()),"LANG":"C.UTF-8",
             "PORT":"8000","DATA_FILE":str(ROOT/'data/todos.json'),"NODE_LABEL":"Orion"}
        return start_component('backend',[str(NODE),str(ROOT/'backend/server.mjs')],ROOT/'backend',env)
    if action == "stop":
        name=body['component'];assert name in ['daemon','backend']
        return stop_component(name)
    if action == "data":
        path=ROOT/'data/todos.json';data=path.read_bytes();assert len(data)<1048576
        return {"json":json.loads(data),"sha256":hashlib.sha256(data).hexdigest()}
    raise RuntimeError('unsupported action')


if __name__ == '__main__':
    try:
        payload=json.loads(base64.b64decode(sys.argv[2])) if len(sys.argv)>2 else {}
        print(json.dumps(main(sys.argv[1],payload),ensure_ascii=False))
    except Exception as error:
        print(json.dumps({"error":str(error)}),file=sys.stderr)
        sys.exit(1)
