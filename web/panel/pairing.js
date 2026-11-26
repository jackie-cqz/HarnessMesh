'use strict';
// Pair this computer with another one from the panel. One side pastes the
// other's pairing code and sends a request; the other side approves it once,
// typing the six-character check code the first side shows. The daemon that
// sent the request finishes pairing by itself when the approval arrives.
const pairingState={sent:null,waiting:new Set(),dismissed:new Set(),typed:new Map(),messages:new Map(),busy:false,nearby:[],nearbyLooked:false,nearbyError:'',chosen:null,passcodeMessage:'',passcodeError:false};
const pairProblems={
 INVALID_PAIR_CODE:'这不是配对码。配对码以 hmesh:// 开头，在对方面板的「配对新电脑」里复制。',
 PEER_UNREACHABLE:'连不上这台电脑：确认它开着 HarnessMesh，并且两台电脑在同一个 Tailnet 里。',
 UNAUTHENTICATED:'对方的身份和配对码对不上。请重新复制对方的配对码。',
 RATE_LIMITED:'对方刚收到太多请求，过一分钟再试。',
 INVALID_INPUT:'对方没有接受这个申请。如果粘贴的是这台电脑自己的配对码，请换成对方的。',
 PAIR_CHECK_MISMATCH:'校验码不对。请看对方面板上显示的 6 位校验码。',
 PAIR_REQUEST_CLOSED:'这个申请已经处理过，或者已经过期。',
 PERMISSION_DENIED:'没能批准：申请已经处理过或过期，或者对方的证书和之前配对时不一样（要先撤销旧的配对）。',
 PAIR_CHECK_AMBIGUOUS:'有不止一条申请对得上这个校验码，其中可能有冒充的。请拒绝不认识的申请，或请对方重新申请后再批准。',
 TASK_CONFLICT:'这个申请已经处理过了。',
 PANEL_READ_ONLY:'面板现在是只读的，不能配对。',
};
function pairProblem(error){return pairProblems[error?.code]||`没有完成（${error?.code||error?.message||'原因未知'}）。`;}
// The last six hex digits of a certificate fingerprint, as the check code.
function pairCheck(fingerprint){return String(fingerprint||'').replace(/:/g,'').slice(-6).toUpperCase();}
function pairingReady(){return state.runtime?.capabilities?.pairing===true;}
function pairOpen(p,now=Date.now()/1000){return p.status==='pending'&&p.expires_at>now;}
function pairLeft(p){const s=Math.max(0,Math.round(p.expires_at-Date.now()/1000));return s<60?'不到 1 分钟后过期':`${Math.ceil(s/60)} 分钟内有效`;}
function pairHost(p){try{return new URL(p.card.endpoint).host;}catch{return p.card.endpoint||'';}}
function ownPairCheck(){return pairCheck(data('node',{}).node?.fingerprint);}
// A code to send the same request again, rebuilt from the other computer's card.
function pairCodeOf(p){return `hmesh://${pairHost(p)}/${String(p.card.fingerprint||'').replace(/:/g,'').toLowerCase()}`;}

// Tell the person when a request sent from here finishes on its own.
function noticePairs(pairs){
 for(const p of pairs){
  if(p.direction!=='outgoing')continue;
  if(p.status==='pending'){pairingState.waiting.add(p.request_id);continue;}
  if(!pairingState.waiting.delete(p.request_id))continue;
  // The dialog says it when it is still open; otherwise a short note.
  if(pairingState.sent?.request_id===p.request_id&&$('#pair-dialog').open){pairingState.sent=p;drawPairDialog();continue;}
  if(p.status==='approved')toast(`已和 ${p.card.display_name} 配对，可以查看对方授权的 Agent。`);
 }
}

// Listening on every interface: the port is open only to networks this
// computer trusts. On one it has not trusted yet, say so and offer to trust it.
const networkState={busy:false,message:''};
function networksReady(){return state.runtime?.capabilities?.trusted_networks===true;}
function networkLabel(local){
 if(local.network==='tailscale')return 'Tailnet';
 if(local.network!=='direct')return '本机回环';
 return local.networks?.every_interface&&(local.networks.trusted||[]).some(t=>t.tailnet)?'局域网直连 · Tailscale':'局域网直连';
}
function networkNotice(local){
 const networks=local.networks;if(!networksReady()||!networks?.every_interface)return null;
 const box=el('div','pair-network'),suggested=networks.suggested;
 if(suggested){
  box.classList.add('untrusted');
  const trust=button(networkState.busy?'正在信任…':'信任这个网络','button small',()=>trustNetwork(suggested.subnet));
  trust.type='button';trust.id='trust-network';trust.disabled=networkState.busy||!canWrite();
  box.append(el('p','pair-network-text','这台电脑现在的网络还没认过：别的电脑连不进来。'),el('span','pair-network-detail mono',`${suggested.subnet} · ${suggested.interface}`),trust);
  // No gateway hardware address seen here: trusting it would go by the subnet alone.
  if(suggested.subnet_only)box.append(el('p','pair-network-note muted','读不到这个网络路由器的硬件地址：信任后只按网段认，同网段的别的网络也会被当成认过。'));
 }else{
  const current=(networks.current||[]).filter(n=>n.trusted&&!n.tailnet);
  const subnetOnly=current.find(n=>n.subnet_only),unverified=current.find(n=>n.gateway_unverified);
  if(subnetOnly)box.append(el('p','pair-network-text muted',`${subnetOnly.subnet} 只按网段认：同网段的别的网络也会被当成认过。`));
  else if(unverified)box.append(el('p','pair-network-text muted',`${unverified.subnet} 的路由器现在核对不了，暂时只按网段认。`));
  else return null;
 }
 if(networkState.message){const message=el('p','form-note pair-message',networkState.message);message.setAttribute('role','status');box.append(message);}
 return box;
}
async function trustNetwork(subnet){
 if(networkState.busy)return;
 Object.assign(networkState,{busy:true,message:''});renderPairing();
 try{
  unwrap(await api('/networks/trust',{method:'POST',body:JSON.stringify({subnet})}));
  toast('已信任这个网络：同一网络里配对的电脑现在能连进来。');
  await refresh();
 }catch(error){
  networkState.message=error?.code==='INVALID_INPUT'?'这台电脑已经不在这个网络上了。刷新后再试。':pairProblem(error);
 }finally{networkState.busy=false;renderPairing();}
}

function renderPairing(){
 const box=$('#pairing');if(!box)return;
 if(!pairingReady()){box.replaceChildren();return;}
 const pairs=data('pairs');noticePairs(pairs);
 // Keep what someone is typing, and where, across the periodic redraw.
 for(const input of box.querySelectorAll('.pair-check'))pairingState.typed.set(input.dataset.request,input.value);
 const focused=document.activeElement?.classList?.contains('pair-check')?document.activeElement.dataset.request:null;
 const bar=el('div','pair-toolbar'),add=button('配对新电脑','button',openPairDialog);add.id='pair-new';add.disabled=!canWrite();
 const local=data('node',{}),identity=el('div','pair-identity'),identityName=el('div');
 identityName.append(el('span','muted','这台电脑'),el('strong','',local.node?.display_name||'节点状态暂不可读取'));
 identity.append(icon('monitor'),identityName);
 if(local.network)identity.append(el('span','badge',networkLabel(local)));
 bar.append(identity,add);
 const notice=networkNotice(local);
 const cards=[];
 for(const p of pairs){
  if(p.direction==='incoming'&&pairOpen(p))cards.push(incomingPairCard(p));
  else if(p.direction==='outgoing'&&p.status==='pending')cards.push(outgoingPairCard(p));
  else if(p.direction==='outgoing'&&['declined','expired','failed'].includes(p.status)&&!pairingState.dismissed.has(p.request_id))cards.push(endedPairCard(p));
 }
 const section=el('section','pair-requests'),heading=el('h2','','配对申请'),list=el('div','card-grid entity-list');
 list.append(...cards);section.append(heading,list);
 box.replaceChildren(bar,...(notice?[notice]:[]),...(cards.length?[section]:[]));
 for(const input of box.querySelectorAll('.pair-check')){input.value=pairingState.typed.get(input.dataset.request)||'';if(input.dataset.request===focused)input.focus();}
}
function pairMessage(p){const m=el('p','form-note pair-message',pairingState.messages.get(p.request_id)||'');m.setAttribute('role','status');m.hidden=!m.textContent;return m;}
function incomingPairCard(p){
 const card=el('article','entity-card pair-card');card.dataset.request=p.request_id;card.dataset.direction='incoming';
 const top=el('div','entity-top');top.append(el('h3','entity-name',p.card.display_name),el('span','badge warn','待你批准'));
 const facts=el('p','pair-facts',`${pairHost(p)} · ${pairLeft(p)}`);facts.title=`对方证书指纹 ${p.card.fingerprint}`;
 const form=el('form','pair-approve'),label=el('label','pair-check-label','校验码'),input=el('input','pair-check');
 input.dataset.request=p.request_id;input.maxLength=6;input.autocomplete='off';input.spellcheck=false;input.placeholder='6 位';input.setAttribute('aria-label',`${p.card.display_name} 面板上显示的 6 位校验码`);label.append(input);
 const approve=el('button','button','批准');approve.type='submit';
 const decline=button('拒绝','button secondary',()=>declinePair(p));decline.type='button';
 input.addEventListener('input',()=>{if(pairingState.messages.delete(p.request_id)){const m=card.querySelector('.pair-message');m.textContent='';m.hidden=true;}});
 form.append(label,approve,decline);form.addEventListener('submit',event=>{event.preventDefault();approvePair(p,input.value);});
 if(!canWrite())for(const control of form.querySelectorAll('input,button'))control.disabled=true;
 card.append(top,facts,el('p','pair-ask',`请向 ${p.card.display_name} 确认它面板上的 6 位校验码，再批准。`),form,pairMessage(p));
 return card;
}
function outgoingPairCard(p){
 const card=el('article','entity-card pair-card');card.dataset.request=p.request_id;card.dataset.direction='outgoing';
 const top=el('div','entity-top');top.append(el('h3','entity-name',p.card.display_name),el('span','badge live','待对方批准'));
 const ask=el('p','pair-ask');ask.append(`在 ${p.card.display_name} 的面板上批准时，输入校验码 `,el('strong','pair-digits-inline',ownPairCheck()));
 const facts=el('p','pair-facts',pairOpen(p)?`${pairHost(p)} · ${pairLeft(p)} · 对方批准后，这里会自动完成`:`${pairHost(p)} · 已过有效期，正在向对方确认它批没批`);
 const actions=el('div','entity-actions'),cancel=button('取消申请','button secondary',()=>declinePair(p));cancel.disabled=!canWrite();actions.append(cancel);
 card.append(top,ask,facts,pairMessage(p),actions);
 return card;
}
function endedPairCard(p){
 const name=p.card.display_name,text={declined:`${name} 拒绝了配对申请。`,expired:`${name} 没有在 5 分钟内批准，申请已过期。`,failed:`${name} 批准了，但这台电脑没能完成配对：它的证书和之前配对时不一样。需要先撤销旧的配对。`}[p.status];
 const card=el('article','entity-card pair-card pair-ended');card.dataset.request=p.request_id;card.dataset.direction='outgoing';
 const top=el('div','entity-top');top.append(el('h3','entity-name',`和 ${name} 的配对没有完成`),el('span','badge',{declined:'已拒绝',expired:'已过期',failed:'没完成'}[p.status]));
 const actions=el('div','entity-actions'),again=button('重新申请','button secondary',()=>sendPair(pairCodeOf(p)));again.disabled=!canWrite()||p.status==='failed';
 actions.append(button('知道了','button secondary',()=>{pairingState.dismissed.add(p.request_id);renderPairing();}),again);
 card.append(top,el('p','pair-facts',text),pairMessage(p),actions);
 return card;
}

// Direct network: this computer answers pairing for ten minutes after the
// dialog opens it, and shows a one-time passcode that a computer nearby
// types to pair. Redraws reuse an open window; opening the dialog renews it.
function pairingWindowNote(){
 const box=el('div','pair-window-box'),passcode=el('div','pair-passcode-own'),note=el('span','pair-window'),line=el('span','pair-window-line');
 const again=button('再开 10 分钟','button secondary small',()=>openPairingWindow(box));again.type='button';again.hidden=true;
 line.append(note,again);box.append(passcode,line);
 if((pairingState.windowClosesAt||0)>Date.now()/1000)tickPairingWindow(box);else openPairingWindow(box);
 return box;
}
async function openPairingWindow(box){
 const note=box.querySelector('.pair-window'),again=box.querySelector('.pair-window-line .button');
 note.textContent='正在打开配对窗口…';again.hidden=true;
 try{const w=unwrap(await api('/pairing/window',{method:'POST',body:'{}'}));pairingState.windowClosesAt=w.closes_at;pairingState.windowRevision=w.revision||0;pairingState.windowBoot=w.boot;pairingState.ownPasscode=w.passcode||null;pairingState.triesLeft=w.tries_left??0;}
 catch(e){note.textContent=`没能打开配对窗口：${pairProblem(e)}`;again.hidden=false;return;}
 tickPairingWindow(box);
}
// What the daemon says, when its status is at least as new as what opening
// the window returned (by revision): a passcode used or out of tries, a
// window closed, or one opened again elsewhere (hmesh pair show).
function currentWindow(){
 // A restarted daemon (another boot) closed the window, whatever its revision.
 const w=data('node',{}).pairing_window,fresh=w&&(w.boot!==pairingState.windowBoot||(w.revision??-1)>=(pairingState.windowRevision||0));
 if(w&&fresh)return {closesAt:w.closes_at||0,code:w.passcode||null,tries:w.tries_left??0,used:w.passcode_used===true};
 return {closesAt:pairingState.windowClosesAt||0,code:pairingState.ownPasscode||null,tries:pairingState.triesLeft??0,used:false};
}
function tickPairingWindow(box){
 clearInterval(pairingState.windowTimer);
 const note=box.querySelector('.pair-window'),again=box.querySelector('.pair-window-line .button'),passcode=box.querySelector('.pair-passcode-own');
 let ticks=0;
 const tick=()=>{
  if(!note.isConnected||!$('#pair-dialog')?.open){clearInterval(pairingState.windowTimer);return;}
  // Ask the daemon every few seconds while the dialog is open.
  if(++ticks%3===0&&!document.hidden)refresh();
  const {closesAt,code,tries,used}=currentWindow();pairingState.windowClosesAt=closesAt;
  const left=Math.round(closesAt-Date.now()/1000);
  if(left<=0){passcode.replaceChildren();passcode.hidden=true;note.textContent='配对窗口已关闭：别的电脑现在找不到这台，用配对码申请也会被拒绝。';again.textContent='再开 10 分钟';again.hidden=false;clearInterval(pairingState.windowTimer);return;}
  const clock=`${Math.floor(left/60)}:${String(left%60).padStart(2,'0')}`;
  if(nearbyReady()&&code){
   passcode.hidden=false;
   const digits=el('span','pair-passcode-digits',spacedPasscode(code));
   passcode.replaceChildren(el('b','','这台电脑的口令'),digits,el('span','',`在另一台电脑的「附近的电脑」里选中这台，输入这 6 位数字。还能试 ${tries} 次。`));
   note.textContent=`配对窗口还剩 ${clock}，这段时间里附近的电脑能看到这台，也能用配对码申请。`;again.hidden=true;
  }else if(nearbyReady()){
   passcode.hidden=false;passcode.replaceChildren(el('span','',used?'口令已经用过了。要再配一台电脑，换一个新口令。':'口令错了 3 次，已经作废，附近的电脑也看不到这台了。配对码照样能用；要用口令，换一个新口令。'));
   note.textContent=`配对窗口还剩 ${clock}，这段时间里别的电脑能用配对码申请。`;again.textContent='换一个口令';again.hidden=false;
  }else{
   passcode.hidden=true;
   note.textContent=`配对窗口还剩 ${clock}，这段时间里别的电脑才能用这个配对码申请。`;again.hidden=true;
  }
 };
 tick();pairingState.windowTimer=setInterval(tick,1000);
}

// Direct network: computers on this network with pairing open. Choose one
// and type the passcode its screen shows; nothing to approve on either side.
function directNode(){return data('node',{}).network==='direct';}
function nearbyReady(){return state.runtime?.capabilities?.pairing_nearby===true&&directNode();}
function spacedPasscode(code){return code?`${code.slice(0,3)} ${code.slice(3)}`:'';}
function nearbySection(){
 const box=el('section','pair-nearby');box.id='pair-nearby';
 box.append(el('div','pair-nearby-head'),el('div','pair-nearby-list'));
 // After the dialog is shown: looking stops once it is closed.
 drawNearby(box);setTimeout(lookNearby,0);
 return box;
}
async function lookNearby(){
 clearTimeout(pairingState.nearbyTimer);
 if(!$('#pair-dialog')?.open||pairingState.sent)return;
 try{pairingState.nearby=unwrap(await api('/pairing/nearby'))?.computers||[];pairingState.nearbyError='';}
 catch(e){pairingState.nearbyError=pairProblem(e);}
 pairingState.nearbyLooked=true;
 const box=$('#pair-nearby');if(box)drawNearby(box);
 if($('#pair-dialog')?.open&&!pairingState.sent)pairingState.nearbyTimer=setTimeout(lookNearby,3000);
}
function drawNearby(box){
 const head=box.querySelector('.pair-nearby-head'),list=box.querySelector('.pair-nearby-list'),found=pairingState.nearby||[];
 head.replaceChildren(el('b','','附近的电脑'),el('span','',!pairingState.nearbyLooked?'正在找…':found.length?'选中要配对的那台，输入它屏幕上的 6 位口令。':''));
 // Keep the passcode being typed, and the focus, across redraws.
 const typing=list.querySelector('.pair-passcode'),typed=typing?.value||'',focused=document.activeElement===typing;
 const rows=found.map(nearbyRow);
 if(pairingState.nearbyLooked&&!found.length)rows.push(el('p','form-note muted pair-nearby-empty',pairingState.nearbyError||'还没找到。对方也要打开「配对新电脑」；有的 Wi-Fi 会挡住查找，这时用下面的配对码。'));
 list.replaceChildren(...rows);
 const input=list.querySelector('.pair-passcode');if(input){input.value=typed;if(focused)input.focus();}
}
function nearbyRow(c){
 const row=el('div','pair-nearby-row');row.dataset.endpoint=c.endpoint;
 const who=el('div','pair-nearby-who');who.append(el('b','',c.name),el('span','',c.address));
 row.append(who);
 if(c.paired){row.append(el('span','badge','已配对'));return row;}
 if(pairingState.chosen!==c.endpoint){
  const pick=button('用口令配对','button secondary small',()=>{pairingState.chosen=c.endpoint;pairingState.passcodeMessage='';drawNearby($('#pair-nearby'));$('#pair-nearby .pair-passcode')?.focus();});
  pick.type='button';pick.disabled=!canWrite();row.append(pick);return row;
 }
 const form=el('div','pair-passcode-form'),input=el('input','pair-passcode');
 input.inputMode='numeric';input.autocomplete='off';input.maxLength=7;input.placeholder='6 位口令';input.setAttribute('aria-label',`${c.name} 屏幕上的 6 位口令`);
 const go=el('button','button small','配对');go.type='button';go.disabled=!canWrite()||pairingState.busy;
 const message=el('p','form-note pair-message',pairingState.passcodeMessage||'');message.setAttribute('role','status');message.hidden=!message.textContent;if(pairingState.passcodeError)message.classList.add('error');
 form.append(input,go);go.addEventListener('click',()=>pairWithPasscode(c,input.value));
 // Enter pairs with this computer, never submits the pairing-code form around it.
 input.addEventListener('keydown',event=>{if(event.key==='Enter'){event.preventDefault();event.stopPropagation();pairWithPasscode(c,input.value);}});
 input.addEventListener('input',()=>{if(pairingState.passcodeMessage){pairingState.passcodeMessage='';message.hidden=true;}});
 row.classList.add('chosen');row.append(form,message);
 return row;
}
async function pairWithPasscode(c,value){
 const code=String(value||'').replace(/[\s-]/g,'');
 if(!/^[0-9]{6}$/.test(code)){pairingState.passcodeMessage='口令是对方屏幕上的 6 位数字。';pairingState.passcodeError=true;drawNearby($('#pair-nearby'));return;}
 if(pairingState.busy)return;pairingState.busy=true;
 pairingState.passcodeMessage='正在配对…';pairingState.passcodeError=false;drawNearby($('#pair-nearby'));
 try{
  const p=unwrap(await api('/pairs/passcode',{method:'POST',body:JSON.stringify({endpoint:c.endpoint,passcode:code})}));
  pairingState.sent={...p,viaPasscode:true};pairingState.chosen=null;pairingState.passcodeMessage='';
  if(p.status==='pending')pairingState.waiting.add(p.request_id);
  drawPairDialog();refresh();
 }catch(error){pairingState.busy=false;pairingState.passcodeMessage=passcodeProblem(error);pairingState.passcodeError=true;drawNearby($('#pair-nearby'));}
 finally{pairingState.busy=false;}
}
function passcodeProblem(error){
 return {UNAUTHENTICATED:'口令不对。请看对方屏幕上的 6 位数字；每台电脑的口令只能试 3 次。',PERMISSION_DENIED:'对方现在没有可用的口令：请在对方面板里重新打开「配对新电脑」。',TASK_CONFLICT:'口令对上了，但对方那边没能完成配对。请重新选中它再试一次。'}[error?.code]||pairProblem(error);
}
function openPairDialog(){
 if(pairingState.busy)return;
 Object.assign(pairingState,{sent:null,windowClosesAt:0,windowRevision:0,windowBoot:null,ownPasscode:null,triesLeft:0,nearby:[],nearbyLooked:false,nearbyError:'',chosen:null,passcodeMessage:'',passcodeError:false});
 const local=data('node',{}).node?.display_name;
 $('#pair-local').textContent=local?`本机 · ${local}`:'本机节点';
 drawPairDialog();
 $('#pair-dialog').showModal();
 if(!nearbyReady())$('#pair-code')?.focus({preventScroll:true});
}
function copyPairText(node,label){
 const copy=button(label,'button secondary',async()=>{
  try{await navigator.clipboard.writeText(node.textContent);copy.textContent='已复制';}
  catch{getSelection().selectAllChildren(node);copy.textContent='已选中，请按复制';}
 });
 copy.type='button';copy.disabled=!node.textContent;
 return copy;
}
function drawPairDialog(){
 const body=$('#pair-body'),sent=pairingState.sent;
 if(sent){
  const name=sent.card.display_name,done=el('div','pair-sent pair-scroll'),actions=el('div','confirm-actions pair-footer');
  const status=el('div','pair-sent-state');status.setAttribute('role','status');
  if(sent.status==='approved'){
   status.append(el('span','badge good','已配对'),el('h3','pair-sent-title',`已和 ${name} 配对`));
   done.append(status,el('p','','可在「已配对的电脑」查看对方授权的 Agent。'));
  }else if(sent.status==='pending'&&sent.viaPasscode){
   status.append(el('span','badge live','正在完成'),el('h3','pair-sent-title',`口令对上了，正在和 ${name} 完成配对`));
   done.append(status,el('p','','连上对方后几秒内自动完成，不用再操作。'));
  }else if(sent.status==='pending'){
   status.append(el('span','badge warn','待对方批准'),el('h3','pair-sent-title',`已向 ${name} 发出配对申请`));
   const check=el('code','pair-digits',ownPairCheck()),checkBox=el('div','pair-check-tile');
   checkBox.append(check,copyPairText(check,'复制校验码'));
   done.append(status,el('p','',`在 ${name} 的「配对节点」页输入这 6 位校验码并批准。`),checkBox,
    el('p','form-note muted',`${pairLeft(sent)}。对方批准后，状态会自动更新。`));
  }else{
   status.append(el('h3','pair-sent-title',`和 ${name} 的配对没有完成`));
   done.append(status,el('p','',{declined:`${name} 拒绝了这个申请。`,expired:'申请已过期。',cancelled:'申请已取消。'}[sent.status]||'请在节点列表查看申请状态。'));
  }
  const close=button(sent.status==='pending'?'返回列表':'完成','button',()=>$('#pair-dialog').close());
  actions.append(close);
  body.replaceChildren(done,actions);
  close.focus({preventScroll:true});
  return;
 }
 const form=el('form','pair-form'),scroll=el('div','pair-scroll'),label=el('label','form-label','对方的配对码'),code=el('textarea');
 code.id='pair-code';code.rows=3;code.spellcheck=false;code.autocomplete='off';code.placeholder='hmesh://…';code.maxLength=600;label.append(code);
 const node=data('node',{}),canReceive=!outboundOnly(),remoteBox=el('div','pair-entry');
 if(!canReceive)remoteBox.append(el('p','form-note muted',nearbyReady()?'本机以仅 MCP 模式运行，配对需要从这台电脑发起：在上面选中附近的电脑、输入它的口令，或者粘贴对方的配对码。':'本机以仅 MCP 模式运行，配对需要从这台电脑发起。'));
 remoteBox.append(label,el('p','form-note muted','让对方在「配对新电脑」里切到「本机配对码」，把复制的内容发给你。'));
 const ownBox=el('div','pair-entry'),ownCode=el('code','pair-own-code',canReceive?node.pair_code||'':'');ownCode.id='own-pair-code';
 ownBox.append(el('p','form-note','把本机配对码发给对方。收到申请后，在「配对节点」核对对方显示的 6 位校验码。'),
  ownCode,el('p','form-note muted',node.network==='loopback'?'本机当前使用回环地址，供同一台电脑上的节点配对。':''));
 const message=el('p','form-note pair-message');message.id='pair-message';message.setAttribute('role','status');message.hidden=true;
 const send=el('button','button','发出配对申请');send.type='submit';send.id='pair-send';send.disabled=!canWrite();
 const copy=copyPairText(ownCode,'复制配对码');
 const cancel=button('取消','button secondary',()=>$('#pair-dialog').close());cancel.type='button';
 const actions=el('div','confirm-actions');actions.append(cancel,copy,send);
 const footer=el('div','pair-footer');footer.append(message,actions);
 const methods=el('div','segmented pair-methods');methods.setAttribute('role','group');methods.setAttribute('aria-label','配对码来源');
 const useMode=own=>{
  if(pairingState.busy)return;
  remote.setAttribute('aria-pressed',String(!own));local.setAttribute('aria-pressed',String(own));
  remote.classList.toggle('selected',!own);local.classList.toggle('selected',own);
  remoteBox.hidden=own;ownBox.hidden=!own;message.hidden=own||!message.textContent;
  copy.hidden=!own;send.hidden=own;
  scroll.scrollTop=0;
 };
 const remote=button('粘贴对方的码','',()=>useMode(false)),local=button('本机配对码','',()=>useMode(true));
 remote.type=local.type='button';methods.append(remote,local);
 // A direct network lists computers nearby with pairing open.
 if(nearbyReady())scroll.append(nearbySection());
 if(canReceive)scroll.append(methods);
 scroll.append(remoteBox);
 if(canReceive)scroll.append(ownBox);
 if(node.pairing_window?.required&&ownCode.textContent)scroll.append(pairingWindowNote());
 form.append(scroll,footer);
 code.addEventListener('input',()=>{message.hidden=true;message.textContent='';message.classList.remove('error');});
 form.addEventListener('submit',event=>{event.preventDefault();sendPair(code.value,message,send);});
 body.replaceChildren(form);
 useMode(false);
}
async function sendPair(code,message=null,send=null){
 if(pairingState.busy)return;
 // Already waiting on this computer: show that request instead of sending another.
 const fingerprint=String(code||'').replace(/\s+/g,'').split('/').pop().toLowerCase();
 const waiting=data('pairs').find(p=>p.direction==='outgoing'&&pairOpen(p)&&String(p.card.fingerprint).toLowerCase()===fingerprint);
 if(waiting){pairingState.sent=waiting;if(!$('#pair-dialog').open)$('#pair-dialog').showModal();drawPairDialog();return;}
 pairingState.busy=true;
 const controls=send?.form.querySelectorAll('button:enabled,textarea:enabled')||[];
 if(send){
  for(const control of controls)control.disabled=true;
  $('#pair-dialog [data-close]').disabled=true;
 }
 if(message){message.hidden=false;message.classList.remove('error');message.textContent='正在联系对方…';}
 try{
  const p=unwrap(await api('/pairs/request',{method:'POST',body:JSON.stringify({code:String(code||'').trim()})}));
  pairingState.sent=p;pairingState.waiting.add(p.request_id);
  if(!$('#pair-dialog').open)$('#pair-dialog').showModal();
  drawPairDialog();refresh();
 }catch(error){
  if(message){message.textContent=pairProblem(error);message.classList.add('error');}else toast(pairProblem(error));
 }finally{
  pairingState.busy=false;
  $('#pair-dialog [data-close]').disabled=false;
  for(const control of controls)control.disabled=false;
  if(send)send.disabled=!canWrite();
 }
}
$('#pair-dialog').addEventListener('cancel',event=>{if(pairingState.busy)event.preventDefault();});
async function approvePair(p,check){
 const value=String(check||'').trim();
 if(!/^[0-9a-fA-F]{6}$/.test(value)){pairingState.messages.set(p.request_id,'请输入 6 位校验码（数字和字母 A–F）。');renderPairing();return;}
 try{
  unwrap(await api(`/pairs/${p.request_id}/approve`,{method:'POST',body:JSON.stringify({check:value})}));
  pairingState.typed.delete(p.request_id);pairingState.messages.delete(p.request_id);
  toast(`已批准 ${p.card.display_name}。它那边几秒内会自动完成配对。`);await refresh();
 }catch(error){pairingState.messages.set(p.request_id,pairProblem(error));renderPairing();}
}
async function declinePair(p){
 try{
  unwrap(await api(`/pairs/${p.request_id}/decline`,{method:'POST',body:'{}'}));
  pairingState.waiting.delete(p.request_id);pairingState.typed.delete(p.request_id);pairingState.messages.delete(p.request_id);
  toast(p.direction==='incoming'?`已拒绝 ${p.card.display_name} 的配对申请。`:`已取消向 ${p.card.display_name} 的配对申请。`);await refresh();
 }catch(error){pairingState.messages.set(p.request_id,pairProblem(error));renderPairing();}
}
// While a request sent from here waits, check more often than the usual ten
// seconds so the finished pairing shows up soon after the other side approves.
setInterval(()=>{if(!document.hidden&&state.snapshot&&data('pairs').some(p=>p.direction==='outgoing'&&p.status==='pending'))refresh();},3000);

// Where a paired computer is reached: the addresses it told this one, and
// ones typed here by hand (kept only on this computer), edited on its card.
const addressState={editing:null,draft:'',message:'',busy:false};
function addressesReady(){return state.runtime?.capabilities?.peer_addresses===true;}
function originHost(origin){try{return new URL(origin).host;}catch{return String(origin||'');}}
function peerAddressRow(peer){
 const id=peer.card.node_id,routes=peer.routes||{},row=el('div','paired-addresses');row.dataset.peer=id;
 const told=[...new Set([peer.card.endpoint,...(routes.advertised||[])].map(originHost).filter(Boolean))];
 const manual=(routes.manual||[]).map(originHost).filter(Boolean);
 if(addressState.editing!==id){
  const list=el('span','paired-address-list mono',[...manual.map(h=>`${h}（手动）`),...told].join(' · '));
  const edit=button('改地址','text-button paired-address-edit',()=>{Object.assign(addressState,{editing:id,draft:manual.join(' '),message:''});renderNodes();$('#node-list .paired-address-input')?.focus();});
  edit.type='button';edit.disabled=!canWrite();
  row.append(el('span','paired-address-label','地址'),list,edit);
  return row;
 }
 const form=el('form','paired-address-form'),input=el('input','paired-address-input');
 input.value=addressState.draft;input.placeholder='如 studio.local:38880，多个用空格隔开';input.spellcheck=false;input.autocomplete='off';input.maxLength=2000;
 input.setAttribute('aria-label',`${peer.card.display_name} 的手动地址`);
 input.addEventListener('input',()=>{addressState.draft=input.value;});
 const save=el('button','button small','保存');save.type='submit';save.disabled=addressState.busy||!canWrite();
 const close=()=>{Object.assign(addressState,{editing:null,draft:'',message:''});renderNodes();};
 const cancel=button('取消','button secondary small',close);cancel.type='button';
 input.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();close();}});
 form.append(input,save,cancel);
 form.addEventListener('submit',event=>{event.preventDefault();savePeerAddresses(peer,input.value);});
 const message=el('p','form-note pair-message',addressState.message);message.setAttribute('role','status');message.hidden=!addressState.message;
 row.classList.add('editing');
 row.append(el('span','paired-address-label','手动地址'),form,el('p','form-note muted paired-address-note','只保存在这台电脑上，前面的地址连不上时再试。留空保存就清除。'),message);
 return row;
}
async function savePeerAddresses(peer,value){
 if(addressState.busy)return;
 const addresses=String(value||'').split(/[\s,，]+/).filter(Boolean),name=peer.card.display_name;
 addressState.busy=true;addressState.message='';
 try{
  unwrap(await api(`/peers/${peer.card.node_id}/addresses`,{method:'POST',body:JSON.stringify({addresses})}));
  Object.assign(addressState,{editing:null,draft:'',message:''});
  toast(addresses.length?`已保存 ${name} 的地址。`:`已清除 ${name} 的手动地址。`);
  await refresh();
 }catch(error){
  addressState.message=error?.code==='INVALID_INPUT'?'地址写成「主机名:端口」或「IP:端口」，比如 studio.local:38880。':pairProblem(error);
 }finally{addressState.busy=false;renderNodes();}
}
