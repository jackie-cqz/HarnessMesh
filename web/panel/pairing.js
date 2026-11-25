'use strict';
// Pair this computer with another one from the panel. One side pastes the
// other's pairing code and sends a request; the other side approves it once,
// typing the six-character check code the first side shows. The daemon that
// sent the request finishes pairing by itself when the approval arrives.
const pairingState={sent:null,waiting:new Set(),dismissed:new Set(),typed:new Map(),messages:new Map(),busy:false};
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
  if(p.status==='approved')toast(`已和 ${p.card.display_name} 配对。两台电脑现在可以互相使用对方的 Agent。`);
 }
}

function renderPairing(){
 const box=$('#pairing');if(!box)return;
 if(!pairingReady()){box.replaceChildren();return;}
 const pairs=data('pairs');noticePairs(pairs);
 // Keep what someone is typing, and where, across the periodic redraw.
 for(const input of box.querySelectorAll('.pair-check'))pairingState.typed.set(input.dataset.request,input.value);
 const focused=document.activeElement?.classList?.contains('pair-check')?document.activeElement.dataset.request:null;
 const bar=el('div','pair-toolbar'),add=button('＋ 配对新电脑','button',openPairDialog);add.id='pair-new';add.disabled=!canWrite();
 bar.append(add);
 const cards=[];
 for(const p of pairs){
  if(p.direction==='incoming'&&pairOpen(p))cards.push(incomingPairCard(p));
  else if(p.direction==='outgoing'&&p.status==='pending')cards.push(outgoingPairCard(p));
  else if(p.direction==='outgoing'&&['declined','expired','failed'].includes(p.status)&&!pairingState.dismissed.has(p.request_id))cards.push(endedPairCard(p));
 }
 const list=el('div','card-grid entity-list pair-requests');list.append(...cards);
 box.replaceChildren(bar,...(cards.length?[list]:[]));
 for(const input of box.querySelectorAll('.pair-check')){input.value=pairingState.typed.get(input.dataset.request)||'';if(input.dataset.request===focused)input.focus();}
}
function pairMessage(p){const m=el('p','form-note pair-message',pairingState.messages.get(p.request_id)||'');m.setAttribute('role','status');m.hidden=!m.textContent;return m;}
function incomingPairCard(p){
 const card=el('article','entity-card pair-card');card.dataset.request=p.request_id;card.dataset.direction='incoming';
 const top=el('div','entity-top');top.append(el('h3','entity-name',`${p.card.display_name} 想和这台电脑配对`),el('span','badge warn','等你批准'));
 const facts=el('p','pair-facts',`${pairHost(p)} · ${pairLeft(p)}`);facts.title=`对方证书指纹 ${p.card.fingerprint}`;
 const form=el('form','pair-approve'),label=el('label','pair-check-label','校验码'),input=el('input','pair-check');
 input.dataset.request=p.request_id;input.maxLength=6;input.autocomplete='off';input.spellcheck=false;input.placeholder='6 位';input.setAttribute('aria-label',`${p.card.display_name} 面板上显示的 6 位校验码`);label.append(input);
 const approve=el('button','button','批准');approve.type='submit';
 const decline=button('拒绝','button secondary',()=>declinePair(p));decline.type='button';
 input.addEventListener('input',()=>{if(pairingState.messages.delete(p.request_id)){const m=card.querySelector('.pair-message');m.textContent='';m.hidden=true;}});
 form.append(label,approve,decline);form.addEventListener('submit',event=>{event.preventDefault();approvePair(p,input.value);});
 if(!canWrite())for(const control of form.querySelectorAll('input,button'))control.disabled=true;
 card.append(top,facts,form,el('p','form-note muted',`${p.card.display_name} 发出申请后，它的面板上会显示 6 位校验码。和你要配对的那台电脑对上了再批准。`),pairMessage(p));
 return card;
}
function outgoingPairCard(p){
 const card=el('article','entity-card pair-card');card.dataset.request=p.request_id;card.dataset.direction='outgoing';
 const top=el('div','entity-top');top.append(el('h3','entity-name',`等 ${p.card.display_name} 批准`),el('span','badge live','等对方批准'));
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
// dialog opens it. Redraws reuse an open window; opening the dialog renews it.
function pairingWindowNote(){
 const note=el('span','pair-window'),line=el('span','pair-window-line');
 const again=button('再开 10 分钟','button secondary small',()=>openPairingWindow(note,again));again.type='button';again.hidden=true;
 line.append(note,again);
 if((pairingState.windowClosesAt||0)>Date.now()/1000)tickPairingWindow(note,again);else openPairingWindow(note,again);
 return line;
}
async function openPairingWindow(note,again){
 note.textContent='正在打开配对窗口…';again.hidden=true;
 try{pairingState.windowClosesAt=unwrap(await api('/pairing/window',{method:'POST',body:'{}'})).closes_at;}
 catch(e){note.textContent=`没能打开配对窗口：${pairProblem(e)}`;again.hidden=false;return;}
 tickPairingWindow(note,again);
}
function tickPairingWindow(note,again){
 clearInterval(pairingState.windowTimer);
 const tick=()=>{
  if(!note.isConnected){clearInterval(pairingState.windowTimer);return;}
  const left=Math.round((pairingState.windowClosesAt||0)-Date.now()/1000);
  if(left<=0){note.textContent='配对窗口已关闭：别的电脑现在用这个配对码申请会被拒绝。';again.hidden=false;clearInterval(pairingState.windowTimer);return;}
  note.textContent=`配对窗口还剩 ${Math.floor(left/60)}:${String(left%60).padStart(2,'0')}，这段时间里别的电脑才能用这个配对码申请。`;
 };
 tick();pairingState.windowTimer=setInterval(tick,1000);
}
function openPairDialog(){pairingState.sent=null;pairingState.windowClosesAt=0;drawPairDialog();$('#pair-dialog').showModal();$('#pair-code')?.focus();}
function drawPairDialog(){
 const body=$('#pair-body'),sent=pairingState.sent;
 if(sent){
  const name=sent.card.display_name,done=el('div','pair-sent'),actions=el('div','confirm-actions');
  if(sent.status==='approved')done.append(el('p','pair-sent-title',`已和 ${name} 配对`),el('p','','两台电脑现在可以互相使用对方的 Agent：在会话里选它的 Agent，或者让本机的 Agent 调用它。'));
  else if(sent.status==='pending')done.append(el('p','pair-sent-title',`已向 ${name} 发出配对申请`),el('p','',`请在 ${name} 的面板上批准，批准时输入这个校验码：`),el('div','pair-digits',ownPairCheck()),el('p','form-note muted','对方批准后，这里会自动完成，不用再操作。申请 5 分钟内有效，关掉这个窗口也没关系。'));
  else done.append(el('p','pair-sent-title',`和 ${name} 的配对没有完成`),el('p','',{declined:`${name} 拒绝了这个申请。`,expired:'申请已过期。',cancelled:'申请已取消。'}[sent.status]||'可以重新申请。'));
  actions.append(button('好','button',()=>$('#pair-dialog').close()));
  body.replaceChildren(done,actions);return;
 }
 const form=el('form','pair-form'),label=el('label','form-label','粘贴另一台电脑的配对码'),code=el('textarea');
 code.id='pair-code';code.rows=3;code.spellcheck=false;code.autocomplete='off';code.placeholder='hmesh://…';code.maxLength=600;label.append(code);
 const own=data('node',{}).pair_code||'',ownBox=el('div','pair-own'),ownText=el('div'),ownCode=el('code','pair-own-code',own);ownCode.id='own-pair-code';
 ownText.append(el('b','','这台电脑的配对码'),ownCode,el('span','','也可以把它发给对方，让对方来申请；对方的申请会出现在「配对节点」里，由你批准。'));
 const copy=button('复制','button secondary small',async()=>{try{await navigator.clipboard.writeText(own);copy.textContent='已复制';}catch{getSelection().selectAllChildren(ownCode);copy.textContent='已选中，请按复制';}});copy.type='button';copy.disabled=!own;
 ownBox.append(ownText,copy);
 // A direct network answers pairing only while this window is open.
 if(data('node',{}).pairing_window?.required&&own)ownText.append(pairingWindowNote());
 // A computer that only connects out cannot be reached: pairing starts here.
 if(typeof outboundOnly==='function'&&outboundOnly())ownBox.replaceChildren(el('span','pair-outbound','这台电脑只用 MCP，不接受别的电脑连进来，所以配对要从这里发起：粘贴对方的配对码。'));
 const message=el('p','form-note pair-message');message.id='pair-message';message.setAttribute('role','status');message.hidden=true;
 const send=el('button','button','发出配对申请');send.type='submit';send.id='pair-send';send.disabled=!canWrite();
 const cancel=button('取消','button secondary',()=>$('#pair-dialog').close());cancel.type='button';
 const actions=el('div','confirm-actions');actions.append(cancel,send);
 form.append(label,el('p','form-note muted','在另一台电脑的面板里点「配对新电脑」，复制那里的配对码。'),ownBox,message,actions);
 code.addEventListener('input',()=>{message.hidden=true;message.classList.remove('error');});
 form.addEventListener('submit',event=>{event.preventDefault();sendPair(code.value,message,send);});
 body.replaceChildren(form);
}
async function sendPair(code,message=null,send=null){
 if(pairingState.busy)return;
 // Already waiting on this computer: show that request instead of sending another.
 const fingerprint=String(code||'').replace(/\s+/g,'').split('/').pop().toLowerCase();
 const waiting=data('pairs').find(p=>p.direction==='outgoing'&&pairOpen(p)&&String(p.card.fingerprint).toLowerCase()===fingerprint);
 if(waiting){pairingState.sent=waiting;if(!$('#pair-dialog').open)$('#pair-dialog').showModal();drawPairDialog();return;}pairingState.busy=true;if(send)send.disabled=true;
 if(message){message.hidden=false;message.classList.remove('error');message.textContent='正在联系对方…';}
 try{
  const p=unwrap(await api('/pairs/request',{method:'POST',body:JSON.stringify({code:String(code||'').trim()})}));
  pairingState.sent=p;pairingState.waiting.add(p.request_id);
  if(!$('#pair-dialog').open)$('#pair-dialog').showModal();
  drawPairDialog();refresh();
 }catch(error){
  if(message){message.textContent=pairProblem(error);message.classList.add('error');}else toast(pairProblem(error));
 }finally{pairingState.busy=false;if(send)send.disabled=!canWrite();}
}
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
