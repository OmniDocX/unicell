/* Compact formatting for a cell range or selected text, using the ribbon's commands. */
(() => {
  const toolbar=document.createElement('div');toolbar.id='selection-toolbar';toolbar.hidden=true;
  toolbar.setAttribute('role','toolbar');toolbar.setAttribute('aria-label','选区快速格式');
  const font=document.createElement('select');font.setAttribute('aria-label','选区字体');
  const size=document.createElement('select');size.setAttribute('aria-label','选区字号');
  toolbar.append(font,size);
  const commands=[['B','加粗','btn-bold'],['I','斜体','btn-italic'],['U','下划线','btn-underline']];
  for(const [text,label,id] of commands){
    const button=document.createElement('button');button.type='button';button.textContent=text;
    button.title=label;button.setAttribute('aria-label',label);button.dataset.command=id;
    button.addEventListener('click',()=>{document.getElementById(id).click();setTimeout(sync,0);});
    toolbar.append(button);
  }
  for(const [text,label,kind] of [['A','字体颜色','font'],['▧','填充颜色','bg']]){
    const button=document.createElement('button');button.type='button';button.textContent=text;
    button.title=label;button.setAttribute('aria-label',label);button.className='quick-'+kind;
    button.addEventListener('click',()=>openColorPop(kind,button));toolbar.append(button);
  }
  for(const [label,delta] of [['增大字号',1],['减小字号',-1]]){
    const button=document.createElement('button');button.type='button';button.textContent=delta>0?'A⁺':'A⁻';
    button.title=label;button.setAttribute('aria-label',label);
    button.addEventListener('click',()=>{
      const source=document.getElementById('sel-fontsize');
      const value=Math.max(1,Math.min(409,Number(source.value||11)+delta));
      if(![...source.options].some(o=>Number(o.value)===value))source.add(new Option(String(value),String(value)));
      source.value=String(value);source.dispatchEvent(new Event('change'));sync();
    });toolbar.append(button);
  }
  const change=(target,sourceId)=>{
    const source=document.getElementById(sourceId);source.value=target.value;source.dispatchEvent(new Event('change'));
  };
  font.addEventListener('change',()=>change(font,'sel-fontfamily'));
  size.addEventListener('change',()=>change(size,'sel-fontsize'));
  toolbar.addEventListener('mousedown',event=>{
    if(event.target.closest('button'))event.preventDefault();
    event.stopPropagation();
  });
  toolbar.addEventListener('keydown',event=>event.stopPropagation());
  document.body.append(toolbar);
  function sync(){
    for(const [target,id] of [[font,'sel-fontfamily'],[size,'sel-fontsize']]){
      const source=document.getElementById(id);target.innerHTML=source.innerHTML;target.value=source.value;
    }
    syncButtons();
  }
  function syncButtons(){
    for(const button of toolbar.querySelectorAll('[data-command]')){
      button.setAttribute('aria-pressed',String(document.getElementById(button.dataset.command).classList.contains('active')));
    }
  }
  const formattingObserver=new MutationObserver(syncButtons);
  for(const [, , id] of commands)formattingObserver.observe(document.getElementById(id),{attributes:true,attributeFilter:['class']});
  const hide=()=>{toolbar.hidden=true;};
  function show(event){
    if(event.target.closest?.('#selection-toolbar,.color-pop'))return;
    if(!gridScroll.contains(event.target) && event.target!==formulaInput)return;
    let rect;
    if(S.editing){
      const selection=window.getSelection();
      if(selection?.rangeCount && !selection.isCollapsed && richCellEditor.contains(selection.anchorNode))rect=selection.getRangeAt(0).getBoundingClientRect();
      else if([formulaInput,editor].includes(event.target) && event.target.selectionStart!==event.target.selectionEnd)rect=event.target.getBoundingClientRect();
      else return hide();
    }else{
      const range=document.getElementById('sel-range');
      rect=(range.style.display==='none'?document.getElementById('sel-cursor'):range).getBoundingClientRect();
    }
    sync();toolbar.hidden=false;
    const width=toolbar.offsetWidth,height=toolbar.offsetHeight;
    const topBoundary=gridScroll.getBoundingClientRect().top;
    const top=rect.top-height-8>=topBoundary?rect.top-height-8:rect.bottom+8;
    toolbar.style.left=Math.max(8,Math.min(rect.left,innerWidth-width-8))+'px';
    toolbar.style.top=Math.max(topBoundary,Math.min(top,innerHeight-height-8))+'px';
  }
  document.addEventListener('mouseup',event=>setTimeout(()=>show(event),0));
  document.addEventListener('mousedown',event=>{if(!toolbar.contains(event.target)&&!event.target.closest?.('.color-pop,.cp-pop'))hide();});
  document.addEventListener('keydown',event=>{if(event.key==='Escape'||!toolbar.contains(event.target))hide();});
  gridScroll.addEventListener('scroll',hide,{passive:true});window.addEventListener('resize',hide);
})();
