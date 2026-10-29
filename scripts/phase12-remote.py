"""Orion side of explicitly authorized bidirectional SSH and native MCP smoke."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sqlite3
import sys

ROOT=Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('todo_control',str(ROOT/'todo-remote.py'))
control=importlib.util.module_from_spec(spec);spec.loader.exec_module(control)
def sha(data):return hashlib.sha256(data).hexdigest()
def main(action,body):
    token=body['token'];assert re.fullmatch(r'[a-f0-9]{32}',token)
    trial=ROOT/('phase12-'+token)
    if action=='init':
        trial.mkdir(mode=0o700)
        policy_path=ROOT/'node/policy.toml'
        (trial/'before-policy.toml').write_bytes(policy_path.read_bytes())
        def agent(role,artifact,mesh=False):
            project='phase12-'+role+'-'+token
            folder=trial/role;folder.mkdir(mode=0o700)
            rows=(ROOT/'agent.toml').read_text().splitlines()
            rows=[('name='+json.dumps('orion-'+role)) if line.startswith('name=') else
                  ('project_id='+json.dumps(project)) if line.startswith('project_id=') else
                  ('root='+json.dumps(str(folder))) if line.startswith('root=') else
                  ('artifact_paths='+json.dumps([artifact])) if line.startswith('artifact_paths=') else line for line in rows]
            if mesh:
                server=Path(control.read_config()['hmesh']).with_name('hmesh-mcp');assert server.is_file()
                rows+=['mesh_server='+json.dumps(str(server)),'mesh_scope='+json.dumps(str(trial/'mesh-scope.json'))]
            config=trial/(role+'.toml');config.write_text('\n'.join(rows)+'\n')
            card=control.cli(['agent','register','--config',str(config)])
            verification=control.cli(['agent','verify',card['agent_id'],'--allow-model'])
            return {'agent':card,'project':project,'verification':verification,'root':str(folder)}
        scope={'targets':[{'peer_id':body['windows_node'],'agent_id':body['dsh_agent'],'project_id':body['dsh_project']}],
               'max_submissions':1,'max_timeout_seconds':300,'max_output_bytes':1048576}
        (trial/'mesh-scope.json').write_text(json.dumps(scope))
        worker=agent('worker','out/forward.json');parent=agent('parent','out/delegated.json',True)
        policy=(trial/'before-policy.toml').read_text()
        policy=re.sub(r'(?m)^grant\s*=\s*\[\s*\]\s*\n','',policy)
        policy+='\n[[grant]]\npeer='+json.dumps(body['windows_node'])+'\nagent='+json.dumps(worker['agent']['agent_id'])
        policy+='\nproject='+json.dumps(worker['project'])+'\nactions=["agent.read","task.submit"]\nrequire_approval=true\n'
        policy+='\n[[grant]]\npeer='+json.dumps(body['windows_node'])+'\nagent='+json.dumps(parent['agent']['agent_id'])
        policy+='\nproject='+json.dumps(parent['project'])+'\nactions=["agent.read","task.submit"]\nrequire_approval=true\n'
        (trial/'policy.toml').write_text(policy)
        control.cli(['policy','--file',str(trial/'policy.toml')])
        (trial/'policy-after.sha256').write_text(sha(policy_path.read_bytes()))
        return {'worker':worker,'parent':parent,'scope':scope,'root':str(trial)}
    if action=='artifact':
        role,path={'forward':('worker','out/forward.json'),'parent':('parent','out/delegated.json')}[body['which']]
        file=trial/role/path
        assert file.resolve().is_relative_to((trial/role).resolve()) if hasattr(Path,'is_relative_to') else str(file.resolve()).startswith(str((trial/role).resolve())+'/')
        data=file.read_bytes();assert len(data)<=65536
        return {'json':json.loads(data),'sha256':sha(data),'bytes':len(data)}
    if action=='delegations':
        parent=body['parent'];assert re.fullmatch(r'[a-f0-9-]{36}',parent)
        db=sqlite3.connect(str(ROOT/'node/state.sqlite3'))
        rows=db.execute('SELECT request_id,child_task_id,peer_id FROM tool_submissions WHERE parent_task_id=?',(parent,)).fetchall();db.close()
        return [{'request_id':a,'task_id':b,'peer_id':c} for a,b,c in rows]
    if action=='restore-policy':
        assert sha((ROOT/'node/policy.toml').read_bytes())==(trial/'policy-after.sha256').read_text(),'policy changed outside this smoke; refusing to overwrite'
        return control.cli(['policy','--file',str(trial/'before-policy.toml')])
    raise RuntimeError('unsupported phase action')
if __name__=='__main__':
    try:print(json.dumps(main(sys.argv[1],json.loads(base64.b64decode(sys.argv[2]))),ensure_ascii=False))
    except Exception as error:
        print(json.dumps({'error':str(error)}),file=sys.stderr);sys.exit(1)
