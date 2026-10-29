"""Opt-in Orion helper for isolated managed-service/repair acceptance.

Only fresh trial directories are used. Existing Todo services/configuration are
read as templates and are never stopped, overwritten or registered as owned.
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import time
import zipfile

ROOT = Path(__file__).resolve().parent
NODE = Path.home() / '.nvm/versions/node/v24.20.0/bin/node'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def settings():
    return json.loads((ROOT / 'trial.json').read_text())


def cli(arguments):
    env = os.environ.copy()
    env.update(HTTP_PROXY='http://127.0.0.1:7892', HTTPS_PROXY='http://127.0.0.1:7892')
    result = subprocess.run([str(ROOT/'target/debug/hmesh'), '--json', '--data-dir', str(ROOT/'node')] + arguments,
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=40)
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors='replace')[-3000:])
    return json.loads(result.stdout) if result.stdout.strip() else None


def build_worker():
    data = settings()
    build = Path(data['build_root'])
    source = ROOT/'source'
    source.mkdir(exist_ok=True)
    with zipfile.ZipFile(ROOT/'source.zip') as archive:
        for info in archive.infolist():
            assert not info.filename.startswith('/') and '..' not in Path(info.filename).parts
        archive.extractall(source)
    (source/'.cargo').mkdir(exist_ok=True)
    (source/'.cargo/config.toml').write_text('[source.crates-io]\nreplace-with="vendored-sources"\n[source.vendored-sources]\ndirectory='+json.dumps(str(build/'vendor'))+'\n')
    env = dict(PATH=str(build/'tools/rust/bin')+':/usr/bin:/bin', HOME=str(Path.home()), LANG='C.UTF-8',
               CARGO_HOME=str(build/'cargo-home'), CARGO_TARGET_DIR=str(ROOT/'target'), CARGO_BUILD_JOBS='2')
    # The portable Orion toolchain contains rustc/cargo, without rustfmt/clippy.
    # Those checks run on Windows; Linux has independent locked build/tests.
    commands = [ ['test', '--offline', '--locked', '--workspace'], ['build', '--offline', '--locked', '-p', 'hmesh-cli', '-p', 'hmesh-mcp'] ]
    results = []
    for args in commands:
        result = subprocess.run([str(build/'tools/rust/bin/cargo')]+args, cwd=source, env=env, timeout=900,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        output=result.stdout.decode(errors='replace')
        print(output,flush=True)
        passed=sum(int(n) for n in re.findall(r'test result: ok\. (\d+) passed',output))
        results.append(dict(arguments=args, exit=result.returncode, passed=passed))
        if result.returncode:
            break
    (ROOT/'checks.json').write_text(json.dumps(results))


def main(action, body):
    assert re.fullmatch(r'managed-[a-f0-9]{32}', ROOT.name), 'fresh dedicated trial directory required'
    if action == 'build-start':
        assert not (ROOT/'trial.json').exists(), 'trial already initialized'
        (ROOT/'trial.json').write_text(json.dumps(body))
        with (ROOT/'build.log').open('wb') as log:
            process = subprocess.Popen(['python3', str(Path(__file__).resolve()), '_build'], stdout=log, stderr=log, start_new_session=True)
        return dict(pid=process.pid)
    if action == '_build':
        build_worker()
        return dict(done=True)
    if action == 'build-resume':
        assert (ROOT/'checks.json').exists()
        (ROOT/'checks.json').rename(ROOT/('checks-history-'+str(time.time_ns())+'.json'))
        with (ROOT/'build.log').open('ab') as log:
            process = subprocess.Popen(['python3', str(Path(__file__).resolve()), '_build'], stdout=log, stderr=log, start_new_session=True)
        return dict(pid=process.pid)
    if action == 'build-status':
        return dict(checks=json.loads((ROOT/'checks.json').read_text()) if (ROOT/'checks.json').exists() else None,
                    log_tail=(ROOT/'build.log').read_text(errors='replace')[-3500:])
    if action == 'prepare':
        assert not (ROOT/'node').exists()
        template = Path(settings()['template_root'])
        shutil.copytree(ROOT/'source/examples/ssh-todo/server', ROOT/'backend')
        (ROOT/'data').mkdir(mode=0o700)
        source = ROOT/'backend/server.mjs'
        original = source.read_text()
        needle = "{ status: 'ok', service: SERVICE_NAME, node: resolvedNodeLabel }"
        assert original.count(needle) == 1
        source.write_text(original.replace(needle, "{ status: 'ok', service: SERVICE_NAME }"))
        (ROOT/'before.json').write_text(json.dumps(dict(source_sha256=digest(source.read_bytes()), original_source_sha256=digest(original.encode()),
                                                       contract_sha256=digest((ROOT/'backend/api-contract.json').read_bytes()))))
        project = ROOT.name+'-backend'
        rows = (template/'agent.toml').read_text().splitlines()
        rows = ['name="orion-managed-repair"' if row.startswith('name=') else
                'project_id='+json.dumps(project) if row.startswith('project_id=') else
                'root='+json.dumps(str(ROOT/'backend')) if row.startswith('root=') else
                'require_remote_task_approval=false' if row.startswith('require_remote_task_approval=') else
                'env_allowlist=[]' if row.startswith('env_allowlist=') and body.get('model_network','direct')=='direct' else row for row in rows]
        credential_refs={}
        for row in rows:
            if row.startswith(('credential_source=','models_file=')):
                key,value=row.split('=',1)
                file=Path(json.loads(value))
                credential_refs[key]=dict(path=str(file),sha256=digest(file.read_bytes()))
        (ROOT/'credential-before.json').write_text(json.dumps(credential_refs))
        (ROOT/'agent.toml').write_text('\n'.join(rows)+'\n')
        card = cli(['init', '--name', 'orion-managed-checkpoint', '--port', '39181'])
        return dict(card=card, project=project, before=json.loads((ROOT/'before.json').read_text()))
    if action == 'start':
        record = ROOT/'daemon-process.json'
        assert not record.exists()
        env = os.environ.copy()
        env.update(HTTP_PROXY='http://127.0.0.1:7892', HTTPS_PROXY='http://127.0.0.1:7892')
        with (ROOT/'daemon.log').open('wb') as log:
            process = subprocess.Popen([str(ROOT/'target/debug/hmesh'), '--data-dir', str(ROOT/'node'), 'daemon', 'start'],
                                       env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
        time.sleep(.2)
        assert process.poll() is None
        record.write_text(json.dumps(dict(pid=process.pid, start_ticks=(Path('/proc')/str(process.pid)/'stat').read_text().split()[21])))
        return dict(pid=process.pid)
    if action == 'restart-owned':
        record=ROOT/'daemon-process.json'
        old=json.loads(record.read_text());proc=Path('/proc')/str(old['pid'])
        assert not proc.exists() or not (proc/'cmdline').read_bytes(), 'prior owned daemon still running'
        record.rename(ROOT/('daemon-process-history-'+str(time.time_ns())+'.json'))
        return main('start',{})
    if action == 'final-fault':
        task=cli(['task','show',body['task_id']]);assert task['status']=='SUCCEEDED'
        source=ROOT/'backend/server.mjs';data=source.read_bytes()
        assert digest(data)==body['expected_sha256'], 'backend changed since prior acceptance'
        text=data.decode()
        field=re.compile(r'(?m)^[ \t]*node: resolvedNodeLabel,[ \t]*\r?\n')
        assert len(field.findall(text))==1, 'expected exactly one expanded health node field'
        (ROOT/'fixed-before-timeout-retest.mjs').write_bytes(data)
        (ROOT/'before.json').rename(ROOT/'before-timeout-history.json')
        source.write_text(field.sub('',text,count=1))
        before=dict(source_sha256=digest(source.read_bytes()),original_source_sha256=digest(data),contract_sha256=digest((ROOT/'backend/api-contract.json').read_bytes()))
        (ROOT/'before.json').write_text(json.dumps(before));(ROOT/'final-mode.json').write_text('{}')
        return before
    if action == 'cli':
        return cli(body['arguments'])
    if action == 'config-cli':
        # Only fixed owner-side configuration routes, never a remote shell API.
        command = body['command']
        assert command in ['policy', 'publish']
        file = ROOT/(command+'.json')
        if command == 'policy':
            value = '[default]\naction="deny"\n[[grant]]\npeer='+json.dumps(body['peer'])+'\nagent='+json.dumps(body['agent'])+'\nproject='+json.dumps(ROOT.name+'-backend')+'\nactions=["agent.read","task.submit"]\nrequire_approval=false\n'
            file.write_text(value)
            return cli(['policy', '--file', str(file)])
        file.write_text(json.dumps(body['spec']))
        return cli(['service', 'publish', '--config', str(file)])
    if action == 'service-spec':
        return dict(id=body['id'], project_id=ROOT.name+'-backend', name='orion-managed-todo', target_port=8011,
                    allowed_peer_ids=[body['peer']], allowed_restart_peer_ids=[body['peer']], lease_seconds=3600,
                    health_path='/api/health',request_timeout_seconds=30, process=dict(executable=str(NODE), cwd=str(ROOT/'backend'), args=[str(ROOT/'backend/server.mjs')],
                                                          env=dict(PORT='8011', DATA_FILE=str(ROOT/'data'/('final-todos.json' if (ROOT/'final-mode.json').exists() else 'todos.json')), NODE_LABEL='Orion')))
    if action == 'evidence':
        files = {}
        for name in ['server.mjs', 'api-contract.json', 'server.test.mjs']:
            data = (ROOT/'backend'/name).read_bytes()
            files[name] = dict(sha256=digest(data), bytes=len(data))
        datafile=ROOT/'data'/('final-todos.json' if (ROOT/'final-mode.json').exists() else 'todos.json')
        if datafile.exists():
            files['data'] = dict(sha256=digest(datafile.read_bytes()),name=datafile.name)
        env = dict(PATH=str(NODE.parent)+':/usr/bin:/bin', HOME=str(Path.home()), LANG='C.UTF-8')
        test = subprocess.run([str(NODE), '--test', 'server.test.mjs'], cwd=ROOT/'backend', env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
        refs=json.loads((ROOT/'credential-before.json').read_text())
        unchanged=all(digest(Path(v['path']).read_bytes())==v['sha256'] for v in refs.values())
        source_hashes={}
        for file in (ROOT/'source').rglob('*'):
            if file.is_file() and (file.suffix in ['.rs','.sql'] or file.name in ['Cargo.toml','Cargo.lock']):
                source_hashes[str(file.relative_to(ROOT/'source'))]=digest(file.read_bytes())
        return dict(files=files, tests=dict(exit=test.returncode, output=test.stdout.decode(errors='replace')[-2500:]),
                    before=json.loads((ROOT/'before.json').read_text()), checks=json.loads((ROOT/'checks.json').read_text()),
                    binary_sha256=digest((ROOT/'target/debug/hmesh').read_bytes()),source_hashes=source_hashes,credential_files_unchanged=unchanged)
    if action == 'stop':
        return cli(['daemon', 'stop'])
    if action == 'data-evidence':
        file=ROOT/'data'/('final-todos.json' if (ROOT/'final-mode.json').exists() else 'todos.json')
        data=file.read_bytes()
        return dict(name=file.name,sha256=digest(data),bytes=len(data),at=time.time())
    if action == 'http-diagnostic':
        import urllib.request
        import urllib.error
        request=urllib.request.Request('http://127.0.0.1:8011/api/todos',data=json.dumps(dict(title='direct-managed-diagnostic')).encode(),headers={'Content-Type':'application/json'},method='POST')
        opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request,timeout=7) as response:
                return dict(status=response.status,body=response.read(2048).decode())
        except Exception as error:
            return dict(error_type=type(error).__name__,reason=str(getattr(error,'reason',''))[:200])
    if action == 'storage-diagnostic':
        import urllib.request
        samples=[]
        file=ROOT/'data/fsync-diagnostic'
        for _ in range(4):
            start=time.monotonic()
            with file.open('wb') as output:
                output.write(b'isolated service storage timing\n');output.flush();os.fsync(output.fileno())
            samples.append(round(time.monotonic()-start,3))
        file.unlink()
        return dict(fsync_seconds=samples)
    if action == 'diagnose':
        # No generation request. Inspect only the failed trial's settings and
        # authenticated GET /models connectivity; never return credentials.
        import urllib.request
        task=cli(['task','show',body['task_id']])
        assert task['status'] in ['FAILED','SUCCEEDED','CANCELLED','UNKNOWN']
        attempt=ROOT/'node/runs'/task['last_attempt_id']
        refs=json.loads((ROOT/'credential-before.json').read_text())
        key=json.loads(Path(refs['credential_source']['path']).read_text())['deepseek']['key']
        result=dict(source_changed=digest((ROOT/'backend/server.mjs').read_bytes())!=json.loads((ROOT/'before.json').read_text())['source_sha256'])
        start=time.monotonic()
        try:
            opener=urllib.request.build_opener(urllib.request.ProxyHandler(dict(http='http://127.0.0.1:7892',https='http://127.0.0.1:7892')))
            request=urllib.request.Request('https://api.deepseek.com/models',headers={'Authorization':'Bearer '+key})
            with opener.open(request,timeout=15) as response:
                result['models_http_status']=response.status
                result['model_ids']=[m['id'] for m in json.loads(response.read(131072)).get('data',[])]
        except Exception as error:
            result['models_error_type']=type(error).__name__
        result['models_elapsed_seconds']=round(time.monotonic()-start,3)
        entry=NODE.parent.parent/'lib/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js'
        env=dict(PATH=str(NODE.parent)+':/usr/bin:/bin',LANG='C.UTF-8',PI_CODING_AGENT_DIR=str(attempt/'config'),PI_CODING_AGENT_SESSION_DIR=str(ROOT/'diagnostic-sessions'),PI_OFFLINE='1',PI_TELEMETRY='0')
        process=subprocess.run([str(NODE),str(entry),'--mode','rpc','--offline','--no-session','--no-extensions','--no-skills','--no-prompt-templates','--no-context-files','--no-themes','--no-approve','--provider','deepseek','--model','deepseek-v4-flash','--tools','read,write,edit'],cwd=ROOT/'backend',env=env,input=b'{"id":"diagnose","type":"get_state"}\n',stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=15)
        for line in process.stdout.splitlines():
            item=json.loads(line)
            if item.get('id')=='diagnose':
                data=item.get('data',{})
                result['thinking_level']=data.get('thinkingLevel')
                result['is_streaming']=data.get('isStreaming')
        (ROOT/'diagnostic.json').write_text(json.dumps(result))
        return result
    if action == 'diagnose-network':
        import socket
        import urllib.request
        refs=json.loads((ROOT/'credential-before.json').read_text())
        key=json.loads(Path(refs['credential_source']['path']).read_text())['deepseek']['key']
        results={}
        for label,proxies in [('direct',{}),('original_proxy',dict(http='http://127.0.0.1:7892',https='http://127.0.0.1:7892'))]:
            start=time.monotonic()
            try:
                opener=urllib.request.build_opener(urllib.request.ProxyHandler(proxies))
                request=urllib.request.Request('https://api.deepseek.com/models',headers={'Authorization':'Bearer '+key})
                with opener.open(request,timeout=8) as response:
                    results[label]=dict(status=response.status,elapsed=round(time.monotonic()-start,3))
            except Exception as error:
                results[label]=dict(error_type=type(error).__name__,reason=str(getattr(error,'reason',''))[:250].replace(key,'[REDACTED]'),elapsed=round(time.monotonic()-start,3))
        try:
            connection=socket.create_connection(('127.0.0.1',7892),3);connection.close();results['proxy_tcp']='connected'
        except Exception as error:
            results['proxy_tcp']=type(error).__name__
        (ROOT/'network-diagnostic.json').write_text(json.dumps(results))
        return results
    if action == 'direct-agent':
        file=ROOT/'agent-direct.toml'
        assert not file.exists(), 'direct agent already prepared; query its existing task'
        rows=(ROOT/'agent.toml').read_text().splitlines()
        rows=['name="orion-managed-repair-direct"' if row.startswith('name=') else
              'env_allowlist=[]' if row.startswith('env_allowlist=') else row for row in rows]
        file.write_text('\n'.join(rows)+'\n')
        agent=cli(['agent','register','--config',str(file)])
        verification=cli(['agent','verify',agent['agent_id'],'--allow-model'])
        return dict(agent=agent,verification=verification,network=json.loads((ROOT/'network-diagnostic.json').read_text()))
    if action == 'cleanup-status':
        import sqlite3
        record=json.loads((ROOT/'daemon-process.json').read_text())
        proc=Path('/proc')/str(record['pid'])
        alive=proc.exists() and bool((proc/'cmdline').read_bytes()) and (proc/'stat').read_text().split()[21]==record['start_ticks']
        db=sqlite3.connect('file:'+str(ROOT/'node/state.sqlite3')+'?mode=ro',uri=True)
        records=[json.loads(row[0]) for row in db.execute("SELECT record_json FROM managed_records WHERE kind IN ('service','tunnel','connection')")]
        db.close()
        return dict(owned_daemon_alive=alive,listener_file=(ROOT/'node/daemon.json').exists(),enabled_resources=sum(bool(r['enabled']) for r in records))
    raise RuntimeError('unsupported managed trial action')


if __name__ == '__main__':
    try:
        body = json.loads(base64.b64decode(sys.argv[2])) if len(sys.argv)>2 else {}
        print(json.dumps(main(sys.argv[1], body), ensure_ascii=False))
    except Exception as error:
        print(json.dumps(dict(error=str(error))), file=sys.stderr)
        sys.exit(1)
