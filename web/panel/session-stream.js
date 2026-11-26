// One conversation stream for every ACP agent. What is drawn follows the
// meaning ACP gives an update (a tool call's kind, arguments, diffs and
// terminal output), never which agent sent it. A turn is the person's
// message, the work the agent did, and its answer: the text after its last
// step. Work stays open while it runs and folds once the turn ends; failed
// steps stay in view.

const stepVerbs={read:'读取',edit:'编辑',delete:'删除',move:'移动',search:'搜索',execute:'运行',think:'思考',fetch:'获取',switch_mode:'切换模式',delegate:'调用',other:'调用'};
const stepIcons={read:'file-text',edit:'pencil',delete:'pencil',move:'pencil',search:'search',execute:'terminal',think:'lightbulb',fetch:'corner-down-right',switch_mode:'workflow',delegate:'bot',other:'wrench'};
// Common tool names, for calls whose kind says nothing.
const toolNameKinds={bash:'execute',shell:'execute',sh:'execute',zsh:'execute',exec:'execute',run:'execute',run_shell:'execute',run_command:'execute',shell_command:'execute',terminal:'execute',command:'execute',read:'read',read_file:'read',view:'read',view_file:'read',cat:'read',ls:'read',list:'read',list_dir:'read',list_files:'read',write:'edit',write_file:'edit',create_file:'edit',edit:'edit',edit_file:'edit',multiedit:'edit',str_replace:'edit',str_replace_editor:'edit',apply_patch:'edit',patch:'edit',delete:'delete',delete_file:'delete',remove:'delete',rm:'delete',move:'move',rename:'move',mv:'move',grep:'search',glob:'search',find:'search',search:'search',ripgrep:'search',rg:'search',codebase_search:'search',fetch:'fetch',web_fetch:'fetch',webfetch:'fetch',http_get:'fetch',think:'think'};
const pathKeys=['path','file_path','filePath','file','filename','target_file','notebook_path','absolute_path','abs_path','source'];
const editKeys=['content','new_string','newString','new_text','newText','edits','patch','diff','old_string','oldString'];
const stepStates={in_progress:'进行中',pending:'进行中',failed:'失败',cancelled:'已取消'};
const MINUS='−';

// ---- Events to turns ----
function streamModel(events){
  const turns=[],tools=new Map();let turn=null;
  const open=user=>{turn={key:user?`u${user.seq}`:`t${turns.length}`,user,items:[],ended:false,plan:null,planSeq:0,first:user?.created_at,last:user?.created_at};turns.push(turn);return turn;};
  for(const e of events){
    const p=e.payload||{};
    if(e.kind==='user_message'){open(e);continue;}
    if(!turn)open(null);
    turn.first??=e.created_at;turn.last=e.created_at;
    const lastItem=turn.items.at(-1);
    if(e.kind==='agent_message_chunk'||e.kind==='agent_thought_chunk'){
      const type=e.kind==='agent_message_chunk'?'text':'thought';
      if(lastItem?.type===type)lastItem.text+=p.text||'';
      else turn.items.push({type,key:`${type}${e.seq}`,text:p.text||''});
      continue;
    }
    if(e.kind==='tool_call'||e.kind==='tool_call_update'){
      const id=String(p.id??'');
      let state=tools.get(id);
      if(!state){state={id,version:0,turn:turn.key};tools.set(id,state);turn.items.push({type:'tool',key:`tool:${id}`,id});}
      mergeTool(state,p);
      continue;
    }
    if(e.kind==='plan'){
      if(!turn.plan)turn.items.push({type:'plan',key:'plan'});
      turn.plan=Array.isArray(p.entries)?p.entries:[];turn.planSeq=e.seq;
      continue;
    }
    if(e.kind==='turn_end')turn.ended=true;
    turn.items.push({type:'event',key:`event${e.seq}`,event:e});
  }
  return{turns,tools};
}
function mergeTool(state,p){
  state.version++;
  for(const k of ['title','kind','status','input','output'])if(p[k]!==undefined&&p[k]!==null)state[k]=p[k];
  if(p.args&&typeof p.args==='object')state.args=p.args;
  // Events recorded by earlier versions carry the arguments only as text.
  else if(!state.args&&typeof p.input==='string'){try{const v=JSON.parse(p.input);if(v&&typeof v==='object'&&!Array.isArray(v))state.args=v;}catch{}}
  if(Array.isArray(p.locations))state.locations=p.locations;
  if(Array.isArray(p.diffs)){state.diffs=p.diffs;state.diffsOmitted=p.diffs_omitted||0;}
  if(p.terminal&&typeof p.terminal==='object'){
    const t=state.terminal||(state.terminal={text:'',exit:null,signal:null,tail:'',skipped:0});
    if(typeof p.terminal.append==='string')t.text+=p.terminal.append;
    if(Number.isInteger(p.terminal.exit_code))t.exit=p.terminal.exit_code;
    if(typeof p.terminal.signal==='string'&&p.terminal.signal)t.signal=p.terminal.signal;
    // A tail after nothing was skipped simply continues the output.
    if(typeof p.terminal.tail==='string'){if(p.terminal.skipped>0){t.skipped=p.terminal.skipped;t.tail=p.terminal.tail;}else t.text+=p.terminal.tail;}
  }
}

// ---- What a step is ----
function stepArgs(t){return t.args&&typeof t.args==='object'&&!Array.isArray(t.args)?t.args:{};}
function argString(v){return typeof v==='string'?v.trim():'';}
function cleanTitle(title){return String(title||'').trim().replace(/^`+|`+$/g,'').replace(/\s+/g,' ');}
// A title that is only a tool's name says nothing to a person.
function rawToolName(t){const title=String(t.title||'').trim();if(!title||/\s/.test(title))return '';return title.split(/__|[.:/]/).filter(Boolean).pop().toLowerCase();}
function stepPath(t){
  const a=stepArgs(t);
  return argString(t.locations?.find(l=>l?.path)?.path)||pathKeys.map(k=>argString(a[k])).find(Boolean)||argString(t.diffs?.[0]?.path)||'';
}
function stepCommand(t){
  const a=stepArgs(t),c=a.command??a.cmd??a.script??a.commandLine;
  if(Array.isArray(c)){
    const words=c.filter(w=>typeof w==='string');
    // A shell's `-c` wraps the command a person would recognize.
    if(words.length>=3&&/(^|\/)[a-z]*sh$/.test(words[0])&&/^-[a-z]*c$/.test(words[1]))return words.slice(2).join(' ');
    return words.join(' ');
  }
  return argString(c);
}
function stepQuery(t){const a=stepArgs(t);return ['pattern','query','regex','glob','q','search_term'].map(k=>argString(a[k])).find(Boolean)||'';}
function stepUrl(t){const a=stepArgs(t);return [a.url,a.uri].map(argString).find(u=>/^https?:\/\//i.test(u))||'';}
function isDelegation(t){return /^mesh_(send|wait)$/.test(rawToolName(t))||/(^|[^a-z])mesh_(send|wait)\b/i.test(String(t.title||''));}
function stepKind(t){
  if(isDelegation(t))return 'delegate';
  if(t.kind&&t.kind!=='other'&&stepVerbs[t.kind])return t.kind;
  const named=toolNameKinds[rawToolName(t)];
  if(named)return named;
  if(stepCommand(t))return 'execute';
  if(t.diffs?.length)return 'edit';
  const a=stepArgs(t);
  if(stepPath(t)&&editKeys.some(k=>a[k]!==undefined))return 'edit';
  if(stepUrl(t))return 'fetch';
  if(stepQuery(t))return 'search';
  if(stepPath(t))return 'read';
  if(t.terminal)return 'execute';
  return 'other';
}
function splitPath(path){
  const parts=path.replace(/^file:\/\//,'').split(/[\\/]/).filter(Boolean);
  const name=parts.pop()||path;
  return{name,dir:parts.length>2?`…/${parts.slice(-2).join('/')}`:parts.join('/')};
}
// A path named in a title like "Edit notes.txt".
function titlePath(t){return cleanTitle(t.title).split(' ').reverse().find(w=>/[\w-]\.\w{1,8}$|\/\w/.test(w))||'';}
function firstLine(text){const line=String(text||'').split('\n').find(l=>l.trim())||'';return line.length>160?`${line.slice(0,160)}…`:line;}
// A one-line preview of Markdown, without its markers.
function plainLine(text){return firstLine(String(text||'').replace(/[`*_~#>]+/g,'')).trim();}

function stepLabel(t,kind=stepKind(t)){
  const label=el('span','step-label'),title=cleanTitle(t.title);
  const part=(cls,text)=>{if(label.childNodes.length)label.append(' ');label.append(el('span',cls,text));};
  const verb=text=>part('step-verb',text);
  const target=(text,cls='step-target')=>part(cls,text);
  const dim=text=>{if(text)part('step-dim',text);};
  if(['read','edit','delete','move'].includes(kind)){
    const path=stepPath(t)||titlePath(t);
    verb(stepVerbs[kind]);
    if(path){const{name,dir}=splitPath(path);target(name);dim(dir);}
    else if(title&&!rawToolName(t))dim(title);
    const files=new Set((t.diffs||[]).map(d=>d.path).filter(Boolean));
    if(files.size>1)dim(`等 ${files.size} 个文件`);
    if(kind==='move'){const to=argString(stepArgs(t).destination||stepArgs(t).new_path||stepArgs(t).to);if(to)dim(`→ ${splitPath(to).name}`);}
  }else if(kind==='execute'){
    verb('运行');
    const command=firstLine(stepCommand(t))||(!rawToolName(t)&&title.replace(/^(run|running|ran|execute|executing)\s+/i,''));
    if(command)target(command,'step-target code');else dim('命令');
  }else if(kind==='search'){
    verb('搜索');
    const query=stepQuery(t);
    if(query){target(firstLine(query),'step-target code');const where=argString(stepArgs(t).path);if(where&&!/^\.\/?$/.test(where))dim(`在 ${where}`);}
    else if(title&&!rawToolName(t))dim(title);
  }else if(kind==='fetch'){
    verb('获取');const url=stepUrl(t);
    if(url){try{const u=new URL(url);target(u.host);dim(u.pathname.length>1?u.pathname:'');}catch{target(url);}}
    else if(title&&!rawToolName(t))dim(title);
  }else if(kind==='think'){verb('思考');if(title&&!rawToolName(t)&&title.toLowerCase()!=='think')dim(title);}
  else if(kind==='switch_mode'){verb('切换模式');dim(argString(stepArgs(t).mode||stepArgs(t).mode_id));}
  else if(kind==='delegate'){
    const agent=argString(stepArgs(t).agent);
    verb(agent?'调用':'继续对话');
    target(agent||'Agent');
  }else if(rawToolName(t)||!title){verb('调用');target(rawToolName(t)?title:'工具','step-target code');}
  else target(title,'step-target plain');
  return label;
}
function diffStats(t){
  let add=0,del=0;
  for(const d of t.diffs||[]){const ops=diffOps(d.old,d.new);for(const op of ops){if(op.type==='add')add++;else if(op.type==='del')del++;}}
  return{add,del};
}
function stepMeta(t,kind){
  const meta=el('span','step-meta');
  if(t.diffs?.length){const{add,del}=diffStats(t);if(add||!del)meta.append(el('span','add',`+${add}`));if(del)meta.append(el('span','del',`${MINUS}${del}`));}
  if(kind==='execute'&&t.terminal){
    if(t.terminal.exit!==null&&t.terminal.exit!==0)meta.append(el('span','bad',`退出 ${t.terminal.exit}`));
    else if(t.terminal.signal)meta.append(el('span','bad',`信号 ${t.terminal.signal}`));
  }
  return meta.childElementCount?meta:null;
}

// ---- Diffs ----
function diffSplit(text){if(text===null||text===undefined)return [];const lines=String(text).split('\n');if(lines.at(-1)==='')lines.pop();return lines;}
function diffOps(oldText,newText){
  const a=diffSplit(oldText),b=diffSplit(newText);
  let start=0;while(start<a.length&&start<b.length&&a[start]===b[start])start++;
  let endA=a.length,endB=b.length;
  while(endA>start&&endB>start&&a[endA-1]===b[endB-1]){endA--;endB--;}
  const ops=a.slice(0,start).map(text=>({type:'ctx',text}));
  const x=a.slice(start,endA),y=b.slice(start,endB);
  if(x.length*y.length>250000){ops.push(...x.map(text=>({type:'del',text})),...y.map(text=>({type:'add',text})));}
  else{
    // Longest common subsequence over the changed middle.
    const w=y.length+1,table=new Uint32Array((x.length+1)*w);
    for(let i=x.length-1;i>=0;i--)for(let j=y.length-1;j>=0;j--)table[i*w+j]=x[i]===y[j]?table[(i+1)*w+j+1]+1:Math.max(table[(i+1)*w+j],table[i*w+j+1]);
    let i=0,j=0;
    while(i<x.length&&j<y.length){
      if(x[i]===y[j]){ops.push({type:'ctx',text:x[i]});i++;j++;}
      else if(table[(i+1)*w+j]>=table[i*w+j+1])ops.push({type:'del',text:x[i++]});
      else ops.push({type:'add',text:y[j++]});
    }
    while(i<x.length)ops.push({type:'del',text:x[i++]});
    while(j<y.length)ops.push({type:'add',text:y[j++]});
  }
  ops.push(...a.slice(endA).map(text=>({type:'ctx',text})));
  return ops;
}
// `named` shows the file's name; a step about one file already says it.
function diffView(d,{named=true,maxLines=400}={}){
  const box=el('div','diff'),ops=diffOps(d.old,d.new);
  const created=d.old===null||d.old===undefined;
  if(named||created||d.truncated){
    const head=el('div','diff-file');
    if(named){const{name,dir}=splitPath(d.path||'');head.append(el('span','diff-name',name));if(dir)head.append(el('span','step-dim',dir));}
    if(created)head.append(el('span','diff-tag','新文件'));
    if(d.truncated)head.append(el('span','diff-tag','内容过长，只显示首尾'));
    box.append(head);
  }
  const lines=el('div','diff-lines'),near=new Set();
  ops.forEach((op,i)=>{if(op.type!=='ctx')for(let k=i-3;k<=i+3;k++)near.add(k);});
  let shown=0,gap=0;
  const flushGap=()=>{if(gap){lines.append(el('div','diff-gap',`⋯ ${gap} 行未改动`));gap=0;}};
  ops.forEach((op,i)=>{
    if(op.type==='ctx'&&!near.has(i)){gap++;return;}
    flushGap();
    if(shown++>=maxLines)return;
    const line=el('div',`diff-line ${op.type}`);
    line.append(el('span','diff-sign',op.type==='add'?'+':op.type==='del'?MINUS:' '),el('span','diff-text',op.text||' '));
    lines.append(line);
  });
  flushGap();
  if(shown>maxLines)lines.append(el('div','diff-gap',`还有 ${shown-maxLines} 行没有显示`));
  box.append(lines);
  return box;
}

// ---- Output ----
// Output some agents wrap in a code fence, like ```console.
function stripFence(text){const m=String(text||'').match(/^\s*(`{3,})[\w-]*\n([\s\S]*?)\n?\1\s*$/);return m?m[2]:String(text||'');}
function formatBytes(n){return n>=1048576?`${(n/1048576).toFixed(1)} MB`:n>=1024?`${Math.round(n/1024)} KB`:`${n} 字节`;}
function terminalView(t,live){
  const box=el('div','term'),pre=el('pre','term-out'),term=t.terminal;
  const text=term?.text||stripFence(t.output||'');
  pre.append(document.createTextNode(text));
  if(term?.skipped){pre.append(el('span','term-skip',`${text.endsWith('\n')?'':'\n'}… 中间省略 ${formatBytes(term.skipped)} …\n`),document.createTextNode(term.tail));}
  if(!text&&!term?.skipped)pre.append(el('span','term-skip',live?'等待输出…':'没有输出'));
  box.append(pre);
  if(term&&(term.exit!==null||term.signal))box.append(el('div',`term-exit ${term.exit===0?'ok':'bad'}`,term.signal?`被信号 ${term.signal} 结束`:`退出码 ${term.exit}`));
  pre.dataset.scroll='term';
  return box;
}
// Arguments of a step nothing else describes, as "name: value" lines.
function argumentList(args){
  const list=el('div','step-args');let count=0;
  const add=(key,value,depth)=>{
    if(count>=40)return;
    if(value&&typeof value==='object'&&depth<2){for(const [k,v] of Object.entries(value))add(key?`${key}.${k}`:k,v,depth+1);return;}
    const row=el('div','step-arg');count++;
    row.append(el('span','step-arg-key',key||'参数'),el('span','step-arg-value',typeof value==='string'?value:Array.isArray(value)?value.map(v=>typeof v==='string'?v:String(v)).join(', '):String(value)));
    list.append(row);
  };
  add('',args,0);
  return count?list:null;
}
// What another agent answered: the mesh tools reply with a summary of the
// session, whose final text is the answer.
function delegationReply(output){
  if(!output)return null;
  const raw=stripFence(output);
  let body=null;
  try{body=JSON.parse(raw);}catch{}
  if(!body&&raw.trimStart().startsWith('{')){
    // A long reply is recorded with its middle cut out, which leaves the
    // summary unreadable as a whole; its answer can still be read.
    const m=raw.match(/"final_text"\s*:\s*("(?:[^"\\]|\\.)*")/);
    // The cut marker brings raw line breaks into the string; escape them first.
    if(m){try{return{text:JSON.parse(m[1].replace(/[\u0000-\u001f]/g,c=>`\\u${c.charCodeAt(0).toString(16).padStart(4,'0')}`))};}catch{}}
    return{note:'对方的回答太长，这里没有完整记下'};
  }
  if(!body||typeof body!=='object'||Array.isArray(body))return{text:raw};
  if(typeof body.final_text==='string'&&body.final_text)return{text:body.final_text};
  const error=body.error?.message||body.error;
  if(typeof error==='string'&&error)return{note:error,bad:true};
  if(body.done===false)return{note:'对方还在处理，稍后会有结果'};
  return{note:'对方没有给出回答'};
}
function stepBody(t,kind,live){
  const parts=[];
  if(kind==='delegate'){
    const ask=argString(stepArgs(t).message);
    if(ask)parts.push(el('div','delegate-ask',ask));
    const reply=delegationReply(t.output);
    if(reply?.text){const md=renderMarkdown(reply.text);md.classList.add('delegate-reply');parts.push(md);}
    else if(reply?.note)parts.push(el('div',`delegate-note ${reply.bad?'bad':''}`.trim(),reply.note));
    return parts;
  }
  const files=new Set((t.diffs||[]).map(d=>d.path));
  for(const d of (t.diffs||[]))parts.push(diffView(d,{named:files.size>1}));
  if(t.diffsOmitted)parts.push(el('div','diff-gap',`还有 ${t.diffsOmitted} 个文件的改动没有显示`));
  // A command not started yet (or waiting for permission) has nothing to show.
  if(kind==='execute'&&(hasOutput(t)||(live&&t.status==='in_progress')))parts.push(terminalView(t,live));
  else if(t.output&&!(t.diffs?.length&&kind==='edit'))parts.push(el('pre','step-output',stripFence(t.output)));
  if(!parts.length&&kind==='other'){
    let args=t.args;
    if(!args&&t.input){try{args=JSON.parse(t.input);}catch{args=t.input;}}
    const list=args&&typeof args==='object'?argumentList(args):args?el('pre','step-output',String(args)):null;
    if(list)parts.push(list);
  }
  return parts;
}

// ---- Building and patching ----
// A rebuilt step keeps where its output was scrolled to; output read to its
// end follows new lines, and new output opens at its end.
// Read before the old node leaves the page, where it has no scroll position.
function scrollMemo(old){
  const before=new Map();
  if(old)for(const n of old.querySelectorAll('[data-scroll]'))before.set(n.dataset.scroll,{top:n.scrollTop,end:n.scrollHeight-n.scrollTop-n.clientHeight<8});
  return before;
}
function carryScroll(before,fresh){
  for(const n of fresh.querySelectorAll('[data-scroll]')){
    const was=before.get(n.dataset.scroll);
    if(was&&!was.end)n.scrollTop=was.top;
    else queueMicrotask(()=>{n.scrollTop=n.scrollHeight;});
  }
}
// Each child carries a key; a child whose signature changed is rebuilt, the
// rest stay as they are, so open folds and scroll positions survive.
function patchChildren(parent,specs){
  const byKey=new Map();
  for(const node of [...parent.children]){if(node.dataset.key)byKey.set(node.dataset.key,node);else node.remove();}
  specs.forEach((spec,index)=>{
    let node=byKey.get(spec.key);
    if(!node||(!spec.update&&node.dataset.sig!==spec.sig)){
      const fresh=spec.make();fresh.dataset.key=spec.key;if(spec.sig!==undefined)fresh.dataset.sig=spec.sig;
      const before=scrollMemo(node);
      if(node)node.replaceWith(fresh);
      carryScroll(before,fresh);
      node=fresh;
    }
    byKey.delete(spec.key);
    spec.update?.(node);
    if(spec.open!==undefined&&node.open!==spec.open)node.open=spec.open;
    if(parent.children[index]!==node)parent.insertBefore(node,parent.children[index]||null);
  });
  for(const node of byKey.values())node.remove();
}
// A fold whose open state the person chose is kept as they left it.
function remembered(details,open,key){
  const summary=details.querySelector(':scope > summary');
  summary?.addEventListener('click',()=>open.set(key,!details.open));
}
function stepState(t,live,waiting){
  if(waiting)return{cls:'waiting',text:'等待授权'};
  const status=t.status||'pending';
  if((status==='pending'||status==='in_progress')&&!live)return null;
  return stepStates[status]?{cls:status,text:stepStates[status]}:null;
}
function stepSpec(t,ctx,prefix=''){
  const kind=stepKind(t),live=ctx.live&&ctx.liveTurn===t.turn,waiting=ctx.waiting.has(t.id);
  const state=stepState(t,live,waiting),key=`step:${t.id}`;
  const running=live&&['pending','in_progress'].includes(t.status||'pending');
  const sig=`${t.version}|${state?.cls||''}|${live}`;
  const make=()=>{
    const body=stepBody(t,kind,live);
    const node=el(body.length?'details':'div',`step kind-${kind} ${state?.cls||''}`.trim());
    if(kind==='delegate')node.classList.add('delegate');
    node.dataset.tool=t.id;
    const head=el(body.length?'summary':'div','step-head');
    head.append(icon(stepIcons[kind]||'wrench','sm'),stepLabel(t,kind));
    const meta=stepMeta(t,kind);if(meta)head.append(meta);
    if(state)head.append(el('span',`step-state ${state.cls}`,state.text));
    if(t.title)head.title=cleanTitle(t.title);
    if(body.length){head.append(icon('chevron-down','sm chevron'));const box=el('div','step-body');box.append(...body);node.append(head,box);remembered(node,ctx.open,key);}
    else node.append(head);
    return node;
  };
  const chosen=ctx.open.get(key);
  const open=chosen??(running||t.status==='failed');
  return{key:prefix+key,sig,make,open:stepHasBody(t,kind)?open:undefined};
}
function hasOutput(t){return !!(t.output||t.terminal?.text||t.terminal?.skipped||t.terminal?.exit!==null&&t.terminal?.exit!==undefined);}
function stepHasBody(t,kind){return kind==='delegate'||kind==='execute'&&(hasOutput(t)||t.status==='in_progress')||!!t.diffs?.length||!!t.output||(kind==='other'&&!!(t.args||t.input));}
function textSpec(item,cls){
  return{key:item.key,sig:String(item.text.length),make:()=>{const m=el('div',`msg agent ${cls}`),body=renderMarkdown(item.text);body.classList.add('msg-text');m.append(body);return m;}};
}
function thoughtSpec(item,ctx){
  const key=`thought:${item.key}`;
  return{key:item.key,sig:String(item.text.length),open:ctx.open.get(key)??false,make:()=>{
    const box=el('details','thought'),sum=el('summary');
    sum.append(icon('lightbulb','sm'),el('span','','思考'),el('span','thought-preview',plainLine(item.text)));
    const body=renderMarkdown(item.text);body.classList.add('msg-text');
    box.append(sum,body);remembered(box,ctx.open,key);return box;
  }};
}
function planSpec(turn){
  const entries=turn.plan||[],done=entries.filter(e=>e.status==='completed').length;
  return{key:'plan',sig:String(turn.planSeq),make:()=>{
    const box=el('div','plan'),head=el('div','plan-head');
    head.append(icon('list-todo','sm'),el('span','','计划'),el('span','plan-count',`${done}/${entries.length}`));
    box.append(head);
    for(const entry of entries){
      const row=el('div',`plan-entry ${entry.status||'pending'}`);
      row.append(icon(entry.status==='completed'?'check':'circle','sm'),el('span','',String(entry.content||'')));
      box.append(row);
    }
    return box;
  }};
}
// An event that is neither text nor a step becomes zero or more notes.
function noteSpecs(item,ctx){
  return [].concat(ctx.note(item.event)||[]).map((note,i)=>({key:`${item.key}:${i}`,sig:`${note.tone||''}|${note.text}`,make:()=>{
    const n=el('div',`event-note ${note.tone||''}`.trim());
    if(note.icon)n.append(icon(note.icon,'sm'));
    n.append(el('span','',note.text));return n;
  }}));
}
function workSummary(steps,turn,live){
  const reads=new Set(),edits=new Set();let commands=0,searches=0,fetches=0,agents=0,others=0,failed=0;
  for(const t of steps){
    const kind=stepKind(t);
    if(t.status==='failed')failed++;
    if(kind==='read')reads.add(stepPath(t)||t.id);
    else if(['edit','delete','move'].includes(kind))edits.add(stepPath(t)||t.id);
    else if(kind==='execute')commands++;
    else if(kind==='search')searches++;
    else if(kind==='fetch')fetches++;
    else if(kind==='delegate')agents++;
    else if(kind!=='think')others++;
  }
  const parts=[
    reads.size&&`读取 ${reads.size} 个文件`,edits.size&&`编辑 ${edits.size} 个文件`,commands&&`运行 ${commands} 条命令`,
    searches&&`搜索 ${searches} 次`,fetches&&`获取 ${fetches} 个网页`,agents&&`调用 ${agents} 次 Agent`,others&&`其他 ${others} 步`,
  ].filter(Boolean);
  const seconds=turn.first&&turn.last?Math.max(0,turn.last-turn.first):0;
  const sum=el('summary','work-summary');
  sum.append(el('span','work-title',live?'正在工作':'工作过程'));
  if(parts.length)sum.append(el('span','work-parts',parts.join(' · ')));
  if(failed)sum.append(el('span','work-bad',`${failed} 步失败`));
  if(!live&&seconds>=1)sum.append(el('span','work-time',seconds>=60?`用时 ${Math.floor(seconds/60)} 分 ${seconds%60} 秒`:`用时 ${seconds} 秒`));
  sum.append(icon('chevron-down','sm chevron'));
  return{node:sum,sig:`${live}|${parts.join()}|${failed}|${live?0:seconds}`};
}
function turnSpecs(turn,ctx){
  const specs=[];
  if(turn.user)specs.push(ctx.user(turn.user));
  const live=ctx.live&&ctx.liveTurn===turn.key;
  // The answer is the text after the turn's last step.
  let lastStep=-1;
  turn.items.forEach((item,i)=>{if(item.type!=='text'&&item.type!=='event')lastStep=i;});
  const work=turn.items.slice(0,lastStep+1),after=turn.items.slice(lastStep+1);
  if(work.length){
    const steps=work.filter(i=>i.type==='tool').map(i=>ctx.tools.get(i.id));
    const failed=steps.filter(t=>t.status==='failed');
    const summary=workSummary(steps,turn,live),key=`work:${turn.key}`;
    const open=ctx.open.get(key)??live;
    specs.push({key:'work',open,make:()=>el('details','work'),update:node=>{
      const children=[{key:'summary',sig:summary.sig,make:()=>{const s=summary.node;s.addEventListener('click',()=>ctx.open.set(key,!node.open));return s;}}];
      for(const item of work){
        if(item.type==='event')children.push(...noteSpecs(item,ctx));
        else children.push(item.type==='tool'?stepSpec(ctx.tools.get(item.id),ctx):item.type==='text'?textSpec(item,'narration'):item.type==='thought'?thoughtSpec(item,ctx):planSpec(turn));
      }
      patchChildren(node,children);
    }});
    if(failed.length&&!live)specs.push({key:'failed',make:()=>el('div','work-failed'),update:node=>patchChildren(node,failed.map(t=>stepSpec(t,ctx,'failed:')))});
  }
  const answer=after.filter(i=>i.type==='text');
  if(answer.length){
    const text=answer.map(i=>i.text).join('');
    specs.push({key:'answer',sig:String(text.length),make:()=>{const m=el('div','msg agent answer'),body=renderMarkdown(text);body.classList.add('msg-text');m.append(body);return m;}});
  }
  for(const item of after.filter(i=>i.type==='event'))specs.push(...noteSpecs(item,ctx));
  return specs;
}
// Draws `model` into `stream`, reusing what is already there.
// ctx: {key, live, open, waiting, note(event), user(event), head:[specs], tail:[specs]}
function renderStream(stream,model,ctx){
  let col=stream.querySelector(':scope > .chat-col');
  if(!col||col.dataset.session!==ctx.key){col=el('div','chat-col');col.dataset.session=ctx.key;stream.replaceChildren(col);}
  const last=model.turns.at(-1);
  const full={...ctx,tools:model.tools,liveTurn:ctx.live?last?.key:null};
  const specs=[...ctx.head];
  for(const turn of model.turns){
    const inner=turnSpecs(turn,full);
    if(!inner.length)continue;
    specs.push({key:`turn:${turn.key}`,make:()=>el('section','turn'),update:node=>patchChildren(node,inner)});
  }
  specs.push(...ctx.tail);
  patchChildren(col,specs);
}
// The step a permission request is about, for its card.
function permissionStep(t){
  const box=el('div','permission-step');
  if(!t)return box;
  const kind=stepKind(t),line=el('div','permission-line');
  line.append(icon(stepIcons[kind]||'wrench','sm'),stepLabel(t,kind));
  box.append(line);
  if(t.diffs?.length)box.append(diffView(t.diffs[0],{named:false,maxLines:40}));
  else if(kind==='execute'){const command=stepCommand(t);if(command&&command!==firstLine(command))box.append(el('pre','step-output',command));}
  return box;
}
