// Markdown to DOM for agent replies. It only creates elements and text
// nodes; no string is ever parsed as HTML. Links open http(s) addresses
// only; anything else keeps its text and loses the link.

const mdListItem=/^( *)([-*+]|\d{1,9}[.)])(?: +(.*))?$/;
const mdFence=/^ {0,3}(`{3,}|~{3,})\s*([^\s`]*)/;
const mdRule=/^ {0,3}([-*_])(?: *\1){2,} *$/;
const mdHeading=/^ {0,3}(#{1,6})(?: +(.*?))?(?: +#+)? *$/;
const mdTableRule=/^ *\|? *:?-+:? *(\| *:?-+:? *)*\|? *$/;

function renderMarkdown(source){
  const root=el('div','md');
  mdBlocks(String(source??'').replace(/\r\n?/g,'\n').split('\n'),root);
  return root;
}
function mdIndent(line){return line.match(/^ */)[0].length;}
function mdStartsBlock(line){return mdFence.test(line)||mdHeading.test(line)||mdRule.test(line)||/^ {0,3}>/.test(line)||mdListItem.test(line);}
function mdCells(line){
  let row=line.trim();
  if(row.startsWith('|'))row=row.slice(1);
  if(row.endsWith('|')&&!row.endsWith('\\|'))row=row.slice(0,-1);
  const cells=[];let cell='';
  for(let i=0;i<row.length;i++){
    if(row[i]==='\\'&&row[i+1]==='|'){cell+='|';i++;}
    else if(row[i]==='|'){cells.push(cell.trim());cell='';}
    else cell+=row[i];
  }
  cells.push(cell.trim());
  return cells;
}
function mdBlocks(lines,parent){
  let i=0;
  while(i<lines.length){
    const line=lines[i];
    if(!line.trim()){i++;continue;}
    let m=line.match(mdFence);
    if(m){
      const mark=m[1][0],size=m[1].length,body=[];i++;
      while(i<lines.length&&!new RegExp(`^ {0,3}\\${mark}{${size},} *$`).test(lines[i]))body.push(lines[i++]);
      i++;
      parent.append(mdCode(body.join('\n'),m[2]));continue;
    }
    m=line.match(mdHeading);
    if(m){const h=el(`h${m[1].length}`);mdInline(m[2]||'',h);parent.append(h);i++;continue;}
    if(mdRule.test(line)){parent.append(el('hr'));i++;continue;}
    if(/^ {0,3}>/.test(line)){
      const quoted=[];
      while(i<lines.length&&lines[i].trim()&&(/^ {0,3}>/.test(lines[i])||!mdStartsBlock(lines[i])))quoted.push(lines[i++].replace(/^ {0,3}> ?/,''));
      const box=el('blockquote');mdBlocks(quoted,box);parent.append(box);continue;
    }
    if(line.includes('|')&&i+1<lines.length&&mdTableRule.test(lines[i+1])&&lines[i+1].includes('-')){i=mdTable(lines,i,parent);continue;}
    if(mdListItem.test(line)&&line.match(mdListItem)[3]!==undefined){i=mdList(lines,i,parent);continue;}
    const text=[];
    while(i<lines.length&&lines[i].trim()&&!(text.length&&mdStartsBlock(lines[i])))text.push(lines[i++].replace(/^ +/,''));
    const p=el('p');mdInline(text.join('\n'),p);parent.append(p);
  }
}
function mdCode(text,lang){
  const box=el('div','md-code'),pre=el('pre'),code=el('code','',text);
  if(lang)code.dataset.lang=lang;
  pre.append(code);
  const bar=el('div','md-code-bar');bar.append(el('span','',lang||''));
  const copy=button('复制','md-copy',async()=>{
    try{await navigator.clipboard.writeText(text);copy.textContent='已复制';setTimeout(()=>{copy.textContent='复制';},1500);}
    catch{copy.textContent='复制失败';}
  });
  copy.type='button';bar.append(copy);
  box.append(bar,pre);
  return box;
}
function mdTable(lines,i,parent){
  const head=mdCells(lines[i]),aligns=mdCells(lines[i+1]).map(c=>c.startsWith(':')&&c.endsWith(':')?'center':c.endsWith(':')?'right':c.startsWith(':')?'left':'');
  const wrap=el('div','md-table'),table=el('table'),thead=el('thead'),tbody=el('tbody'),tr=el('tr');
  const cell=(tag,text,col)=>{const c=el(tag);if(aligns[col])c.className=`align-${aligns[col]}`;mdInline(text,c);return c;};
  head.forEach((text,col)=>tr.append(cell('th',text,col)));
  thead.append(tr);i+=2;
  while(i<lines.length&&lines[i].trim()&&lines[i].includes('|')){
    const row=el('tr'),cells=mdCells(lines[i++]);
    head.forEach((_,col)=>row.append(cell('td',cells[col]??'',col)));
    tbody.append(row);
  }
  table.append(thead,tbody);wrap.append(table);parent.append(wrap);
  return i;
}
function mdList(lines,i,parent){
  const first=lines[i].match(mdListItem),indent=first[1].length,ordered=/\d/.test(first[2]);
  const list=el(ordered?'ol':'ul');
  if(ordered&&parseInt(first[2],10)!==1)list.start=parseInt(first[2],10);
  let loose=false;
  while(i<lines.length){
    const m=lines[i].match(mdListItem);
    if(!m||m[3]===undefined||m[1].length!==indent||/\d/.test(m[2])!==ordered)break;
    const body=[m[3]],inner=indent+m[2].length+1;i++;
    while(i<lines.length){
      const line=lines[i];
      if(!line.trim()){
        const next=lines.slice(i+1).find(l=>l.trim());
        if(next!==undefined&&mdIndent(next)>indent){body.push('');i++;continue;}
        break;
      }
      const item=line.match(mdListItem);
      if(item&&item[1].length<=indent)break;
      if(mdIndent(line)>indent){body.push(line.slice(Math.min(mdIndent(line),inner)));i++;continue;}
      if(mdStartsBlock(line))break;
      body.push(line.trim());i++;
    }
    if(body.includes(''))loose=true;
    const li=el('li');
    const task=body[0].match(/^\[([ xX])\] +(.*)$/);
    if(task){li.classList.add('task');li.append(el('span',`md-check ${task[1]===' '?'':'done'}`,task[1]===' '?'☐':'☑'));body[0]=task[2];}
    mdBlocks(body,li);
    list.append(li);
    if(i<lines.length&&!lines[i].trim()){
      const next=lines.slice(i).find(l=>l.trim());
      const again=next?.match(mdListItem);
      if(again&&again[1].length===indent){loose=true;while(!lines[i].trim())i++;}
    }
  }
  // A tight list keeps its items on one line each, without paragraph gaps.
  if(!loose)for(const p of list.querySelectorAll(':scope > li > p'))p.replaceWith(...p.childNodes);
  parent.append(list);
  return i;
}
function mdLink(label,url){
  let safe=null;
  try{const u=new URL(url);if(u.protocol==='http:'||u.protocol==='https:')safe=u.href;}catch{}
  if(!safe){const span=el('span','md-unlinked');mdInline(label,span,true);return span;}
  const a=el('a');a.href=safe;a.target='_blank';a.rel='noopener noreferrer';a.title=safe;
  mdInline(label,a,true);
  return a;
}
// The closing delimiter of an emphasis run, or -1.
function mdClose(text,mark,from){
  for(let at=text.indexOf(mark,from);at>=0;at=text.indexOf(mark,at+1)){
    if(at>from&&!/\s/.test(text[at-1])){
      if(mark.length===1&&text[at+1]===mark){at++;continue;}
      if(mark==='_'&&/[\p{L}\p{N}]/u.test(text[at+1]||''))continue;
      return at;
    }
  }
  return -1;
}
function mdLinkAt(text,i){
  let depth=0,end=-1;
  for(let j=i;j<text.length;j++){
    if(text[j]==='\\'){j++;continue;}
    if(text[j]==='[')depth++;
    else if(text[j]===']'&&--depth===0){end=j;break;}
  }
  if(end<0||text[end+1]!=='(')return null;
  let parens=0,close=-1;
  for(let j=end+1;j<text.length;j++){
    if(text[j]==='(')parens++;
    else if(text[j]===')'&&--parens===0){close=j;break;}
  }
  if(close<0)return null;
  const target=text.slice(end+2,close).trim().replace(/\s+("[^"]*"|'[^']*')$/,'').replace(/^<(.*)>$/,'$1');
  return{label:text.slice(i+1,end),url:target,end:close+1};
}
function mdInline(text,parent,inLink=false){
  let buffer='';
  const flush=()=>{if(buffer){parent.append(document.createTextNode(buffer));buffer='';}};
  for(let i=0;i<text.length;){
    const c=text[i];
    if(c==='\\'&&/[!-/:-@[-`{-~]/.test(text[i+1]||'')){buffer+=text[i+1];i+=2;continue;}
    if(c==='\n'){flush();parent.append(el('br'));i++;continue;}
    if(c==='`'){
      const run=text.slice(i).match(/^`+/)[0],end=text.indexOf(run,i+run.length);
      if(end>0){flush();parent.append(el('code','',text.slice(i+run.length,end).replace(/^ (.+) $/,'$1')));i=end+run.length;continue;}
      buffer+=run;i+=run.length;continue;
    }
    const pair=text.slice(i,i+2);
    if((pair==='**'||pair==='__'||pair==='~~')&&!/\s/.test(text[i+2]||' ')){
      const end=mdClose(text,pair,i+2);
      if(end>0){flush();const node=el(pair==='~~'?'del':'strong');mdInline(text.slice(i+2,end),node,inLink);parent.append(node);i=end+2;continue;}
    }
    if((c==='*'||c==='_')&&text[i+1]!==c&&!/\s/.test(text[i+1]||' ')&&!(c==='_'&&/[\p{L}\p{N}]/u.test(text[i-1]||''))){
      const end=mdClose(text,c,i+1);
      if(end>0){flush();const node=el('em');mdInline(text.slice(i+1,end),node,inLink);parent.append(node);i=end+1;continue;}
    }
    if(!inLink&&c==='['){
      const link=mdLinkAt(text,i);
      if(link){flush();parent.append(mdLink(link.label,link.url));i=link.end;continue;}
    }
    if(!inLink&&c==='<'){
      const m=text.slice(i).match(/^<(https?:\/\/[^\s<>]+)>/i);
      if(m){flush();parent.append(mdLink(m[1],m[1]));i+=m[0].length;continue;}
    }
    if(!inLink&&(c==='h'||c==='H')&&/^https?:\/\//i.test(text.slice(i,i+8))&&!/[\p{L}\p{N}]/u.test(text[i-1]||'')){
      const url=text.slice(i).match(/^https?:\/\/[^\s<>]+/i)[0].replace(/[.,;:!?'")\]}。，；：！？）】」』]+$/u,'');
      flush();parent.append(mdLink(url,url));i+=url.length;continue;
    }
    buffer+=c;i++;
  }
  flush();
}
