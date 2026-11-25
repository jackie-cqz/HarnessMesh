'use strict';

// The catalog belongs to a single live session. Every native action is checked
// a second time by the executor against its current ACP announcement.
let commandMenu=null;

function sessionCommandsSupported(){
  return state.runtime?.capabilities?.session_commands===true;
}

function parseCommandDraft(draft){
  const match=/^\/([^\s]*)([\s\S]*)$/.exec(draft.trimStart());
  return match&&{name:match[1],hasSeparator:match[2].length>0,args:match[2].trim()};
}

function modelAlias(catalog){
  if(!['ready','unadvertised'].includes(catalog.state))return null;
  const name={pi:'model',opencode:'models',dsh:'model'}[catalog.harness];
  const option=catalog.config_options?.find(o=>(o.category==='model'||o.id==='model')&&o.kind==='select'&&o.choices?.length);
  if(!name||!option||catalog.commands.some(command=>command.name===name))return null;
  return {name,description:catalog.harness==='dsh'?'面板快捷方式 · 选择 DSH 会话模型':'查看或切换当前会话的模型',input_hint:'模型名称',option};
}

function commandChoices(catalog,draft){
  const alias=modelAlias(catalog);
  if(alias&&draft.name===alias.name&&draft.hasSeparator){
    const query=draft.args.toLowerCase();
    return alias.option.choices.filter(choice=>[choice.name,choice.value,choice.group].some(v=>v?.toLowerCase().includes(query)))
      .map(choice=>({name:choice.name,description:choice.value,choice,alias,current:alias.option.current===choice.value}));
  }
  if(draft.hasSeparator)return [];
  const commands=alias?[alias,...catalog.commands]:catalog.commands;
  return commands.filter(command=>command.name.toLowerCase().startsWith(draft.name.toLowerCase()));
}

function dismissCommands(){
  commandMenu=null;
  const input=$('#composer-input');
  input.removeAttribute('role');input.removeAttribute('aria-controls');input.removeAttribute('aria-activedescendant');
  input.removeAttribute('aria-autocomplete');input.removeAttribute('aria-expanded');
  $('#command-menu').hidden=true;$('#command-options').replaceChildren();
}

function resetSessionCommands(){
  dismissCommands();
  if(parseCommandDraft($('#composer-input').value))$('#composer-input').value='';
}

function openSessionCommands(){
  const input=$('#composer-input'),record=conv.record;
  if(!record||input.disabled||state.view!=='sessions'||!parseCommandDraft(input.value)){dismissCommands();return;}
  if(commandMenu?.id===record.session_id){commandMenu.selected=0;drawCommands();return;}
  commandMenu={id:record.session_id,status:record.status,selected:0,catalog:null,error:'',loading:false,refresh:false};
  input.setAttribute('role','combobox');input.setAttribute('aria-controls','command-options');
  input.setAttribute('aria-autocomplete','list');input.setAttribute('aria-expanded','true');
  $('#command-menu').hidden=false;
  if(sessionCommandsSupported())readCommands(record.status==='idle');
  else{commandMenu.error='当前 Daemon 不提供会话命令。';drawCommands();}
}

async function readCommands(initialize=false){
  const menu=commandMenu;if(!menu)return;
  if(menu.loading){menu.refresh=true;return;}
  menu.loading=true;menu.error='';drawCommands();
  try{
    const reply=unwrap(await api(`/sessions/${menu.id}/commands`,initialize?{method:'POST',body:'{}',signal:AbortSignal.timeout(45000)}:{}));
    if(commandMenu!==menu||reply.session_id!==menu.id)return;
    menu.catalog=reply;
  }catch(error){
    if(commandMenu!==menu)return;
    menu.catalog=null;menu.error=`读取命令失败：${error.message||error.code}。关闭命令列表后可重新打开。`;
  }finally{
    if(commandMenu===menu){
      menu.loading=false;
      if(menu.refresh){menu.refresh=false;readCommands(menu.status==='idle'&&menu.catalog?.state==='pending_approval');}
      else drawCommands();
    }
  }
}

function refreshOpenCommands(){
  if(commandMenu&&sessionCommandsSupported())readCommands();
}

function updateOpenCommandStatus(){
  const menu=commandMenu;if(!menu)return;
  const status=conv.record?.status;
  if(conv.selected!==menu.id||!status||status==='closed'){dismissCommands();return;}
  if(status!==menu.status){
    menu.status=status;
    if(status==='idle'&&sessionCommandsSupported()){readCommands(true);return;}
  }
  drawCommands();
}

function drawCommands(){
  const menu=commandMenu;
  if(!menu)return;
  const input=$('#composer-input'),draft=parseCommandDraft(input.value);
  if(!draft){dismissCommands();return;}
  const catalog=menu.catalog,choices=catalog?commandChoices(catalog,draft):[];
  menu.selected=Math.min(menu.selected,Math.max(0,choices.length-1));
  const list=$('#command-options'),alias=catalog&&modelAlias(catalog),models=alias&&draft.name===alias.name&&draft.hasSeparator;
  list.setAttribute('aria-label',models?'可选模型':'当前会话的命令');
  // Background events can re-render the conversation during keyboard and touch
  // navigation. Keep the same option nodes and scroll position when possible.
  if(menu.renderedCatalog!==catalog||menu.renderedDraft!==input.value){
    menu.renderedCatalog=catalog;menu.renderedDraft=input.value;
    list.replaceChildren(...choices.map((entry,i)=>{
      const row=el('div','command-option');row.id=`slash-option-${i}`;row.setAttribute('role','option');
      const label=el('span','command-name',entry.choice?entry.name:`/${entry.name}`);
      if(entry.current)label.append(icon('check','xs'));
      row.append(label,el('span','command-description',entry.description));
      if(entry.input_hint)row.append(el('span','command-argument',entry.input_hint));
      row.addEventListener('mousedown',event=>event.preventDefault());
      row.addEventListener('click',()=>{if(commandMenu===menu)chooseCommand(entry);});
      return row;
    }));
    if(document.activeElement===input&&matchMedia('(max-width:700px)').matches){
      input.scrollIntoView({block:'nearest'});
    }
  }
  for(const [i,row]of Array.from(list.children).entries())row.setAttribute('aria-selected',String(i===menu.selected));
  if(choices.length)input.setAttribute('aria-activedescendant',list.children[menu.selected].id);
  else input.removeAttribute('aria-activedescendant');
  const exact=catalog?.commands.find(command=>command.name===draft.name);
  const help=$('#command-help');
  if(menu.status==='pending_approval')help.textContent='等待执行端批准，批准后会显示可用命令。';
  else if(menu.error)help.textContent=menu.error;
  else if(!catalog)help.textContent='正在读取这个 Agent 的命令…';
  else if(catalog.state==='inactive')help.textContent='Agent 尚未启动，重新打开命令列表后可再试。';
  else if(menu.status!=='idle')help.textContent='这一轮结束后可以使用命令。';
  else if(choices.length)help.textContent=models?'选择模型以切换当前会话。':'↑ ↓ 选择 · Tab 补全 · Enter 确认';
  else if(exact&&draft.hasSeparator)help.textContent=[`/${exact.name}`,exact.input_hint,'Enter 执行'].filter(Boolean).join(' · ');
  else if(models)help.textContent='没有匹配的模型。';
  else if(catalog.harness==='dsh'&&!alias&&!catalog.commands.length&&!draft.name)help.textContent='DSH 没有提供可选模型。可在 DSH 的「设置 → 模型」配置后打开新会话。';
  else if(draft.name)help.textContent=`此会话没有可用的 /${draft.name} 命令。`;
  else help.textContent=catalog.state==='unadvertised'?'Agent 尚未公布命令。':'此会话没有可用命令。';
}

function insertCommand(entry){
  const input=$('#composer-input');
  input.value=`/${entry.name} `;commandMenu.selected=0;input.focus();drawCommands();
}

function chooseCommand(entry){
  if(!commandMenu||commandMenu.status!=='idle'||conv.busy||conv.settingBusy)return;
  if(entry.choice){applyCommandModel(entry.alias,entry.choice);return;}
  insertCommand(entry);
}

async function applyCommandModel(alias,choice){
  const menu=commandMenu,input=$('#composer-input'),draft=input.value;
  if(!menu||conv.settingBusy||conv.busy||conv.record?.status!=='idle')return;
  if(conv.record.origin?.node_id!==localNodeId()){toast('模型设置由打开这个会话的主人修改。');return;}
  const ok=await changeSetting(alias.option.id,choice.value);
  if(ok&&menu===commandMenu&&input.value===draft){input.value='';dismissCommands();input.focus();}
}

async function submitSessionCommand(){
  const input=$('#composer-input'),draft=input.value,parsed=parseCommandDraft(draft);
  if(!parsed||conv.record?.status!=='idle'||conv.busy||conv.settingBusy||!canWrite())return;
  if(!commandMenu){openSessionCommands();return;}
  const menu=commandMenu,catalog=menu.catalog,generation=conv.generation;
  if(!catalog||menu.error){drawCommands();return;}
  const alias=modelAlias(catalog);
  if(alias?.name===parsed.name){
    const choice=alias.option.choices.find(c=>c.value===parsed.args);
    if(choice)return applyCommandModel(alias,choice);
    if(!parsed.hasSeparator)insertCommand(alias);
    else drawCommands();
    return;
  }
  if(!catalog.commands.some(c=>c.name===parsed.name)){drawCommands();return;}
  conv.busy=true;renderConversation();
  try{
    const record=unwrap(await api(`/sessions/${menu.id}/command`,{method:'POST',body:JSON.stringify({name:parsed.name,arguments:parsed.args})}));
    if(conv.selected===menu.id&&conv.generation===generation){
      if(input.value===draft){input.value='';dismissCommands();}
      conv.record={...conv.record,...record};
    }
  }catch(error){
    if(conv.selected===menu.id&&conv.generation===generation){
      toast(`命令请求失败：${error.message||error.code}`);
      if(commandMenu===menu)readCommands();
    }
  }finally{conv.busy=false;renderConversation();loadSessions();}
}

const commandInput=$('#composer-input');
commandInput.addEventListener('input',e=>{if(!e.isComposing)openSessionCommands();});
commandInput.addEventListener('compositionend',openSessionCommands);
commandInput.addEventListener('focus',openSessionCommands);
commandInput.addEventListener('keydown',event=>{
  const menu=commandMenu;
  if(event.isComposing||event.keyCode===229||!menu||event.ctrlKey||event.metaKey||event.altKey)return;
  if(event.key==='Escape'){event.preventDefault();dismissCommands();return;}
  const draft=parseCommandDraft(commandInput.value);
  if(!draft||!menu.catalog)return;
  const choices=commandChoices(menu.catalog,draft);
  if(choices.length&&['ArrowUp','ArrowDown'].includes(event.key)){
    event.preventDefault();menu.selected=(menu.selected+(event.key==='ArrowDown'?1:-1)+choices.length)%choices.length;
    drawCommands();$('#command-options').children[menu.selected].scrollIntoView({block:'nearest'});return;
  }
  if(event.shiftKey||!['Tab','Enter'].includes(event.key))return;
  if(event.key==='Enter'&&draft.hasSeparator&&(!choices.length||choices[menu.selected]?.choice?.value===draft.args)){
    event.preventDefault();submitSessionCommand();return;
  }
  if(choices.length){event.preventDefault();chooseCommand(choices[menu.selected]);}
});
document.addEventListener('pointerdown',event=>{
  if(commandMenu&&!event.target.closest('#composer'))dismissCommands();
});
$('#composer').addEventListener('focusout',event=>{
  if(commandMenu&&event.relatedTarget&&!event.currentTarget.contains(event.relatedTarget))dismissCommands();
});
