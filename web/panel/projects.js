'use strict';
// Projects: folders on this computer that sessions work in. This computer's
// sessions are listed by project, with the sessions its agents already keep
// there (their own history) under each one; clicking one continues it here.
// Paired computers keep one flat list each. A session's local name, pin and
// hidden flag apply everywhere. Agent and folder text is always set as text.
const proj={list:null,loading:null,error:null,history:new Map(),queue:[],reading:0,open:readOpen(),more:new Set(),query:'',showHidden:false,menu:null,continuing:new Set(),folders:null,foldersRead:false,step:'choose',pick:null,preview:null,previewMore:new Set(),lastFolder:null,noPicker:false,adding:null};
const EARLIER_SHOWN=6;
function projectsSupported(){return state.runtime?.capabilities?.projects===true;}
function readOpen(){try{return new Map(Object.entries(JSON.parse(localStorage.getItem('hmesh.panel.projects.open')||'{}')));}catch{return new Map();}}
function saveOpen(){try{localStorage.setItem('hmesh.panel.projects.open',JSON.stringify(Object.fromEntries(proj.open)));}catch{}}

// The daemon explains refusals in English; say them in the panel's language.
const projectProblems=[
  [/must be an absolute path/,'请填写文件夹的完整路径，比如 /Users/你/code/项目'],
  [/does not exist on this computer/,'这台电脑上没有这个文件夹'],
  [/is not a folder/,'这个路径不是文件夹'],
  [/already the project "(.*)"/,m=>`这个文件夹已经是项目「${m[1]}」了`],
  [/already a project/,'这个文件夹已经是项目了'],
  [/not valid UTF-8/,'这个路径里有认不出的字符'],
  [/at most (\d+) projects/,m=>`最多只能有 ${m[1]} 个项目`],
  [/the name is empty/,'名字不能是空的'],
  [/names are at most (\d+)/,m=>`名字最多 ${m[1]} 个字`],
  [/control characters/,'名字里有不能用的字符'],
  [/can be hidden but not removed/,'Agent 自己的目录只能隐藏，不能移除'],
  [/no such project/,'这个项目已经不在了'],
  [/project folder is no longer on this computer/,'项目的文件夹已经不在这台电脑上了'],
  [/not shared with other computers/,'对方没有共享这个项目'],
  [/no longer shared/,'这个项目已经不再共享'],
  [/cannot continue its earlier sessions/,'这个 Agent 不能接着它之前的会话'],
  [/unknown session/,'找不到这个会话'],
];
// The folder dialog's refusals come with a code.
const pickerProblems={
  FOLDER_PICKER_BUSY:'这台电脑上已经开着一个文件夹选择框了，先在那里选好或者关掉它',
  FOLDER_PICKER_UNAVAILABLE:'这台电脑弹不出文件夹选择框，请直接填文件夹的完整路径',
  FOLDER_PICKER_TIMEOUT:'文件夹选择框开了 10 分钟没有选，已经关掉了',
};
function projectProblem(e){
  if(pickerProblems[e?.code])return pickerProblems[e.code];
  if(e?.code==='FOLDER_PICKER_FAILED')return `文件夹选择框没有打开：${e.message||'原因不明'}`;
  const text=e?.message||'';
  for(const [pattern,say]of projectProblems){const m=pattern.exec(text);if(m)return typeof say==='function'?say(m):say;}
  return text||e?.code||'原因不明';
}
// A folder as people read it: the home folder as ~, long paths by their end.
function shortPath(path){
  let p=String(path||'');
  const home=/^(\/Users\/[^/]+|\/home\/[^/]+|[A-Za-z]:\\Users\\[^\\]+)(?=[/\\]|$)/.exec(p);
  if(home)p='~'+p.slice(home[0].length);
  const sep=p.includes('\\')&&!p.includes('/')?'\\':'/',parts=p.split(sep).filter(Boolean);
  return parts.length>3?`…${sep}${parts.slice(-2).join(sep)}`:p;
}
function seconds(rfc3339){const t=Date.parse(rfc3339||'');return Number.isFinite(t)?t/1000:null;}
function since(rfc3339){const t=seconds(rfc3339);if(t===null)return '';const a=ago(t);return a==='刚刚'?a:`${a}前`;}
// A name the daemon accepts: no control characters, at most 120 characters.
function cleanTitle(text){return [...String(text||'').replace(/[\u0000-\u001f\u007f-\u009f]/g,' ').trim()].slice(0,120).join('').trim();}

async function readProjects(){
  if(!projectsSupported()||!access)return null;
  try{proj.list=unwrap(await api('/projects'));proj.error=null;}
  catch(e){proj.error=e;}
  return proj.list;
}
function projectFor(s,projects=proj.list||[]){
  if(s.cwd)return projects.find(p=>p.path===s.cwd)||null;
  return projects.find(p=>(p.agent_ids||[]).includes(s.agent_id))||null;
}
// The project a session works in, for its header: this computer's project,
// or one a paired computer shares.
function sessionProjectName(s){
  if(!projectsSupported())return null;
  if(!s._remote)return projectFor(s)?.name||null;
  return (conv.nodes.find(n=>n.node_id===s.node_id)?.projects||[]).find(p=>p.project_id===s.project_id)?.name||null;
}

// ---- Earlier sessions: what each agent already keeps in a project ----
function requestHistory(id,refresh=false){
  const known=proj.history.get(id);
  if(known?.state==='loading'||(known&&!refresh))return;
  proj.history.set(id,{...known,state:'loading'});
  proj.queue.push([id,refresh]);pumpHistory();
}
function pumpHistory(){
  // Each read starts the agents briefly; two projects at a time is plenty.
  while(proj.reading<2&&proj.queue.length){
    const [id,refresh]=proj.queue.shift();proj.reading++;
    (async()=>{
      try{const data=unwrap(await api(`/projects/${encodeURIComponent(id)}/history?refresh=${refresh}`,{signal:AbortSignal.timeout(90000)}));proj.history.set(id,{state:'ready',data});}
      catch(e){proj.history.set(id,{state:'error',error:e,data:proj.history.get(id)?.data});}
      finally{proj.reading--;renderSessionList();pumpHistory();}
    })();
  }
}
// Earlier sessions not yet continued here, newest first.
function earlierSessions(id,local){
  const data=proj.history.get(id)?.data;if(!data)return [];
  const continued=new Set(local.filter(s=>s.agent_session_id).map(s=>`${s.agent_id}/${s.agent_session_id}`));
  const rows=[];
  for(const agent of data.agents||[])for(const entry of agent.sessions||[]){
    if(entry.session_id||continued.has(`${agent.agent_id}/${entry.agent_session_id}`))continue;
    rows.push({agent,entry,title:entry.title||'没有标题的会话'});
  }
  return rows.sort((a,b)=>(seconds(b.entry.updated_at)||0)-(seconds(a.entry.updated_at)||0));
}
// quiet: the caller says what went wrong instead of a toast.
async function continueEarlier(project,agent,entry,quiet=false){
  const key=`${agent.agent_id}/${entry.agent_session_id}`;
  if(proj.continuing.has(key)||!canWrite())return;
  proj.continuing.add(key);renderSessionList();
  try{
    const title=cleanTitle(entry.title);
    const record=unwrap(await api('/sessions',{method:'POST',body:JSON.stringify({agent_id:agent.agent_id,project_id:project.project_id,resume:entry.agent_session_id,...(title?{title}:{})})}));
    entry.session_id=record.session_id;
    if(state.view!=='sessions')showView('sessions');
    selectSession({...record,_remote:false});
    loadSessions();
  }catch(e){if(quiet)throw e;toast(`没有接上这个会话：${projectProblem(e)}`);}
  finally{proj.continuing.delete(key);renderSessionList();}
}

// ---- Sidebar: this computer → projects → sessions; paired computers flat ----
function sideIcon(name,label,action){
  const b=el('button','side-icon');b.type='button';b.setAttribute('aria-label',label);b.title=label;b.append(icon(name,'sm'));
  b.addEventListener('click',e=>{e.stopPropagation();action(b);});return b;
}
function sessionItem(s,child){
  const mark=markOf(s.session_id),item=el('div',`session-item${mark?.hidden?' is-hidden':''}`);
  const menu=sideIcon('ellipsis','会话操作',b=>openMenu(b,[
    {label:'改名',action:()=>renameSession(s)},
    {label:mark?.pinned?'取消置顶':'置顶',action:()=>markSession(s,{pinned:!mark?.pinned})},
    {label:mark?.hidden?'取消隐藏':'隐藏',action:()=>markSession(s,{hidden:!mark?.hidden})},
  ]));
  menu.classList.add('row-menu');menu.setAttribute('aria-haspopup','menu');
  item.append(sessionRow(s,child),menu);return item;
}
function earlierRow(project,{agent,entry,title}){
  const key=`${agent.agent_id}/${entry.agent_session_id}`,row=el('button','session-row earlier');row.type='button';
  row.append(el('span','state-dot earlier'),el('span','session-row-title',title),el('span','session-row-agent',agent.agent_name),el('span','session-row-time',entry.updated_at?ago(seconds(entry.updated_at)||0):''));
  row.title=`${agent.agent_name} 里之前的会话，点开接着聊`;
  row.disabled=proj.continuing.has(key)||!canWrite();
  row.addEventListener('click',()=>continueEarlier(project,agent,entry));
  return row;
}
// Pinned first, then the latest; a delegated session stays under its caller.
function placeRows(items,matches){
  const ids=new Set(items.map(s=>s.session_id)),parentOf=s=>s.origin?.chain?.at(-1)?.session_id;
  const kids=new Map();for(const s of items){const p=parentOf(s);if(p&&ids.has(p)){if(!kids.has(p))kids.set(p,[]);kids.get(p).push(s);}}
  const keep=s=>matches(s)||(kids.get(s.session_id)||[]).some(keep);
  const roots=items.filter(s=>!(parentOf(s)&&ids.has(parentOf(s)))&&keep(s));
  return {roots,kids,keep};
}
function sortPinned(rows){return [...rows].sort((a,b)=>Number(!!markOf(b.session_id)?.pinned)-Number(!!markOf(a.session_id)?.pinned)||(b.updated_at||0)-(a.updated_at||0));}
function appendTree(box,roots,kids,keep){
  const place=(s,depth)=>{box.append(sessionItem(s,depth>0));for(const c of kids.get(s.session_id)||[])if(keep(c))place(c,depth+1);};
  for(const s of sortPinned(roots))place(s,0);
}
function renderProjectSessions(rows){
  const me=localNodeId(),q=proj.query.trim().toLowerCase(),searching=!!q,showHidden=proj.showHidden;
  if(!proj.list&&!proj.error&&!proj.loading)proj.loading=readProjects().finally(()=>{proj.loading=null;if(proj.list||proj.error)renderSessionList();});
  $('#session-tools').hidden=false;
  const matches=s=>!searching||sessionTitle(s).toLowerCase().includes(q);
  const shownRows=rows.filter(s=>showHidden||!markOf(s.session_id)?.hidden);
  const local=shownRows.filter(s=>!s._remote),all=proj.list||[];
  const projects=all.filter(p=>showHidden||!p.hidden);
  // Delegated sessions follow their caller into its project.
  const {roots,kids,keep}=placeRows(local,matches);
  const byProject=new Map(),other=[];
  for(const s of roots){const p=projectFor(s,all);if(!p)other.push(s);else if(showHidden||!p.hidden){if(!byProject.has(p.project_id))byProject.set(p.project_id,[]);byProject.get(p.project_id).push(s);}}
  const groups=[];
  const localGroup=el('div','session-group'),head=el('div','session-group-name');
  head.append(icon('monitor','xs'),el('span','',nodeLabel(me)));localGroup.append(head);
  if(!proj.list&&!proj.error)localGroup.append(el('div','side-note loading','正在读取项目…'));
  if(proj.error&&!proj.list)localGroup.append(el('div','side-note',`项目暂时读不到：${projectProblem(proj.error)}`));
  let shownBlocks=0;
  const count=projects.length;
  projects.forEach((p,index)=>{
    const here=byProject.get(p.project_id)||[],selected=here.some(s=>s.session_id===conv.selected);
    const open=proj.open.get(p.project_id)??(here.length>0||selected);
    if(open&&!searching)requestHistory(p.project_id);
    const earlier=earlierSessions(p.project_id,local).filter(r=>!searching||r.title.toLowerCase().includes(q));
    if(searching&&!here.length&&!earlier.length)return;
    shownBlocks++;
    localGroup.append(projectBlock(p,{here,kids,keep,earlier,open:open||searching,searching,first:index===0,last:index===count-1}));
  });
  if(other.length){shownBlocks++;localGroup.append(projectBlock(null,{here:other,kids,keep,earlier:[],open:proj.open.get('')??true,searching}));}
  if(searching&&!shownBlocks)localGroup.append(el('div','side-empty',`这台电脑上没有标题里有「${proj.query.trim()}」的会话`));
  if(!searching&&proj.list){
    const add=el('button','add-project');add.type='button';add.append(icon('plus','sm'),el('span','','添加项目'));add.disabled=!canWrite();
    add.addEventListener('click',openProjectDialog);localGroup.append(add);
  }
  groups.push(localGroup);
  // Paired computers: one flat list each, as before.
  const remote=new Map();
  for(const s of shownRows.filter(s=>s._remote)){if(!remote.has(s.node_id))remote.set(s.node_id,[]);remote.get(s.node_id).push(s);}
  for(const [node,items]of remote){
    const tree=placeRows(items,matches);if(!tree.roots.length)continue;
    const group=el('div','session-group'),name=el('div','session-group-name');name.append(icon('monitor','xs'),el('span','',nodeLabel(node)));group.append(name);
    appendTree(group,tree.roots,tree.kids,tree.keep);groups.push(group);
  }
  $('#session-list').replaceChildren(...groups);
  const anyHidden=all.some(p=>p.hidden)||(conv.list?.marks||[]).some(m=>m.hidden);
  $('#session-footer').hidden=!anyHidden&&!showHidden;
}
function projectBlock(p,{here,kids,keep,earlier,open,searching,first,last}){
  const id=p?.project_id??'',block=el('div',`project${p?.hidden?' is-hidden':''}${p?'':' other'}`);block.dataset.project=id;
  const head=el('div','project-head'),toggle=el('button','project-toggle');toggle.type='button';toggle.setAttribute('aria-expanded',String(open));
  // A folder, open while the project is; the chevron for opening it is at the end.
  const folder=icon(open?'folder-open':'folder','xs');folder.classList.add('project-folder');
  toggle.append(folder,el('span','project-name',p?p.name:'其他'));
  if(p)toggle.append(el('span','project-path',shortPath(p.path)));
  if(p?.pinned){const pin=icon('pin','xs');pin.classList.add('project-flag');toggle.append(pin);}
  if(p?.shared){const shared=icon('users','xs');shared.classList.add('project-flag');toggle.append(shared);}
  const total=here.length+earlier.length;if(total)toggle.append(el('span','project-count',total));
  const chevron=icon('chevron-right','xs');chevron.classList.add('project-chevron');toggle.append(chevron);
  toggle.title=p?`${p.path}${p.shared?'\n已共享给配对的电脑':''}${p.hidden?'\n已隐藏':''}`:'不在任何项目里的会话';
  toggle.addEventListener('click',()=>{if(searching)return;proj.open.set(id,!open);saveOpen();renderSessionList();});
  head.append(toggle);
  if(p){
    const actions=el('span','project-actions');
    const start=sideIcon('square-pen','新会话',()=>newInProject(p));start.disabled=!canWrite()||p.hidden;
    const more=sideIcon('ellipsis','项目操作',b=>openProjectMenu(p,b,{first,last}));more.setAttribute('aria-haspopup','menu');
    actions.append(start,more);head.append(actions);
  }
  block.append(head);
  if(!open)return block;
  const body=el('div','project-body');
  appendTree(body,here,kids,keep);
  if(p)appendEarlier(body,p,earlier,searching);
  if(!body.childElementCount)body.append(el('div','side-note','还没有会话'));
  block.append(body);return block;
}
function appendEarlier(body,p,earlier,searching){
  const history=proj.history.get(p.project_id),data=history?.data;
  if(searching){for(const r of earlier)body.append(earlierRow(p,r));return;}
  if(!data){
    if(history?.state==='error')body.append(sideNote(`没读到 Agent 里之前的会话：${projectProblem(history.error)}`,p));
    else body.append(el('div','side-note loading','正在读取 Agent 里之前的会话…'));
    return;
  }
  // Nothing earlier: no heading; the project menu can read the list again.
  if(earlier.length){
    const headRow=el('div','earlier-head');headRow.append(el('span','','Agent 里之前的会话'));
    const refresh=el('button','side-link',history.state==='loading'?'正在刷新…':'刷新');refresh.type='button';refresh.disabled=history.state==='loading';
    refresh.addEventListener('click',()=>{requestHistory(p.project_id,true);renderSessionList();});
    headRow.append(refresh);body.append(headRow);
  }else if(history.state==='loading')body.append(el('div','side-note loading','正在重新读取 Agent 里之前的会话…'));
  const more=proj.more.has(p.project_id),shown=more?earlier:earlier.slice(0,EARLIER_SHOWN);
  for(const r of shown)body.append(earlierRow(p,r));
  if(earlier.length>shown.length){
    const all=el('button','side-link more',`再显示 ${earlier.length-shown.length} 个`);all.type='button';
    all.addEventListener('click',()=>{proj.more.add(p.project_id);renderSessionList();});body.append(all);
  }
  // Agents that could not list stay quiet: one muted line each kind.
  // An agent that never lists is named only in its own folder, not in every project.
  const names=(state,own)=>(data.agents||[]).filter(a=>a.state===state&&(!own||(p.agent_ids||[]).includes(a.agent_id))).map(a=>a.agent_name);
  const unsupported=names('unsupported',true),failed=names('failed');
  if(unsupported.length)body.append(el('div','side-note',`${unsupported.join('、')} 不提供之前的会话`));
  if(failed.length)body.append(el('div','side-note',`${failed.join('、')} 这次没读出之前的会话`));
  if((data.agents||[]).some(a=>a.truncated))body.append(el('div','side-note','会话太多，只列出了最近的一部分'));
  if(history.state==='error')body.append(el('div','side-note',`刷新没成功：${projectProblem(history.error)}`));
}
function sideNote(text,p){
  const note=el('div','side-note');note.append(el('span','',text+' '));
  const again=el('button','side-link','再试一次');again.type='button';again.addEventListener('click',()=>{requestHistory(p.project_id,true);renderSessionList();});
  note.append(again);return note;
}
function newInProject(p){
  const ready=readyAgents().filter(r=>r.node.local);
  const own=ready.find(r=>(p.agent_ids||[]).includes(r.agent.agent_id));
  const current=ready.find(r=>r.node.node_id===conv.target?.node&&r.agent.agent_id===conv.target?.agent);
  const pick=current||own||ready[0];
  if(!pick){toast('这台电脑上现在没有能用的 Agent');return;}
  startNew(pick.node.node_id,pick.agent.agent_id,p.project_id);
}

// ---- Menus ----
function closeMenu(){
  if(!proj.menu)return;
  proj.menu.anchor.setAttribute('aria-expanded','false');proj.menu.box.remove();proj.menu=null;
}
function openMenu(anchor,items){
  const reopen=proj.menu?.anchor===anchor;closeMenu();if(reopen)return;
  const box=el('div','side-menu');box.setAttribute('role','menu');
  for(const item of items){
    if(item.separator){box.append(el('div','side-menu-sep'));continue;}
    if(item.note){box.append(el('div','side-menu-note',item.note));continue;}
    const b=el('button',`side-menu-item${item.danger?' danger':''}`);b.type='button';
    b.setAttribute('role',item.checked===undefined?'menuitem':'menuitemcheckbox');
    b.append(el('span','',item.label));
    if(item.checked!==undefined){b.setAttribute('aria-checked',String(item.checked));const mark=icon('check','sm');mark.classList.add('side-menu-check');b.append(mark);}
    b.disabled=!!item.disabled||!canWrite();
    b.addEventListener('click',()=>{closeMenu();item.action();});
    box.append(b);
  }
  document.body.append(box);
  const r=anchor.getBoundingClientRect(),w=box.offsetWidth,h=box.offsetHeight;
  const left=Math.max(8,Math.min(r.right-w,innerWidth-w-8)),below=r.bottom+4+h<=innerHeight-8;
  box.style.left=`${left}px`;box.style.top=`${below?r.bottom+4:Math.max(8,r.top-h-4)}px`;
  anchor.setAttribute('aria-expanded','true');proj.menu={box,anchor};
  box.querySelector('button:not(:disabled)')?.focus({preventScroll:true});
}
document.addEventListener('pointerdown',e=>{if(proj.menu&&!proj.menu.box.contains(e.target)&&!proj.menu.anchor.contains(e.target))closeMenu();});
document.addEventListener('keydown',e=>{
  if(!proj.menu)return;
  if(e.key==='Escape'){const anchor=proj.menu.anchor;closeMenu();anchor.focus?.();return;}
  if(e.key==='ArrowDown'||e.key==='ArrowUp'){
    const items=[...proj.menu.box.querySelectorAll('button:not(:disabled)')];if(!items.length)return;e.preventDefault();
    const at=items.indexOf(document.activeElement);items[(at+(e.key==='ArrowDown'?1:-1)+items.length)%items.length].focus();
  }
});
addEventListener('resize',closeMenu);
$('#sidebar-directory').addEventListener('scroll',closeMenu,{passive:true});

function openProjectMenu(p,anchor,{first,last}){
  const items=[
    {label:'改名',action:()=>renameProject(p)},
    {label:'重新读取之前的会话',disabled:proj.history.get(p.project_id)?.state==='loading',action:()=>{proj.open.set(p.project_id,true);saveOpen();requestHistory(p.project_id,true);renderSessionList();}},
    {label:p.pinned?'取消置顶':'置顶',action:()=>updateProject(p,{pinned:!p.pinned})},
    {label:'上移',disabled:first,action:()=>updateProject(p,{move_by:-1})},
    {label:'下移',disabled:last,action:()=>updateProject(p,{move_by:1})},
    {label:p.hidden?'取消隐藏':'隐藏',action:()=>updateProject(p,{hidden:!p.hidden})},
    {separator:true},
    {label:'共享给配对的电脑',checked:!!p.shared,action:()=>updateProject(p,{shared:!p.shared})},
    {note:'打开后，配对的电脑上的人可以让 Agent 在这个文件夹里开会话，读写里面的文件。'},
  ];
  if(p.source==='added'||!(p.agent_ids||[]).length)items.push({separator:true},{label:'移除',danger:true,action:()=>removeProject(p)});
  openMenu(anchor,items);
}
async function updateProject(p,change){
  try{
    const updated=unwrap(await api(`/projects/${encodeURIComponent(p.project_id)}`,{method:'POST',body:JSON.stringify(change)}));
    await readProjects();renderSessionList();if(state.view==='sessions'&&!conv.selected)renderHero();
    if(change.hidden===true)toast(`已隐藏「${updated.name}」。勾上会话列表底部的「显示隐藏的项目和会话」可以找回来。`);
    if(change.shared===true)toast(`已共享「${updated.name}」：配对的电脑现在可以在这个文件夹里开会话。`);
    if(change.shared===false)toast(`「${updated.name}」不再共享给配对的电脑。`);
  }catch(e){toast(`没有改成：${projectProblem(e)}`);}
}
async function removeProject(p){
  if(!await confirmAction('移除这个项目？',`${p.name}\n${p.path}\n只是从列表里拿掉，文件夹和里面的文件都不会动。`))return;
  try{
    unwrap(await api(`/projects/${encodeURIComponent(p.project_id)}/remove`,{method:'POST',body:'{}'}));
    proj.history.delete(p.project_id);await readProjects();renderSessionList();toast(`已移除「${p.name}」`);
  }catch(e){toast(`没有移除：${projectProblem(e)}`);}
}
function renameProject(p){
  askName({title:'给项目改名',description:p.path,value:p.name,save:async name=>{
    unwrap(await api(`/projects/${encodeURIComponent(p.project_id)}`,{method:'POST',body:JSON.stringify({name})}));
    await readProjects();renderSessionList();
  }});
}
function renameSession(s){
  askName({title:'给会话改名',description:'只改这台电脑上显示的名字，Agent 那边不变。留空就用回原来的名字。',value:sessionTitle(s),allowEmpty:true,save:async title=>{
    await markSession(s,{title},true);
  }});
}
async function markSession(s,change,quiet=false){
  try{
    const mark=unwrap(await api(`/sessions/${s.session_id}/mark`,{method:'POST',body:JSON.stringify(change)}));
    if(conv.list)conv.list.marks=[...(conv.list.marks||[]).filter(m=>m.session_id!==mark.session_id),mark];
    renderSessionList();if(conv.selected===s.session_id)renderHeader(conv.record);
    if(change.hidden===true)toast(`已隐藏「${sessionTitle(s)}」。勾上会话列表底部的「显示隐藏的项目和会话」可以找回来。`);
  }catch(e){if(quiet)throw e;toast(`没有改成：${projectProblem(e)}`);}
}
// One small dialog for every new name; save() may refuse with a reason.
function askName({title,description,value,allowEmpty=false,save}){
  const dialog=$('#name-dialog'),input=$('#name-input'),message=$('#name-message');
  $('#name-title').textContent=title;$('#name-description').textContent=description||'';$('#name-description').hidden=!description;
  input.value=value||'';message.hidden=true;$('#name-save').disabled=!canWrite();
  $('#name-form').onsubmit=async e=>{
    e.preventDefault();const name=input.value.trim();
    if(!name&&!allowEmpty){message.textContent='名字不能是空的';message.hidden=false;return;}
    $('#name-save').disabled=true;
    try{await save(name);dialog.close();}
    catch(err){message.textContent=`没有保存：${projectProblem(err)}`;message.hidden=false;}
    finally{$('#name-save').disabled=!canWrite();}
  };
  dialog.showModal();input.focus();input.select();
}
$('#name-cancel').addEventListener('click',()=>$('#name-dialog').close());
$('#name-input').addEventListener('input',()=>{$('#name-message').hidden=true;});

// ---- Adding a project: pick a folder, see what each agent has there ----
// The folder comes from this computer's own folder dialog, a typed path or a
// folder the agents have used. The preview lists each local agent's sessions
// in it: add the folder as a project, or continue one of them at once.
const PICK_WAIT=11*60*1000,PREVIEW_SHOWN=8;
const projectDialog=$('#project-dialog');
function pickerShown(){return state.runtime?.capabilities?.folder_picker===true&&!proj.noPicker;}
function folderLabel(path){return String(path||'').split(/[\\/]/).filter(Boolean).pop()||String(path||'');}
// The folder above, where the next folder dialog starts.
function parentFolder(path){
  const m=/^(.*?)[\\/]+[^\\/]+[\\/]*$/.exec(path||'');if(!m)return null;
  return m[1]&&!m[1].endsWith(':')?m[1]:m[1]+(String(path).includes('\\')?'\\':'/');
}
function isFullPath(path){return /^(\/|[A-Za-z]:[\\/]|\\\\)/.test(path);}
function projectMessage(text,error=false){const m=$('#project-message');m.textContent=text;m.classList.toggle('error',error);}
function openProjectDialog(){
  proj.preview=null;proj.noPicker=false;proj.adding=null;proj.foldersRead=false;proj.previewMore.clear();
  $('#project-path').value='';$('#project-history').open=false;projectMessage('');
  showProjectStep('choose');projectDialog.showModal();focusChoose();
}
function focusChoose(){(pickerShown()&&!proj.pick?$('#project-pick'):$('#project-path')).focus();}
function showProjectStep(step){
  proj.step=step;
  $('#project-choose').hidden=step!=='choose';$('#project-preview').hidden=step!=='preview';
  drawChoose();drawPreview();drawProjectFooter();
}
function drawChoose(){
  const picker=pickerShown(),waiting=!!proj.pick;
  $('#project-pick-box').hidden=!picker;
  $('#project-pick').hidden=waiting;$('#project-pick').disabled=!canWrite();
  $('#project-waiting').hidden=!waiting;
  $('#project-path-label').textContent=picker?'或者填文件夹的完整路径':'文件夹的完整路径';
  $('#project-look').disabled=!canWrite();
}
// The project this folder already is, if any.
function previewProject(){
  const p=proj.preview;if(!p)return null;
  const id=p.data?.project_id,list=proj.list||[];
  return list.find(x=>id?x.project_id===id:x.path===p.path)||(id?{project_id:id,name:p.name,path:p.path}:null);
}
function drawProjectFooter(){
  const back=$('#project-back'),add=$('#project-add');
  if(proj.step!=='preview'){back.textContent='关闭';back.disabled=false;add.hidden=true;return;}
  const known=previewProject();
  back.textContent='返回';back.disabled=!!proj.adding;add.hidden=false;
  add.textContent=proj.adding==='folder'?'正在添加…':known?'打开项目':'添加为项目';
  add.disabled=!!proj.adding||(!known&&!canWrite());
}

// This computer's own folder dialog. The request waits for the answer; ending
// it (取消, or closing this dialog) closes the folder dialog too.
async function pickFolder(){
  if(proj.pick||!canWrite())return;
  const controller=new AbortController(),pick={controller,timedOut:false};
  const timer=setTimeout(()=>{pick.timedOut=true;controller.abort();},PICK_WAIT);
  const start=parentFolder(proj.lastFolder);
  proj.pick=pick;projectMessage('');drawChoose();$('#project-pick-cancel').focus();
  try{
    const result=unwrap(await api('/folders/pick',{method:'POST',body:JSON.stringify(start?{start}:{}),signal:controller.signal}));
    if(proj.pick!==pick)return;
    proj.pick=null;
    if(result?.path){previewFolder(result.path);return;}
    drawChoose();focusChoose();
  }catch(e){
    if(proj.pick!==pick)return;
    proj.pick=null;
    if(e.code==='FOLDER_PICKER_UNAVAILABLE')proj.noPicker=true;
    if(pick.timedOut)projectMessage(projectProblem({code:'FOLDER_PICKER_TIMEOUT'}),true);
    else if(e.name!=='AbortError')projectMessage(projectProblem(e),true);
    drawChoose();focusChoose();
  }finally{clearTimeout(timer);}
}
function stopPicking(){
  const pick=proj.pick;if(!pick)return;
  proj.pick=null;pick.controller.abort();
  if(projectDialog.open&&proj.step==='choose'){drawChoose();focusChoose();}
}

// What each local agent keeps in the folder. A typed path that is refused
// goes back to the path box with the reason.
async function previewFolder(path,{typed=false,refresh=false}={}){
  const preview={path,name:folderLabel(path),state:'loading',typed};
  proj.preview=preview;proj.lastFolder=path;proj.previewMore.clear();projectMessage('');
  showProjectStep('preview');$('#project-add').focus();
  try{
    const data=unwrap(await api(`/folders/history?path=${encodeURIComponent(path)}&refresh=${refresh}`,{signal:AbortSignal.timeout(120000)}));
    if(proj.preview!==preview)return;
    Object.assign(preview,{state:'ready',data,path:data.path,name:data.name});proj.lastFolder=data.path;
  }catch(e){
    if(proj.preview!==preview)return;
    if(typed){proj.preview=null;showProjectStep('choose');projectMessage(projectProblem(e),true);$('#project-path').focus();return;}
    Object.assign(preview,{state:'error',error:e});
  }
  if(projectDialog.open){drawPreview();drawProjectFooter();}
}
function drawPreview(){
  const box=$('#project-preview'),p=proj.preview;
  if(proj.step!=='preview'||!p){box.replaceChildren();return;}
  const known=previewProject(),head=el('div','preview-head'),name=el('div','preview-name');
  name.append(el('b','',p.name));
  if(known)name.append(folderFlag(known.name&&known.name!==p.name?`已是项目「${known.name}」`:'已是项目'));
  head.append(name,el('span','folder-path',p.path));
  const parts=[head];
  if(p.state==='loading')parts.push(el('p','preview-note loading','正在问各个 Agent 在这个文件夹里有哪些会话，可能要几秒…'));
  else if(p.state==='error'){
    const note=el('p','preview-note');note.append(el('span','',`没读到这个文件夹里的会话：${projectProblem(p.error)} `));
    const again=el('button','side-link','再试一次');again.type='button';again.addEventListener('click',()=>previewFolder(p.path,{refresh:true}));
    note.append(again);parts.push(note);
  }else parts.push(...previewAgents(p,known));
  box.replaceChildren(...parts);
}
function folderFlag(text){const flag=el('span','folder-flag');flag.append(icon('check','xs'),el('span','',text));return flag;}
const newest=(a,b)=>(seconds(b.updated_at)||0)-(seconds(a.updated_at)||0);
function previewAgents(p,known){
  const agents=p.data.agents||[];
  if(!agents.length)return [el('p','preview-note','这台电脑上还没有 Agent。')];
  // Agents with sessions first; those that could not list at the end.
  const rank=a=>a.state==='listed'?((a.sessions||[]).length?0:1):a.state==='failed'?2:3;
  const parts=[];
  if(!known&&agents.some(a=>(a.sessions||[]).some(s=>!s.session_id)))parts.push(el('p','preview-note','点一个会话，会先把这个文件夹加成项目，再接着聊。'));
  for(const agent of [...agents].sort((a,b)=>rank(a)-rank(b))){
    const group=el('section','preview-agent'),head=el('div','preview-agent-head'),sessions=[...(agent.sessions||[])].sort(newest);
    head.append(el('b','',agent.agent_name));
    const quiet=agent.state==='unsupported'?'不提供会话记录':agent.state==='failed'?`没读到：${agent.message?projectProblem({message:agent.message}):'原因不明'}`:sessions.length?'':'这里没有会话';
    if(quiet)head.append(el('span','preview-agent-state',quiet));
    group.append(head);
    if(sessions.length){
      const shown=proj.previewMore.has(agent.agent_id)?sessions:sessions.slice(0,PREVIEW_SHOWN),list=el('div','folder-list');
      for(const entry of shown)list.append(previewSession(agent,entry));
      group.append(list);
      if(sessions.length>shown.length){
        const more=el('button','side-link preview-more',`再显示 ${sessions.length-shown.length} 个`);more.type='button';
        more.addEventListener('click',()=>{proj.previewMore.add(agent.agent_id);drawPreview();});group.append(more);
      }
      if(agent.truncated)group.append(el('p','preview-note','会话太多，只列出了最近的一部分'));
    }
    parts.push(group);
  }
  return parts;
}
function previewSession(agent,entry){
  const open=!!entry.session_id,row=el('button','folder-row preview-session');row.type='button';
  row.dataset.key=`${agent.agent_id}/${entry.agent_session_id}`;
  const title=entry.title||'没有标题的会话',go=el('span','folder-go',open?'打开':'接着聊');go.append(icon('chevron-right','xs'));
  row.append(el('span','preview-title',title),el('span','preview-time',since(entry.updated_at)),go);
  row.title=open?`${title}\n已经在这里接着聊过，点开看看`:`${title}\n接着 ${agent.agent_name} 里的这个会话聊`;
  row.disabled=!!proj.adding||(!open&&!canWrite());
  row.addEventListener('click',()=>pickSession(agent,entry));
  return row;
}
// While the folder is being added or a session continued, nothing else in
// the preview can start.
function holdPreview(key){
  for(const b of document.querySelectorAll('#project-preview button'))b.disabled=true;
  const go=key&&[...document.querySelectorAll('#project-preview .preview-session')].find(r=>r.dataset.key===key)?.querySelector('.folder-go');
  if(go)go.firstChild.textContent='正在接上…';
  drawProjectFooter();
}
// Expand the project in the sidebar, clearing a search that would hide it.
function showInSidebar(p){
  proj.open.set(p.project_id,true);saveOpen();
  if(proj.query){proj.query='';$('#session-search').value='';}
  if(p.hidden&&!proj.showHidden)toast(`「${p.name}」已隐藏。勾上会话列表底部的「显示隐藏的项目和会话」可以看到。`);
  renderSessionList();
}
function revealInSidebar(id){
  const toggle=[...document.querySelectorAll('#session-list .project')].find(b=>b.dataset.project===id)?.querySelector('.project-toggle');
  if(toggle){toggle.scrollIntoView({block:'nearest'});toggle.focus({preventScroll:true});}
}
// The previewed folder as a project: the one it is, or added now. What the
// preview read is kept, so the sidebar need not ask the agents again.
async function previewAsProject(){
  const p=proj.preview,known=previewProject();
  if(known){
    let found=(proj.list||[]).find(x=>x.project_id===known.project_id);
    if(!found){await readProjects();found=(proj.list||[]).find(x=>x.project_id===known.project_id);}
    if(!found)throw Error('no such project');
    showInSidebar(found);return {project:found,added:false};
  }
  const added=unwrap(await api('/projects',{method:'POST',body:JSON.stringify({path:p.data?.path||p.path})}));
  if(p.data){p.data.project_id=added.project_id;proj.history.set(added.project_id,{state:'ready',data:{project_id:added.project_id,agents:p.data.agents,observed_at:p.data.observed_at}});}
  for(const folder of proj.folders?.data?.folders||[])if(folder.path===added.path)folder.project_id=added.project_id;
  await readProjects();
  const project=(proj.list||[]).find(x=>x.project_id===added.project_id)||added;
  showInSidebar(project);
  if(state.view==='sessions'&&!conv.selected)renderHero();
  return {project,added:true};
}
async function pickSession(agent,entry){
  if(proj.adding||!proj.preview)return;
  if(entry.session_id){openContinued(entry.session_id);return;}
  const key=`${agent.agent_id}/${entry.agent_session_id}`;
  proj.adding=key;projectMessage('');holdPreview(key);
  let added=null;
  try{
    const result=await previewAsProject();if(result.added)added=result.project;
    await continueEarlier(result.project,agent,entry,true);
    if(entry.session_id&&projectDialog.open)projectDialog.close();
  }catch(e){
    const text=`${added?`已添加「${added.name}」，但`:''}没有接上这个会话：${projectProblem(e)}`;
    if(projectDialog.open)projectMessage(text,true);else toast(text);
  }finally{
    if(proj.adding===key){
      proj.adding=null;
      if(projectDialog.open){drawPreview();drawProjectFooter();[...document.querySelectorAll('#project-preview .preview-session')].find(r=>r.dataset.key===key)?.focus();}
    }
  }
}
// A session HarnessMesh already continues: open it.
async function openContinued(id){
  const known=previewProject(),find=()=>allSessions().find(s=>s.session_id===id&&!s._remote);
  if(!find())await loadSessions();
  const project=known&&(proj.list||[]).find(x=>x.project_id===known.project_id);
  if(project)showInSidebar(project);
  projectDialog.close();
  if(state.view!=='sessions')showView('sessions');
  selectSession(find()||{session_id:id,_remote:false});
}
$('#project-add').addEventListener('click',async()=>{
  if(proj.adding||!proj.preview)return;
  if(!previewProject()){proj.adding='folder';projectMessage('');holdPreview(null);}
  try{
    const {project,added}=await previewAsProject();
    projectDialog.close();revealInSidebar(project.project_id);
    if(added)toast(`已添加「${project.name}」`);
  }catch(e){if(projectDialog.open)projectMessage(`没有添加：${projectProblem(e)}`,true);}
  finally{if(proj.adding==='folder'){proj.adding=null;if(projectDialog.open){drawPreview();drawProjectFooter();}}}
});
$('#project-back').addEventListener('click',()=>{
  if(proj.step!=='preview'){projectDialog.close();return;}
  proj.preview=null;projectMessage('');showProjectStep('choose');focusChoose();
});
$('#project-pick').addEventListener('click',pickFolder);
$('#project-pick-cancel').addEventListener('click',stopPicking);
$('#project-path-form').addEventListener('submit',e=>{
  e.preventDefault();
  const path=$('#project-path').value.trim();
  if(!path){projectMessage('先填文件夹的完整路径',true);$('#project-path').focus();return;}
  if(!isFullPath(path)){projectMessage(projectProblem({message:'must be an absolute path'}),true);$('#project-path').focus();return;}
  stopPicking();previewFolder(path,{typed:true});
});
$('#project-path').addEventListener('input',()=>{if($('#project-message').classList.contains('error'))projectMessage('');});
projectDialog.addEventListener('close',()=>{stopPicking();proj.preview=null;proj.adding=null;proj.step='choose';$('#project-preview').replaceChildren();});

// The folders the agents have used: read only when asked for.
async function loadFolders(refresh){
  const read=Symbol();proj.folders={...proj.folders,state:'loading',read};proj.foldersRead=true;drawFolders();
  try{const data=unwrap(await api(`/history?refresh=${refresh}`,{signal:AbortSignal.timeout(120000)}));if(proj.folders.read===read)proj.folders={state:'ready',data};}
  catch(e){if(proj.folders.read===read)proj.folders={state:'error',error:e,data:proj.folders.data};}
  if(projectDialog.open)drawFolders();
}
function folderAdded(folder){return !!folder.project_id||(proj.list||[]).some(p=>p.path===folder.path);}
function drawFolders(){
  const box=$('#project-folders'),f=proj.folders,data=f?.data;
  $('#project-folders-refresh').disabled=f?.state==='loading';
  const rows=[];
  if(!data&&f?.state!=='error')rows.push(el('div','folder-empty loading','正在问各个 Agent 在哪些文件夹里有会话，可能要几秒…'));
  if(f?.state==='error')rows.push(el('div','folder-empty',`没查到：${projectProblem(f.error)}`));
  if(data){
    if(f.state==='loading')rows.push(el('div','folder-empty loading','正在重新查找…'));
    // Folders not yet added first; the daemon already lists the latest first.
    const folders=[...(data.folders||[])].sort((x,y)=>Number(folderAdded(x))-Number(folderAdded(y)));
    for(const folder of folders){
      const added=folderAdded(folder),row=el('button','folder-row');row.type='button';
      const text=el('span','folder-text'),meta=[`${folder.sessions} 个会话`];
      if(folder.latest)meta.push(`${since(folder.latest)}用过`);
      if(folder.agents?.length)meta.push(folder.agents.join('、'));
      text.append(el('b','',folder.name),el('span','folder-path',folder.path),el('span','folder-meta',meta.join(' · ')));
      row.append(text);if(added)row.append(folderFlag('已添加'));
      const go=icon('chevron-right','xs');go.classList.add('folder-chevron');row.append(go);
      row.addEventListener('click',()=>previewFolder(folder.path));
      rows.push(row);
    }
    if(!(data.folders||[]).length)rows.push(el('div','folder-empty','Agent 里还没有找到会话记录。'));
    const said=(kind,text)=>{const names=(data.agents||[]).filter(a=>a.state===kind).map(a=>a.agent_name);if(names.length)rows.push(el('div','folder-empty',`${names.join('、')} ${text}`));};
    said('unsupported','不提供会话记录');said('failed','这次没读出会话记录');
  }
  box.replaceChildren(...rows);
}
$('#project-history').addEventListener('toggle',()=>{
  if(!$('#project-history').open)return;
  if(!proj.foldersRead&&proj.folders?.state!=='loading')loadFolders(false);else drawFolders();
});
$('#project-folders-refresh').addEventListener('click',()=>loadFolders(true));

// ---- The project a new session starts in ----
function heroProjectOptions(target){
  if(!projectsSupported()||!target)return null;
  if(target.node.local){
    const list=(proj.list||[]).filter(p=>!p.hidden);if(!list.length)return null;
    const own=list.find(p=>(p.agent_ids||[]).includes(target.agent.agent_id));
    // Its own folder may be a hidden project: then it is offered by name.
    const options=list.map(p=>({value:p.project_id,name:p.name,title:p.path}));
    if(!own)options.unshift({value:'',name:'Agent 自己的目录'});
    return {options,fallback:own?.project_id??''};
  }
  // A paired computer's agent: its own folder, or a project that computer shares.
  const shared=target.node.projects||[];if(!shared.length)return null;
  return {options:[{value:'',name:'Agent 自己的目录'},...shared.map(p=>({value:p.project_id,name:p.name}))],fallback:''};
}
function heroProject(target){
  const o=heroProjectOptions(target);if(!o)return null;
  const pick=conv.heroProject,chosen=pick&&pick.node===target.node.node_id&&o.options.some(x=>x.value===pick.value)?pick.value:o.fallback;
  return {...o,chosen};
}
function renderHeroProject(target){
  const box=$('#hero-project'),pick=heroProject(target);
  box.hidden=!pick;if(!pick){box.replaceChildren();box.dataset.key='';return;}
  const key=JSON.stringify([target.node.node_id,target.agent.agent_id,pick.options,pick.chosen,canWrite()]);
  if(box.dataset.key===key)return;box.dataset.key=key;
  const pill=el('label','setting-pill project-pill'),select=document.createElement('select');
  select.setAttribute('aria-label','项目');select.disabled=!canWrite();
  for(const o of pick.options){const item=el('option','',o.name);item.value=o.value;if(o.title)item.title=o.title;item.selected=o.value===pick.chosen;select.append(item);}
  select.addEventListener('change',()=>{conv.heroProject={node:target.node.node_id,value:select.value};renderHero();});
  const chosen=pick.options.find(o=>o.value===pick.chosen);
  pill.title=`项目：${chosen?.name||''}${chosen?.title?`\n${chosen.title}`:''}`;
  pill.append(el('span','setting-label','项目'),select,icon('chevron-down','xs'));box.replaceChildren(pill);
}

$('#session-search').addEventListener('input',e=>{proj.query=e.target.value;renderSessionList();});
$('#session-search').addEventListener('keydown',e=>{if(e.key==='Escape'&&e.target.value){e.target.value='';proj.query='';renderSessionList();}});
$('#show-hidden').addEventListener('change',e=>{proj.showHidden=e.target.checked;renderSessionList();});
