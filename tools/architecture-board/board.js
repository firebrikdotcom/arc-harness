/* Architecture board renderer: fixed cards in frames, typed edges, pan/zoom, details panel.
   Data comes from board.json (see docs/architecture-board.md §3); nothing project-specific lives here.
   Usage: ArchitectureBoard.mount(rootElement, board, {status}) -> {setOverlay, setStatus} */
(function(){
"use strict";
const CW=320,RH=204,W=240,H=112,FP=20,FH=46,MINK=.2,MAXK=2.5;
const TONES=["orange","blue","green","teal","red","purple","gray","slate","amber","pink","indigo","brown"];
const ICONS={
  person:'<circle cx="12" cy="7" r="3.5"/><path d="M5 21c0-4 3-6.5 7-6.5s7 2.5 7 6.5"/>',
  screen:'<rect x="6" y="2.5" width="12" height="19" rx="2.5"/><path d="M10 18.5h4"/>',
  hub:'<circle cx="12" cy="12" r="3"/><path d="M12 3v6M12 15v6M3 12h6M15 12h6"/>',
  db:'<ellipse cx="12" cy="6" rx="7" ry="3"/><path d="M5 6v12c0 1.7 3.1 3 7 3s7-1.3 7-3V6M5 12c0 1.7 3.1 3 7 3s7-1.3 7-3"/>',
  bell:'<path d="M6 16V11a6 6 0 0 1 12 0v5l1.5 2h-15z"/><path d="M10 20.5a2 2 0 0 0 4 0"/>',
  plug:'<path d="M9 3v5M15 3v5M6.5 8h11v3a5.5 5.5 0 0 1-11 0zM12 16.5V21"/>',
  term:'<rect x="3" y="4" width="18" height="16" rx="2.5"/><path d="m7 9 3 3-3 3M13 15h4"/>',
  cloud:'<path d="M7 18.5a4.5 4.5 0 0 1-.5-9A6 6 0 0 1 18 9a4.5 4.5 0 0 1-.5 9.5z"/>',
  grid:'<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M3 9h18M3 15h18M9 3v18M15 3v18"/>',
  server:'<rect x="3" y="4" width="18" height="7" rx="1.5"/><rect x="3" y="13" width="18" height="7" rx="1.5"/><path d="M7 7.5h.01M7 16.5h.01"/>',
  layers:'<path d="m12 3 9 5-9 5-9-5z"/><path d="m3 13 9 5 9-5"/>',
  android:'<rect x="5" y="9" width="14" height="10" rx="2"/><path d="M8 9a4 4 0 0 1 8 0M8 5 6.5 3M16 5l1.5-2M10 13h.01M14 13h.01"/>',
  key:'<circle cx="8" cy="15" r="4"/><path d="m11 12 9-9M17 6l2 2M15 8l2 2"/>',
  flow:'<path d="M4 6h10M4 12h16M4 18h7"/><circle cx="17" cy="6" r="2"/><circle cx="14" cy="18" r="2"/>',
  chat:'<path d="M4 5h16v11H9l-5 4z"/>',
  kbd:'<rect x="2.5" y="6" width="19" height="12" rx="2"/><path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M7 14h10"/>',
  rpc:'<path d="M4 8h12l-3-3M20 16H8l3 3"/>',
  box:'<path d="m12 2.5 8.5 4.5v10L12 21.5 3.5 17V7z"/><path d="M3.5 7 12 11.5 20.5 7M12 11.5v10"/>',
  check:'<rect x="3.5" y="3.5" width="17" height="17" rx="3"/><path d="m8 12 3 3 5-6"/>',
  globe:'<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/>',
  gear:'<circle cx="12" cy="12" r="3"/><path d="M12 2v3M12 19v3M4.9 4.9 7 7M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1 7 17M17 7l2.1-2.1"/>',
  lock:'<rect x="4.5" y="10.5" width="15" height="10" rx="2"/><path d="M8 10.5V7a4 4 0 0 1 8 0v3.5"/>',
  doc:'<path d="M6 2.5h8l4 4v15H6z"/><path d="M14 2.5v4h4M9 12h6M9 16h6"/>',
  code:'<path d="m8 7-5 5 5 5M16 7l5 5-5 5M14 4l-4 16"/>',
  widget:'<rect x="3" y="3" width="8" height="8" rx="2"/><rect x="13" y="3" width="8" height="8" rx="2"/><rect x="3" y="13" width="8" height="8" rx="2"/><rect x="13" y="13" width="8" height="8" rx="2"/>',
  clock:'<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
  dot:'<circle cx="12" cy="12" r="4"/>'
};
const STYLES={solid:{dash:"",w:1.6},dashed:{dash:"7 5",w:2},dotted:{dash:"2 4",w:1.6},faint:{dash:"1 5",w:1.4}};
const DEFAULT_KINDS={
  call:{label:"In-process call",style:"solid",tone:"slate"},
  remote:{label:"Over the network",style:"dashed",tone:"red"},
  platform:{label:"OS or external service",style:"dotted",tone:"purple"},
  build:{label:"Build / test dependency",style:"faint",tone:"gray"}
};
const esc=s=>String(s==null?"":s).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const tone=t=>`var(--t-${TONES.includes(t)?t:"slate"})`;
const base=p=>p.replace(/\/$/,"").split("/").pop()+(p.endsWith("/")?"/":"");

function mount(root,data,opts={}){
  const B=normalise(data);
  const kinds=B.edge_kinds;
  root.classList.add("ab");
  try{const t=localStorage.getItem("ab-theme");if(t)root.dataset.theme=t}catch{}
  root.innerHTML=`
  <div class="ab-bar">
    <h1>${esc(B.title)} <span>· architecture board</span></h1>
    <span class="ab-status" data-r="status"><i></i><span></span></span>
    <div class="ab-search">
      <svg viewBox="0 0 24 24" fill="none" stroke-width="2.4" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>
      <input data-r="q" type="search" placeholder="Find a component or file  ( / )" autocomplete="off" aria-label="Find a component or file">
      <div class="ab-results" data-r="results" role="listbox"></div>
    </div>
    <div class="ab-tools">
      <button class="ab-tbtn" data-r="zout" title="Zoom out (−)" aria-label="Zoom out">−</button>
      <span class="ab-zoomv" data-r="zv">100%</span>
      <button class="ab-tbtn" data-r="zin" title="Zoom in (+)" aria-label="Zoom in">+</button>
      <button class="ab-tbtn" data-r="fit" title="Fit the whole board (0)">Fit</button>
      <button class="ab-tbtn ab-heatbtn" data-r="heat" title="Colour components by their recent errors" aria-pressed="true" hidden>Heat</button>
      <button class="ab-tbtn" data-r="theme" title="Switch light / dark" aria-label="Switch light or dark theme"><svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z"/></svg></button>
    </div>
    <nav class="ab-frames" data-r="frames" aria-label="Jump to a section"></nav>
  </div>
  <div class="ab-main">
    <div class="ab-vp" data-r="vp" tabindex="0" aria-label="Architecture board. Drag to pan, wheel or pinch to zoom.">
      <div class="ab-world" data-r="world"><svg class="ab-edges" data-r="edges" aria-hidden="true"><defs data-r="defs"></defs></svg></div>
      <div class="ab-hint">Drag to pan · wheel or pinch to zoom · click a card for details · <b>Esc</b> clears</div>
    </div>
    <aside class="ab-panel" data-r="panel" aria-live="polite"></aside>
  </div>
  <div class="ab-toast" data-r="toast" role="status"></div>`;
  const $=r=>root.querySelector(`[data-r="${r}"]`);
  const vp=$("vp"),world=$("world"),svg=$("edges"),panel=$("panel");
  const frameById=new Map(B.frames.map(f=>[f.id,f]));
  const nodeById=new Map(B.nodes.map(n=>[n.id,n]));
  const cardEl=new Map(),edgeEl=new Map(),labelEl=new Map();
  const hiddenKinds=new Set();
  let view={x:0,y:0,k:1},selected=null,activeFlow=null,bounds={x:0,y:0,w:1,h:1};
  let overlay={edited:{},inScope:[],refreshing:[],unownedEdited:[],heat:null},lastOverlay={};
  let heatOn=true;try{heatOn=localStorage.getItem("ab-heat")!=="off"}catch{}
  const px=col=>col*CW,py=row=>row*RH+FH;
  const colorOf=fid=>tone((frameById.get(fid)||{}).tone);
  const kindColor=k=>tone((kinds[k]||{}).tone);
  const frameRect=f=>({x:px(f.col)-FP,y:py(f.row)-FH,w:f.cols*CW-(CW-W)+2*FP,h:f.rows*RH-(RH-H)+FH+FP});
  const nodeRect=n=>({x:px(n.col),y:py(n.row),w:W,h:H});
  const fileHref=p=>{
    if(!B.repo_url)return null;
    const dir=p.endsWith("/");
    return B.repo_url.replace(/\/$/,"")+(dir?"/tree/":"/blob/")+encodeURIComponent(B.default_branch||"main")+"/"+p.replace(/\/$/,"").split("/").map(encodeURIComponent).join("/");
  };

  function buildBoard(){
    let minX=Infinity,minY=Infinity,maxX=0,maxY=0;
    for(const f of B.frames){
      const r=frameRect(f),el=document.createElement("div");
      el.className="ab-frame"+(f.external?" external":"");el.style.cssText=`left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;--c:${tone(f.tone)}`;
      el.innerHTML=`<header><h2>${esc(f.title)}</h2><span>${esc(f.tag)}</span></header>`;
      world.insertBefore(el,svg);
      minX=Math.min(minX,r.x);minY=Math.min(minY,r.y);maxX=Math.max(maxX,r.x+r.w);maxY=Math.max(maxY,r.y+r.h);
    }
    for(const n of B.nodes){
      const r=nodeRect(n),el=document.createElement("button");
      el.className="ab-card"+(n.external?" external":"");el.dataset.id=n.id;
      el.style.cssText=`left:${r.x}px;top:${r.y}px;--c:${colorOf(n.frame)}`;
      const meta=[n.files.length?`<span><b>${n.files.length}</b> file${n.files.length>1?"s":""}</span>`:"",n.tests.length?`<span>· <b>${n.tests.length}</b> test${n.tests.length>1?"s":""}</span>`:"",n.external?"<span>outside the code</span>":""].join("");
      el.innerHTML=`<span class="ab-badges"></span><span class="t"><svg viewBox="0 0 24 24">${ICONS[n.icon]||ICONS.dot}</svg>${esc(n.title)}</span><span class="s">${esc(n.sub)}</span><span class="m">${meta}</span>`;
      el.setAttribute("aria-label",`${n.title}: ${n.sub}`);
      world.appendChild(el);cardEl.set(n.id,el);
      minX=Math.min(minX,r.x);minY=Math.min(minY,r.y);maxX=Math.max(maxX,r.x+r.w);maxY=Math.max(maxY,r.y+r.h);
    }
    svg.setAttribute("width",maxX+40);svg.setAttribute("height",maxY+40);
    $("defs").innerHTML=Object.keys(kinds).map(k=>`<marker id="ab-arr-${esc(k)}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 1 9 5 0 9z" style="fill:${kindColor(k)}"/></marker>`).join("");
    routeEdges();
    bounds=B.nodes.length||B.frames.length?{x:minX,y:minY,w:maxX-minX,h:maxY-minY}:{x:0,y:0,w:1,h:1};
  }

  /* Each edge leaves and enters through the card side facing the other card; several edges on one side are spread out. */
  const opp={l:"r",r:"l",t:"b",b:"t"};
  function side(a,b){const dx=b.col-a.col,dy=b.row-a.row;if(Math.abs(dx)>=Math.abs(dy))return dx>0?"r":"l";return dy>0?"b":"t"}
  function routeEdges(){
    const ports=new Map();
    const add=(key,e,end,other)=>{if(!ports.has(key))ports.set(key,[]);ports.get(key).push({e,end,other})};
    for(const e of B.edges){
      const a=nodeById.get(e.from),b=nodeById.get(e.to),s=side(a,b);
      /* Along a row or column with cards in between: leave and enter on the same outer side so the line arcs around them. */
      if(a.row===b.row&&Math.abs(a.col-b.col)>1){e.sa=e.sb="b";e.arc=true}
      else if(a.col===b.col&&Math.abs(a.row-b.row)>1){e.sa=e.sb=a.col===0?"l":"r";e.arc=true}
      else{e.sa=s;e.sb=opp[s]}
      add(a.id+"\u0000"+e.sa,e,"a",b);add(b.id+"\u0000"+e.sb,e,"b",a);
    }
    for(const [key,list] of ports){
      const s=key.slice(-1),horiz=s==="t"||s==="b";
      list.sort((p,q)=>horiz?(p.other.col-q.other.col)||(p.other.row-q.other.row):(p.other.row-q.other.row)||(p.other.col-q.other.col));
      list.forEach((p,i)=>{p.e[p.end==="a"?"ta":"tb"]=(i+1)/(list.length+1)});
    }
    for(const e of B.edges){
      const a=anchor(nodeById.get(e.from),e.sa,e.ta),b=anchor(nodeById.get(e.to),e.sb,e.tb);
      const d=e.arc?62:Math.max(36,Math.min(150,Math.hypot(b.x-a.x,b.y-a.y)*.4));
      const c1={x:a.x+a.nx*d,y:a.y+a.ny*d},c2={x:b.x+b.nx*d,y:b.y+b.ny*d};
      const st=STYLES[(kinds[e.kind]||{}).style]||STYLES.solid;
      const path=document.createElementNS("http://www.w3.org/2000/svg","path");
      path.setAttribute("d",`M${a.x} ${a.y}C${c1.x} ${c1.y} ${c2.x} ${c2.y} ${b.x} ${b.y}`);
      path.setAttribute("class","edge");path.setAttribute("marker-end",`url(#ab-arr-${e.kind})`);
      path.style.stroke=kindColor(e.kind);path.style.strokeDasharray=st.dash;path.style.strokeWidth=st.w;
      svg.appendChild(path);edgeEl.set(e.id,path);
      const m=labelSpot(a,c1,c2,b,e.label),lab=document.createElement("button");
      lab.className="ab-elabel";lab.textContent=e.label;lab.dataset.edge=e.id;lab.tabIndex=-1;
      if(e.kind!=="call")lab.style.color=kindColor(e.kind);
      lab.style.left=m.x+"px";lab.style.top=m.y+"px";world.appendChild(lab);labelEl.set(e.id,lab);
      if(!e.label)lab.style.display="none";
    }
  }
  /* Labels sit on their curve at the first spot that covers no card and no earlier label. */
  const placed=[];
  function labelSpot(a,c1,c2,b,text){
    const w=String(text).length*6.2+14,h=18;
    const hits=(m,r)=>Math.abs(m.x-(r.x+r.w/2))<(w+r.w)/2+3&&Math.abs(m.y-(r.y+r.h/2))<(h+r.h)/2+3;
    let best=null;
    for(const t of [.5,.42,.58,.34,.66,.26,.74,.2,.8]){
      const m=bez(a,c1,c2,b,t);
      if(B.nodes.some(n=>hits(m,nodeRect(n))))continue;
      if(placed.some(r=>hits(m,r)))continue;
      best=m;break;
    }
    best=best||bez(a,c1,c2,b,.5);
    placed.push({x:best.x-w/2,y:best.y-h/2,w,h});
    return best;
  }
  function anchor(n,s,t=.5){
    const r=nodeRect(n);
    if(s==="l")return {x:r.x,y:r.y+r.h*t,nx:-1,ny:0};
    if(s==="r")return {x:r.x+r.w,y:r.y+r.h*t,nx:1,ny:0};
    if(s==="t")return {x:r.x+r.w*t,y:r.y,nx:0,ny:-1};
    return {x:r.x+r.w*t,y:r.y+r.h,nx:0,ny:1};
  }
  function bez(p0,p1,p2,p3,t){const u=1-t;return {x:u*u*u*p0.x+3*u*u*t*p1.x+3*u*t*t*p2.x+t*t*t*p3.x,y:u*u*u*p0.y+3*u*u*t*p1.y+3*u*t*t*p2.y+t*t*t*p3.y}}

  /* ---------- Pan and zoom ---------- */
  function apply(){
    world.style.transform=`translate(${view.x}px,${view.y}px) scale(${view.k})`;
    vp.style.backgroundSize=`${22*view.k}px ${22*view.k}px`;
    vp.style.backgroundPosition=`${view.x}px ${view.y}px`;
    $("zv").textContent=Math.round(view.k*100)+"%";
    world.classList.toggle("quiet",view.k<.55);
  }
  function zoomAt(k,cx,cy){k=Math.max(MINK,Math.min(MAXK,k));view.x=cx-(cx-view.x)*(k/view.k);view.y=cy-(cy-view.y)*(k/view.k);view.k=k;apply()}
  const isNarrow=()=>matchMedia("(max-width:820px)").matches;
  function visibleBox(){
    const r=vp.getBoundingClientRect();let h=r.height;
    if(isNarrow())h-=panel.classList.contains("open")?panel.getBoundingClientRect().height:46;
    return {w:r.width,h:Math.max(120,h)};
  }
  function fitRect(x,y,w,h,maxK=1.15,animate=true,minK=MINK){
    const b=visibleBox(),pad=24;
    const k=Math.max(minK,Math.min(maxK,(b.w-pad*2)/w,(b.h-pad*2)/h));
    animateTo({k,x:(b.w-w*k)/2-x*k,y:(b.h-h*k)/2-y*k},animate);
  }
  function fitAll(animate=true){fitRect(bounds.x,bounds.y,bounds.w,bounds.h,1.15,animate)}
  let anim=0;
  function animateTo(to,animate){
    cancelAnimationFrame(anim);
    if(!animate||matchMedia("(prefers-reduced-motion:reduce)").matches){view={...to};apply();return}
    const from={...view},t0=performance.now(),D=280;
    const step=now=>{const t=Math.min(1,(now-t0)/D),e=1-Math.pow(1-t,3);
      view={x:from.x+(to.x-from.x)*e,y:from.y+(to.y-from.y)*e,k:from.k+(to.k-from.k)*e};apply();
      if(t<1)anim=requestAnimationFrame(step)};
    anim=requestAnimationFrame(step);
  }
  function boxOf(ids){
    const rs=[...ids].map(i=>nodeRect(nodeById.get(i)));
    const x=Math.min(...rs.map(r=>r.x)),y=Math.min(...rs.map(r=>r.y));
    return {x:x-30,y:y-30,w:Math.max(...rs.map(r=>r.x+r.w))-x+60,h:Math.max(...rs.map(r=>r.y+r.h))-y+60};
  }
  /* Show a component with everything it connects to; stay put if that is already on screen at a readable size. */
  function frameNeighbourhood(ids){
    const r=boxOf(ids),b=visibleBox();
    const inView=r.x*view.k+view.x>=0&&(r.x+r.w)*view.k+view.x<=b.w&&r.y*view.k+view.y>=0&&(r.y+r.h)*view.k+view.y<=b.h;
    if(inView&&view.k>=.5)return;
    fitRect(r.x,r.y,r.w,r.h,1,true,isNarrow()?.4:.5);
  }

  /* A press that moves less than 5px is a click on whatever is under it; anything more pans the board. */
  const pointers=new Map();let drag=null,pinch=null;
  vp.addEventListener("pointerdown",e=>{
    if(e.pointerType==="mouse"&&e.button!==0)return;
    pointers.set(e.pointerId,{x:e.clientX,y:e.clientY});
    cancelAnimationFrame(anim);
    if(pointers.size===1)drag={x:e.clientX,y:e.clientY,vx:view.x,vy:view.y,moved:false,target:e.target};
    else if(pointers.size===2){const [p,q]=[...pointers.values()];pinch={d:Math.hypot(p.x-q.x,p.y-q.y)||1,k:view.k};if(drag)drag.moved=true}
  });
  vp.addEventListener("pointermove",e=>{
    if(!pointers.has(e.pointerId))return;
    pointers.set(e.pointerId,{x:e.clientX,y:e.clientY});
    const r=vp.getBoundingClientRect();
    if(pinch&&pointers.size===2){const [p,q]=[...pointers.values()],d=Math.hypot(p.x-q.x,p.y-q.y);zoomAt(pinch.k*d/pinch.d,(p.x+q.x)/2-r.left,(p.y+q.y)/2-r.top);return}
    if(!drag)return;
    const dx=e.clientX-drag.x,dy=e.clientY-drag.y;
    if(!drag.moved&&Math.hypot(dx,dy)>5){drag.moved=true;vp.classList.add("panning");try{vp.setPointerCapture(e.pointerId)}catch{}}
    if(drag.moved){view.x=drag.vx+dx;view.y=drag.vy+dy;apply()}
  });
  function endPointer(e){
    if(!pointers.has(e.pointerId))return;
    pointers.delete(e.pointerId);
    if(pointers.size<2)pinch=null;
    if(pointers.size===0&&drag){
      if(!drag.moved&&e.type==="pointerup"){
        const card=drag.target.closest(".ab-card"),lab=drag.target.closest(".ab-elabel");
        if(card)select(card.dataset.id);else if(lab)selectEdge(lab.dataset.edge);else clearSelection();
      }
      drag=null;vp.classList.remove("panning");
    }else if(pointers.size===1){const [p]=[...pointers.values()];drag={x:p.x,y:p.y,vx:view.x,vy:view.y,moved:true,target:vp}}
  }
  vp.addEventListener("pointerup",endPointer);vp.addEventListener("pointercancel",endPointer);
  vp.addEventListener("wheel",e=>{
    e.preventDefault();const r=vp.getBoundingClientRect();
    const dy=e.deltaMode===1?e.deltaY*16:e.deltaY;
    zoomAt(view.k*Math.exp(-dy*.0016),e.clientX-r.left,e.clientY-r.top);
  },{passive:false});
  /* Keyboard activation of a focused card (Enter/Space) arrives as a click with detail 0. */
  world.addEventListener("click",e=>{if(e.detail!==0)return;const c=e.target.closest(".ab-card");if(c)select(c.dataset.id)});

  /* ---------- Selection and panel ---------- */
  const neighbours=id=>B.edges.filter(e=>!hiddenKinds.has(e.kind)&&(e.from===id||e.to===id));
  function highlight(nodeIds,edgeIds){
    world.classList.toggle("dim",!!nodeIds);
    for(const [id,el] of cardEl){el.classList.toggle("lit",!!nodeIds&&nodeIds.has(id));el.classList.toggle("sel",id===selected)}
    for(const [id,el] of edgeEl){const on=!!edgeIds&&edgeIds.has(id);el.classList.toggle("lit",on);labelEl.get(id).classList.toggle("lit",on)}
  }
  function select(id){
    const n=nodeById.get(id);if(!n)return;
    selected=id;activeFlow=null;
    const es=neighbours(id),ns=new Set([id]);es.forEach(e=>{ns.add(e.from);ns.add(e.to)});
    highlight(ns,new Set(es.map(e=>e.id)));
    renderNode(n,es);openPanel();
    history.replaceState(null,"","#"+encodeURIComponent(id));
    frameNeighbourhood(ns);
  }
  function selectEdge(eid){
    const e=B.edges.find(x=>x.id===eid);if(!e)return;
    selected=null;activeFlow=null;
    highlight(new Set([e.from,e.to]),new Set([eid]));
    const a=nodeById.get(e.from),b=nodeById.get(e.to);
    panel.style.setProperty("--c",kindColor(e.kind));
    panel.innerHTML=`<button class="ab-close" data-act="clear" aria-label="Close">×</button>
      <div class="kicker">Connection · ${esc((kinds[e.kind]||{}).label||e.kind)}</div>
      <h3>${esc(e.label||"Connection")}</h3><p class="sub">${esc(a.title)} → ${esc(b.title)}</p>
      <div class="ab-conns">${connBtn(a,"from")}${connBtn(b,"to")}</div>`;
    openPanel();
  }
  function connBtn(n,dir,label){
    const arrow=dir==="from"?"←":"→";
    return `<button class="ab-link" data-go="${esc(n.id)}" style="--c2:${colorOf(n.frame)}"><span class="dir">${arrow}</span><b>${esc(n.title)}</b><small>${esc(label||(frameById.get(n.frame)||{}).title||"")}</small></button>`;
  }
  function fileItem(p,note,edited){
    const href=fileHref(p);
    return `<li class="${edited?"edited":""}"><code>${esc(p)}</code><span class="acts"><button class="ab-ibtn" data-copy="${esc(p)}" title="Copy path">Copy</button>${href?`<a class="ab-ibtn" href="${esc(href)}" target="_blank" rel="noopener" title="Open on the forge">Open ↗</a>`:""}</span>${note?`<span class="note">${esc(note)}</span>`:""}</li>`;
  }
  const LEVELS=["fatal","error","warning","info"];
  function ago(iso){
    const t=new Date(iso||NaN).getTime();if(!Number.isFinite(t))return "at an unknown time";
    const m=Math.round((Date.now()-t)/60000);
    return m<1?"just now":m<60?`${m}m ago`:m<2880?`${Math.round(m/60)}h ago`:`${Math.round(m/1440)}d ago`;
  }
  function issueItem(i){
    const lv=LEVELS.includes(i.level)?i.level:"error",n=+i.count||0,u=+i.users||0;
    const url=typeof i.url==="string"&&/^https?:\/\//.test(i.url)?i.url:null;
    return `<li class="lv-${lv}"><span class="lv">${lv}</span><b>${esc(i.title)}</b>
      <small>${n} event${n===1?"":"s"}${u?` · ${u} user${u===1?"":"s"}`:""}${i.last_seen?` · last ${esc(ago(i.last_seen))}`:""}${i.share==="caller"?" · on the call path":""}${i.new?" · <em>new</em>":""}${i.source?` · ${esc(String(i.source).split(":")[0])}`:""}</small>
      ${url?`<a class="ab-ibtn" href="${esc(url)}" target="_blank" rel="noopener noreferrer">Open ↗</a>`:""}</li>`;
  }
  function renderNode(n,es){
    const f=frameById.get(n.frame)||{title:"",tag:""};
    panel.style.setProperty("--c",colorOf(n.frame));
    const out=es.filter(e=>e.from===n.id),inn=es.filter(e=>e.to===n.id);
    const edited=overlay.edited[n.id]||[],editedSet=new Set(edited);
    const hc=heatOn&&overlay.heat&&overlay.heat.cards[n.id];
    const problems=hc&&Array.isArray(hc.top)&&hc.top.length?`<h4>Problems</h4><ul class="ab-issues">${hc.top.map(issueItem).join("")}</ul>${hc.issues>hc.top.length?`<p class="ab-note">${hc.issues-hc.top.length} more issue${hc.issues-hc.top.length>1?"s":""} touch this component.</p>`:""}`:"";
    const live=[
      edited.length?`<div class="live"><b>Edited now:</b> ${edited.length} uncommitted file${edited.length>1?"s":""} in this component.<ul class="files" style="margin-top:6px">${edited.map(p=>fileItem(p,"",true)).join("")}</ul></div>`:"",
      overlay.inScope.includes(n.id)?`<div class="live scope"><b>In scope</b> for the active harness run.</div>`:"",
      overlay.refreshing.includes(n.id)?`<div class="live scope"><b>Refreshing:</b> the background mapper is re-describing this card.</div>`:""
    ].join("");
    panel.innerHTML=`<button class="ab-close" data-act="clear" aria-label="Close">×</button>
      <div class="kicker">${esc(f.title)}${f.tag?" · "+esc(f.tag):""}</div>
      <h3>${esc(n.title)}</h3><p class="sub">${esc(n.sub)}</p>
      ${live}
      ${problems}
      <p>${esc(n.summary)}</p>
      ${n.responsibility?`<div class="resp">${esc(n.responsibility)}</div>`:""}
      ${es.length?`<h4>Connections</h4><div class="ab-conns">${out.map(e=>connBtn(nodeById.get(e.to),"to",e.label)).join("")}${inn.map(e=>connBtn(nodeById.get(e.from),"from",e.label)).join("")}</div>`:""}
      ${n.files.length?`<h4>Source files</h4><ul class="files">${n.files.map(x=>fileItem(x.path,x.note,editedSet.has(x.path))).join("")}</ul>`:""}
      ${n.tests.length?`<h4>Tests</h4><ul class="files">${n.tests.map(t=>fileItem(t,"",editedSet.has(t))).join("")}</ul>`:""}
      ${!n.files.length&&!n.tests.length?`<p class="ab-note">Outside this repository: nothing to open here.</p>`:""}`;
    panel.scrollTop=0;
  }
  function renderOverview(){
    panel.style.removeProperty("--c");
    const editedIds=Object.keys(overlay.edited).filter(id=>nodeById.has(id));
    const liveBox=editedIds.length||overlay.unownedEdited.length?`<h4>What you are touching</h4>
      <div class="ab-conns">${editedIds.map(id=>{const n=nodeById.get(id);return `<button class="ab-link" data-go="${esc(id)}" style="--c2:var(--edited)"><span class="dir">●</span><b>${esc(n.title)}</b><small>${overlay.edited[id].length} uncommitted file${overlay.edited[id].length>1?"s":""}</small></button>`}).join("")}</div>
      ${overlay.unownedEdited.length?`<p class="ab-note" style="margin-top:8px">${overlay.unownedEdited.length} edited file${overlay.unownedEdited.length>1?"s":""} belong to no card yet; the next refresh places them.</p>`:""}`:"";
    const H=heatOn&&overlay.heat;
    let heatBox="";
    if(H){
      const hot=Object.entries(H.cards||{}).filter(([id,c])=>nodeById.has(id)&&c&&c.heat>0).sort((a,b)=>b[1].heat-a[1].heat).slice(0,5);
      const failing=Object.entries(H.sources||{}).filter(([,s])=>s&&s.ok===false);
      const unmapped=(H.totals&&H.totals.unmapped)||0;
      heatBox=`<h4>Where errors happen</h4>
      ${hot.length?`<div class="ab-conns">${hot.map(([id,c])=>`<button class="ab-link" data-go="${esc(id)}" style="--c2:var(--hot)"><span class="dir" style="color:var(--hot)">●</span><b>${esc(nodeById.get(id).title)}</b><small>${+c.issues||0} issue${c.issues===1?"":"s"} · heat ${Math.round((+c.heat||0)*100)}%</small></button>`).join("")}</div>`:`<p class="ab-note">No open errors map to a component right now.</p>`}
      ${unmapped?`<p class="ab-note" style="margin-top:8px">${unmapped} open issue${unmapped>1?"s":""} map to no card: their stack frames are outside the board's files, or need a path_map rule.</p>`:""}
      ${H.stale?`<p class="ab-note" style="margin-top:8px">Error data is stale: last updated ${esc(ago(H.generated_at))}. Is harness-signals running?</p>`:""}
      ${failing.map(([src,s])=>`<p class="ab-note" style="margin-top:8px"><b>${esc(src)}</b> failed: ${esc(s.error||"unknown error")}</p>`).join("")}`;
    }
    panel.innerHTML=`<div class="kicker">How to read this board</div>
      <h3>${esc(B.overview.heading||B.title)}</h3>
      <p class="sub">Each card is a component; each coloured frame is where it lives. Positions are fixed: drag the board, not the cards.</p>
      ${B.overview.text?`<p>${esc(B.overview.text)}</p>`:""}
      ${liveBox}
      ${heatBox}
      <h4>Lines</h4>
      <div class="ab-legend">${Object.entries(kinds).filter(([k])=>B.edges.some(e=>e.kind===k)).map(([k,v])=>{const st=STYLES[v.style]||STYLES.solid;return `<label><input type="checkbox" data-kind="${esc(k)}" ${hiddenKinds.has(k)?"":"checked"}><svg viewBox="0 0 42 10"><path d="M2 5H40" style="fill:none;stroke:${kindColor(k)};stroke-width:2;stroke-dasharray:${st.dash}"/></svg>${esc(v.label)}</label>`}).join("")}</div>
      ${B.flows.length?`<h4>Walk through a flow</h4><div class="ab-flows">${B.flows.map(fl=>`<button class="ab-flow" data-flow="${esc(fl.id)}" aria-pressed="${activeFlow===fl.id}"><b>${esc(fl.title)}</b><small>${esc(fl.sub)}</small></button>`).join("")}</div><div data-r="flowtext"></div>`:""}
      <p class="ab-note">${esc(B.footer||"Static architecture plus your working state. Dashed cards are outside the code.")}<br>Keys: <b>/</b> search · <b>+</b> / <b>−</b> zoom · <b>0</b> fit · arrows pan · <b>Esc</b> clear.</p>`;
    if(activeFlow){const fl=B.flows.find(f=>f.id===activeFlow);$("flowtext").innerHTML=`<ol class="ab-steps">${fl.text.map(t=>`<li>${esc(t)}</li>`).join("")}</ol>`}
  }
  function runFlow(id){
    if(activeFlow===id){clearSelection();return}
    selected=null;activeFlow=id;
    const fl=B.flows.find(f=>f.id===id),ns=new Set(),es=new Set();
    for(const [a,b] of fl.steps){if(!nodeById.has(a)||!nodeById.has(b))continue;ns.add(a);ns.add(b);const e=B.edges.find(x=>(x.from===a&&x.to===b)||(x.from===b&&x.to===a));if(e)es.add(e.id)}
    highlight(ns,es);renderOverview();openPanel();
    if(ns.size){const r=boxOf(ns);fitRect(r.x,r.y,r.w,r.h,1.1)}
  }
  function clearSelection(){
    const had=selected||activeFlow;
    selected=null;activeFlow=null;highlight(null,null);renderOverview();
    if(location.hash)history.replaceState(null,"",location.pathname+location.search);
    if(had)panel.classList.remove("open");
  }
  function openPanel(){panel.classList.add("open")}
  panel.addEventListener("click",e=>{
    const go=e.target.closest("[data-go]");if(go){select(go.dataset.go);return}
    if(e.target.closest("[data-act=clear]")){clearSelection();return}
    const fl=e.target.closest("[data-flow]");if(fl){runFlow(fl.dataset.flow);return}
    const cp=e.target.closest("[data-copy]");if(cp){copy(cp.dataset.copy);return}
    if(isNarrow()&&!panel.classList.contains("open")&&!e.target.closest("a,input,label"))openPanel();
  });
  panel.addEventListener("change",e=>{
    const k=e.target.dataset.kind;if(!k)return;
    e.target.checked?hiddenKinds.delete(k):hiddenKinds.add(k);
    for(const ed of B.edges){const h=hiddenKinds.has(ed.kind);edgeEl.get(ed.id).classList.toggle("hidden",h);labelEl.get(ed.id).classList.toggle("hidden",h)}
  });
  function copy(text){
    const done=()=>toast("Copied "+text);
    const fallback=()=>{const t=document.createElement("textarea");t.value=text;root.appendChild(t);t.select();try{document.execCommand("copy");done()}catch{toast(text)}t.remove()};
    if(navigator.clipboard&&isSecureContext)navigator.clipboard.writeText(text).then(done,fallback);else fallback();
  }
  let toastT;function toast(m){const t=$("toast");t.textContent=m;t.classList.add("on");clearTimeout(toastT);toastT=setTimeout(()=>t.classList.remove("on"),1600)}

  /* ---------- Search ---------- */
  const q=$("q"),results=$("results");let hits=[],hi=0;
  function search(term){
    term=term.trim().toLowerCase();if(!term){results.classList.remove("open");hits=[];return}
    hits=[];
    for(const n of B.nodes){
      const fileHit=n.files.map(x=>x.path).concat(n.tests).find(p=>p.toLowerCase().includes(term));
      if((n.title+" "+n.sub+" "+n.summary).toLowerCase().includes(term))hits.push({n,why:(frameById.get(n.frame)||{}).title||""});
      else if(fileHit)hits.push({n,why:base(fileHit)});
    }
    hits=hits.slice(0,12);hi=0;
    results.innerHTML=hits.length?hits.map((h,i)=>`<button role="option" data-go="${esc(h.n.id)}" aria-selected="${i===hi}"><span style="color:${colorOf(h.n.frame)}">■</span>${esc(h.n.title)}<small>${esc(h.why)}</small></button>`).join(""):`<button disabled>No match</button>`;
    results.classList.add("open");
  }
  q.addEventListener("input",()=>search(q.value));
  q.addEventListener("keydown",e=>{
    if((e.key==="ArrowDown"||e.key==="ArrowUp")&&hits.length){e.preventDefault();hi=(hi+(e.key==="ArrowDown"?1:-1)+hits.length)%hits.length;[...results.children].forEach((b,i)=>b.setAttribute("aria-selected",i===hi))}
    else if(e.key==="Enter"&&hits[hi]){select(hits[hi].n.id);results.classList.remove("open");q.blur()}
    else if(e.key==="Escape"){q.value="";results.classList.remove("open");q.blur()}
  });
  results.addEventListener("pointerdown",e=>{const b=e.target.closest("[data-go]");if(b){e.preventDefault();select(b.dataset.go);results.classList.remove("open");q.blur()}});
  q.addEventListener("blur",()=>setTimeout(()=>results.classList.remove("open"),120));

  /* ---------- Toolbar, keyboard, theme ---------- */
  $("frames").innerHTML=B.frames.map(f=>`<button class="ab-chip" data-frame="${esc(f.id)}" style="--c:${tone(f.tone)}"><i></i>${esc(f.title)}</button>`).join("");
  $("frames").addEventListener("click",e=>{const b=e.target.closest("[data-frame]");if(!b)return;const r=frameRect(frameById.get(b.dataset.frame));fitRect(r.x,r.y,r.w,r.h,1.2)});
  const zc=()=>{const b=visibleBox();return [b.w/2,b.h/2]};
  $("zin").onclick=()=>zoomAt(view.k*1.25,...zc());
  $("zout").onclick=()=>zoomAt(view.k/1.25,...zc());
  $("fit").onclick=()=>fitAll();
  $("theme").onclick=()=>{const next=root.dataset.theme==="dark"?"light":"dark";root.dataset.theme=next;try{localStorage.setItem("ab-theme",next)}catch{}};
  addEventListener("keydown",e=>{
    if(e.target.closest&&e.target.closest("input,textarea,select"))return;
    if(e.metaKey||e.ctrlKey||e.altKey)return;
    const s=60;
    if(e.key==="/"){e.preventDefault();q.focus()}
    else if(e.key==="Escape")clearSelection();
    else if(e.key==="+"||e.key==="=")zoomAt(view.k*1.25,...zc());
    else if(e.key==="-"||e.key==="_")zoomAt(view.k/1.25,...zc());
    else if(e.key==="0")fitAll();
    else if(e.key.startsWith("Arrow")&&!(e.target.closest&&e.target.closest(".ab-panel"))){e.preventDefault();view.x+=e.key==="ArrowLeft"?s:e.key==="ArrowRight"?-s:0;view.y+=e.key==="ArrowUp"?s:e.key==="ArrowDown"?-s:0;apply()}
  });
  let rT;addEventListener("resize",()=>{clearTimeout(rT);rT=setTimeout(()=>{if(!selected&&!activeFlow)fitAll(false)},120)});

  /* ---------- Live overlay and status ---------- */
  function setOverlay(ov){
    lastOverlay=ov||{};
    const heat=ov.heat&&typeof ov.heat==="object"&&ov.heat.cards&&typeof ov.heat.cards==="object"?ov.heat:null;
    overlay={edited:ov.edited||{},inScope:ov.inScope||[],refreshing:ov.refreshing||[],unownedEdited:ov.unownedEdited||[],heat};
    const hb=$("heat");hb.hidden=!heat;hb.setAttribute("aria-pressed",String(heatOn));
    for(const [id,el] of cardEl){
      const ed=(overlay.edited[id]||[]).length,sc=overlay.inScope.includes(id),rf=overlay.refreshing.includes(id);
      const hc=heatOn&&heat&&heat.cards[id],hv=hc?Math.max(0,Math.min(1,+hc.heat||0)):0;
      el.classList.toggle("ov-edited",!!ed);el.classList.toggle("ov-scope",sc);el.classList.toggle("ov-refreshing",rf);
      el.classList.toggle("ov-heat",hv>0);el.style.setProperty("--heat",hv.toFixed(3));
      el.querySelector(".ab-badges").innerHTML=(hv>0?`<span class="ab-badge" style="--b:var(--hot);color:#fff">errors · ${+hc.issues||0}</span>`:"")+(ed?`<span class="ab-badge" style="--b:var(--edited)">edited · ${ed}</span>`:"")+(sc?`<span class="ab-badge" style="--b:var(--scope);color:#fff">in scope</span>`:"")+(rf?`<span class="ab-badge" style="--b:var(--refresh);color:#fff">refreshing</span>`:"");
    }
    const editedFrames=new Set(Object.keys(overlay.edited).map(id=>(nodeById.get(id)||{}).frame));
    root.querySelectorAll(".ab-chip").forEach(c=>c.classList.toggle("ov",editedFrames.has(c.dataset.frame)));
    if(selected)renderNode(nodeById.get(selected),neighbours(selected));else renderOverview();
  }
  $("heat").onclick=()=>{heatOn=!heatOn;try{localStorage.setItem("ab-heat",heatOn?"on":"off")}catch{}setOverlay(lastOverlay)};
  function setStatus(s){
    const el=$("status");el.className="ab-status "+(s.kind||"");el.querySelector("span").textContent=s.text||"";el.title=s.title||"";
  }

  buildBoard();renderOverview();fitAll(false);
  if(opts.status)setStatus(opts.status);
  if(!B.nodes.length)vp.insertAdjacentHTML("beforeend",`<div class="ab-empty"><div><b>No cards yet</b>The board for this project has not been mapped.</div></div>`);
  const start=decodeURIComponent(location.hash.slice(1));if(nodeById.has(start))select(start);
  return {setOverlay,setStatus,select,fitAll};
}

/* Fill defaults and drop anything that would break rendering; the harness validates strictly before writing. */
function normalise(d){
  const B={title:d.title||"Project",repo_url:d.repo_url||"",default_branch:d.default_branch||"main",overview:d.overview||{},footer:d.footer||"",
    edge_kinds:Object.assign({},DEFAULT_KINDS,d.edge_kinds||{}),frames:[],nodes:[],edges:[],flows:[]};
  B.frames=(d.frames||[]).map(f=>({id:String(f.id),title:f.title||f.id,tag:f.tag||"",tone:f.tone||"slate",col:+f.col||0,row:+f.row||0,cols:Math.max(1,+f.cols||1),rows:Math.max(1,+f.rows||1),external:!!f.external}));
  const fids=new Set(B.frames.map(f=>f.id));
  B.nodes=(d.nodes||[]).filter(n=>n&&n.id!=null).map(n=>({id:String(n.id),frame:fids.has(n.frame)?n.frame:(B.frames[0]||{}).id,col:+n.col||0,row:+n.row||0,icon:n.icon||"dot",title:n.title||n.id,sub:n.sub||"",summary:n.summary||"",responsibility:n.responsibility||"",external:!!n.external,
    files:(n.files||[]).map(x=>typeof x==="string"?{path:x,note:""}:{path:String(x.path),note:x.note||""}),tests:(n.tests||[]).map(String),owns:n.owns||[]}));
  const nids=new Set(B.nodes.map(n=>n.id));
  B.edges=(d.edges||[]).filter(e=>nids.has(e.from)&&nids.has(e.to)&&e.from!==e.to).map((e,i)=>({id:"e"+i,from:e.from,to:e.to,label:e.label||"",kind:B.edge_kinds[e.kind]?e.kind:"call"}));
  B.flows=(d.flows||[]).map(f=>({id:String(f.id),title:f.title||f.id,sub:f.sub||"",steps:f.steps||[],text:f.text||[]}));
  return B;
}

window.ArchitectureBoard={mount};
})();
