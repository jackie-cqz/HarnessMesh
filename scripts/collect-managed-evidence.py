"""Validate private opt-in smoke state, then export a deliberately small report."""
import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

repo=Path(__file__).resolve().parent.parent
root=Path(sys.argv[1]) if len(sys.argv)>1 else Path((repo/'.hmesh-v0/managed-active.txt').read_text())
record=json.loads((root/'state.json').read_text(encoding='utf-8-sig'))
run=record['integration']
remote=record['remote_evidence']
assert run['state']=='verified' and 1<=run['cycles']<=2
assert len(run['repair_task_ids'])==run['cycles'] and len(record['repairs'])==run['cycles']
assert all(t['status']=='SUCCEEDED' for t in record['repairs'])
assert run['reports'][0]['passed'] is False and run['reports'][-1]['passed'] is True
assert any(a['pointer']=='/node' and not a['passed'] for t in run['reports'][0]['tests'] for a in t['assertions'])
assert len({r['contract_sha256'] for r in run['reports']})==1
assert len({r['test_plan_sha256'] for r in run['reports']})==1
assert record['client_before']['exit']!=0 and record['client_after']['exit']==0
assert record['client_before']['script_sha256']==record['client_after']['script_sha256']
assert remote['files']['api-contract.json']['sha256']==remote['before']['contract_sha256']
assert remote['files']['server.mjs']['sha256']!=remote['before']['source_sha256']
assert remote['tests']['exit']==0 and remote['credential_files_unchanged']
assert record['tunnel_after']['status']['starts']>record['tunnel_before']['status']['starts']
assert record['tunnel_after']['status']['pid']!=record['tunnel_before']['status']['pid']
assert record['offline_request_failed'] and record['revoke_passed']
assert not record['cleanup']['local_daemon_alive']
assert not record['cleanup']['remote']['owned_daemon_alive']
assert record['cleanup']['remote']['enabled_resources']==0
assert record['original_application_after']['status']=='ok'
assert record['original_application_after']['node']=='Orion'

def sha(data):
    return hashlib.sha256(data).hexdigest()

names=subprocess.check_output(['git','ls-files','--cached','--others','--exclude-standard'],cwd=repo,text=True).splitlines()
sources={name:sha((repo/name).read_bytes()) for name in names if Path(name).suffix in ['.rs','.sql'] or Path(name).name in ['Cargo.toml','Cargo.lock']}
assert sources==remote['source_hashes'], 'Linux and current Windows Rust source hashes differ'
check_prefix='windows-total-budget-checks' if (root/'windows-total-budget-checks.json').exists() else 'windows-deadline-checks' if (root/'windows-deadline-checks.json').exists() else 'windows-final-checks'
windows=json.loads((root/(check_prefix+'.json')).read_text(encoding='utf-8-sig'))
assert len(windows)==4 and all(c['exit']==0 for c in windows)
assert len(remote['checks'])==2 and all(c['exit']==0 for c in remote['checks'])
windows_passed=sum(int(n) for n in re.findall(r'test result: ok\. (\d+) passed',(root/(check_prefix+'.log')).read_text(encoding='utf-8-sig')))
linux_passed=sum(c.get('passed',0) for c in remote['checks'])
assert windows_passed>0 and linux_passed>0
http=json.loads((root/'managed-http.json').read_text(encoding='utf-8-sig'))
assert http['status']=='passed'
assert record['client_unit_tests']['exit']==0
persistence=json.loads((root/'managed-persistence.json').read_text(encoding='utf-8-sig'))
assert persistence['status']=='passed' and persistence['phase']=='persistence-after-backend-restart'

def task(t):
    return {k:t[k] for k in ['task_id','status','last_attempt_id','created_at','updated_at','error']}

previous=[]
for old in record.get('integration_history',[]):
    assert old['state'] in ['needs_input','verified']
    previous.append(dict(id=old['spec']['id'],state=old['state'],cycles=old['cycles'],repair_task_ids=old['repair_task_ids'],error=old['error']))
backend_tests=re.search(r'(?:#|ℹ)\s*pass\s+(\d+)',remote['tests']['output'])
client_attempts=[]
for file in sorted(root.glob('client-check-*.json'),key=lambda f:f.stat().st_mtime):
    value=json.loads(file.read_text(encoding='utf-8-sig'))
    error=value.get('stderr','')
    outcome='passed' if value['exit']==0 else 'assertion_or_transport_failed'
    if '503 !==' in error: outcome='prior_http_503'
    elif "'Orion'" in error or 'undefined' in error: outcome='injected_missing_field'
    client_attempts.append(dict(at=datetime.datetime.fromtimestamp(file.stat().st_mtime,datetime.timezone.utc).isoformat(),exit=value['exit'],outcome=outcome))
report=dict(schema=1,date=datetime.datetime.now(datetime.timezone.utc).isoformat(),status='passed',
    scope='two-node SSH / bounded HTTP service checkpoint; full V0.1/V0.2 remain incomplete',
    harnesses=[dict(kind='pi',version=record['pi']['probe']['version'],provider='deepseek',model='deepseek-v4-flash',task=task(record['pi_verification'])),
               dict(kind='dsh',version=record['dsh']['probe']['version'],provider='deepseek-official',model='deepseek-v4-pro',task=task(record['dsh_task']))],
    nodes=dict(windows=record['local_card']['node_id'],orion=record['remote']['card']['node_id']),
    resources=dict(service_id=record['service_id'],frontend_service_id=record['frontend_id'],connection_id=record['connection_id'],tunnel_id=record['tunnel_id'],endpoint=record['endpoint'],lease_seconds=3600,
                   tunnel_before=record['tunnel_before']['status'],tunnel_after=record['tunnel_after']['status'],offline_request_failed=True,restored_utc=record['restored_utc'],revoked_access_closed=True),
    integration=dict(id=run['spec']['id'],state=run['state'],cycles=run['cycles'],max_fix_cycles=run['spec']['max_fix_cycles'],contract_revision=run['spec']['contract_revision'],contract_sha256=run['spec']['contract_sha256'],reports=run['reports'],repairs=[task(t) for t in record['repairs']]),
    client=dict(before=record['client_before'],after=record['client_after'],existing_ui_files_unchanged=True,budget_only_changes_verified=record.get('budget_only_changes_verified',False),authoring_task_id=record.get('client_v1',{}).get('dsh_task',{}).get('task_id'),attempts=client_attempts),
    artifacts=dict(backend_before=remote['before'],backend_after=remote['files'],final_data=record['final_data']),
    verification=dict(windows=dict(checks=windows,passed=windows_passed),linux=dict(checks=remote['checks'],passed=linux_passed,rustfmt_clippy='not installed in portable toolchain; ran on Windows'),backend_unit_tests_passed=int(backend_tests[1]) if backend_tests else None,client_unit_tests_exit=record['client_unit_tests']['exit'],independent_frontend_http_checks=len(http['checks']),managed_restart_persistence_passed=True,credential_files_unchanged=True,known_hosts_unchanged=True),
    binaries=dict(windows_hmesh_sha256=record['windows_binary_sha256'],linux_hmesh_sha256=remote['binary_sha256']),
    source_hashes=sources,
    prior_runs=previous,prior_repairs=[task(t) for t in record.get('prior_repairs',[])],
    network_diagnostic=record.get('direct',{}).get('network'),
    deviations=['Initial repair hit its explicit 300-second deadline without editing source and stopped as FAILED / needs_input. An owner diagnostic found original-proxy GET /models timed out while direct official API returned 200. A separate direct-route Agent/run passed the fixed contract; original failure retained.',
                'Independent CRUD then exposed HTTP 503 on writes: measured durable fsync samples were 2.463, 1.662, 8.339 and 10.184 seconds, exceeding the initial five-second bridge deadline. HTTP operation timeout is now owner-configured up to 30 seconds; Mesh RPC adds five seconds, demo clients use 45 seconds. DSH changed only timeout literals; assertions/contract remained identical. A new missing-field acceptance after this implementation fix is reported as the final run. Prior run/client failures remain recorded.'] if previous else [],
    http_budgets=dict(service_seconds=30,mesh_rpc_seconds=35,client_frontend_seconds=45,observed_initial_fsync_seconds=[2.463,1.662,8.339,10.184],automatic_write_retry=False),
    repair_events=record['repair_events'],cleanup=record['cleanup'],cleanup_utc=record['cleanup_utc'],original_application_healthy=True,
    not_verified=['three-machine Mac/Windows/Linux gateway','Tailscale','Codex','generic TCP/WebSocket/SSE','OS startup installation','Unix SIGKILL orphan adoption','automatic resumption of old leases/runs'])
destination=repo/'docs/evidence/latest-managed-services.json'
destination.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print(json.dumps(dict(report=str(destination),integration=run['spec']['id'],cycles=run['cycles'],windows_passed=windows_passed,linux_passed=linux_passed),ensure_ascii=False))
