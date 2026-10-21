import { createWiringScene } from './scene.js';

// This module is a read-only visual inspector. It never accesses motor APIs.
(() => {
 const node=document.getElementById('wiring-design');if(!node)return;
 const original=JSON.parse(node.textContent),$=id=>document.getElementById(id);
 let design=original,selectedNet='',selectedPin='',selectedModule='',engine;
 const modules=()=>new Map(design.modules.map(m=>[m.id,m]));
 const transform=(m,p)=>m.position.map((v,i)=>v+(m.basis||[[1,0,0],[0,1,0],[0,0,1]])[i].reduce((s,x,j)=>s+x*p[j],0));
 function pad(ref){const [mid,pid]=ref.split(':'),m=modules().get(mid),p=m.pins.find(p=>p.id===pid);return {module:m,pin:p,world:transform(m,p.position)};}
 const point=p=>p.pin?pad(p.pin).world:p;
 const length=n=>n.paths.reduce((s,path)=>s+path.slice(1).reduce((a,p,i)=>a+Math.hypot(...point(p).map((v,j)=>v-point(path[i])[j])),0),0);
 const membership=ref=>design.nets.find(n=>n.pins.includes(ref));
 const pretty=ref=>{const p=pad(ref),pid=p.pin.id;return p.module.name+' · '+({GND_POWER:'电源 − / GND',VM:'电源 + / 12V',GND_AUX:'GND',GND_P2:'P2 GND',VCC_P2:'P2 VCC', '3V3_OUT':'板载 3V3 输出'}[pid]||pid);};
 function paragraphs(lines){$('wireDetail').replaceChildren(...lines.map(text=>{const p=document.createElement('p');p.textContent=text;return p;}));}
 function inspect(){let title='全部接线',lines=['选择线路、模块或焊盘，查看它连接到哪里。颜色用于区分网络，不代表实际导线颜色。'];
  if(selectedPin){const p=pad(selectedPin),net=membership(selectedPin);title=pretty(selectedPin);lines=[net?'连接：'+net.pins.filter(r=>r!==selectedPin).map(pretty).join(' ↔ '):'当前不外接此端口。'+(selectedPin==='driver:3V3_OUT'?' 不要与 C3 3V3 并联。':''),'模块局部端点：'+p.pin.position.map(v=>v.toFixed(2)).join(', ')+' mm',p.module.evidence,'尺寸依据：'+p.module.confidence.dimensions,'引脚依据：'+p.module.confidence.pins];}
  else if(selectedNet){const n=design.nets.find(n=>n.id===selectedNet);title=n.name;lines=[n.pins.map(pretty).join(' ↔ '),'当前模型线路长 '+length(n).toFixed(1)+' mm，不含接头与预留。',n.id==='vm'?'12V 只给驱动功率端供电，不接 C3 GPIO、3V3 或 5V。':n.id==='v3'?'C3 3V3 接 AS5600 VCC、nSP 与 nRT；驱动板 3V3_OUT 留空。':n.id==='gnd'?'C3、编码器和驱动电源负极共地。同一驱动板的 GND 内部相连。':'保持现有 GPIO 分配；按丝印核对接线。'];}
  else if(selectedModule){const m=modules().get(selectedModule);title=m.name;lines=[m.evidence,'尺寸依据：'+m.confidence.dimensions,'引脚依据：'+m.confidence.pins];}
  $('wireInspectTitle').textContent=title;paragraphs(lines);$('wirePin').value=selectedPin;$('wireModule').value=selectedModule;$('wireFocus').disabled=!selectedModule;
  document.querySelectorAll('#wireNets [data-net]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.net===selectedNet)));
  engine?.select(selectedNet);
 }
 function selectNet(id){selectedNet=id||'';selectedPin='';selectedModule='';inspect();}
 function selectPin(ref){selectedPin=ref;selectedNet=ref?membership(ref)?.id||'':'';selectedModule=ref?ref.split(':')[0]:'';inspect();}
 function selectModule(id){selectedModule=id;selectedPin='';selectedNet='';inspect();}
 function selected(hit){if(hit.pin)selectPin(hit.pin);else if(hit.net)selectNet(hit.net);else if(hit.module)selectModule(hit.module);}
 $('wireNets').replaceChildren(...original.nets.map(n=>{const b=document.createElement('button');b.type='button';b.dataset.net=n.id;b.className='wire-net';b.setAttribute('aria-pressed','false');const dot=document.createElement('i');dot.style.background=n.color;const count=document.createElement('small');count.textContent=n.pins.length+' 点';b.append(dot,document.createTextNode(n.name),count);b.onclick=()=>selectNet(selectedNet===n.id?'':n.id);return b;}));
 for(const m of original.modules){const o=document.createElement('option');o.value=m.id;o.textContent=m.name;$('wireModule').append(o);const group=document.createElement('optgroup');group.label=m.name;for(const p of m.pins){const option=document.createElement('option');option.value=m.id+':'+p.id;option.textContent=p.id;group.append(option);}$('wirePin').append(group);}
 $('wireModule').onchange=e=>selectModule(e.target.value);$('wirePin').onchange=e=>selectPin(e.target.value);$('wireAll').onclick=()=>selectNet('');$('wireFocus').onclick=()=>engine?.focus(selectedModule);
 function metrics(){const d=engine?.envelope();$('wireEnvelope').textContent=d?d.map(v=>v.toFixed(1)).join(' × ')+' mm':'—';$('wireLength').textContent=(design.nets.reduce((s,n)=>s+length(n),0)/1000).toFixed(2)+' m';$('wireLayoutLabel').textContent=design===original?'装配总览':'编码器展开';}
 function changeLayout(mode){design=mode==='exploded'?original.exploded:original;engine?.setDesign(design);document.querySelectorAll('[data-wire-layout]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.wireLayout===mode)));document.querySelectorAll('[data-wire-view]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.wireView==='iso')));inspect();metrics();}
 document.querySelectorAll('[data-wire-layout]').forEach(b=>b.onclick=()=>changeLayout(b.dataset.wireLayout));
 document.querySelectorAll('[data-wire-view]').forEach(b=>b.onclick=()=>{engine?.view(b.dataset.wireView);document.querySelectorAll('[data-wire-view]').forEach(v=>v.setAttribute('aria-pressed',String(v===b)));});
 for(const [id,method] of [['wireLabels','labels'],['wireGlass','ghost'],['wireWires','wires'],['wireGrid','grid']])$(id).onchange=e=>engine?.[method](e.target.checked);
 $('wireReset').onclick=()=>{engine?.view('iso');document.querySelectorAll('[data-wire-view]').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.wireView==='iso')));};
 $('wireFullscreen').onclick=async()=>{const host=$('wireStage');try{if(document.fullscreenElement)await document.exitFullscreen();else await host.requestFullscreen();}catch{host.classList.toggle('wire-expanded');engine?.resize();}};
 $('wireNotes').replaceChildren(...original.notes.map(text=>{const p=document.createElement('p');p.textContent=text;return p;}));
 try{engine=createWiringScene($('wiringCanvas'),design,selected);$('wireLoading').hidden=true;metrics();inspect();}
 catch(error){$('wireLoading').textContent='3D 视图启动失败：'+error.message+'。请使用支持 WebGL 的浏览器；下方接线表仍可查看。';}
})();
