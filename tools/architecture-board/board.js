/* Architecture board renderer: fixed cards in frames, typed edges, pan/zoom, details panel.
   Data comes from board.json (see docs/architecture-board.md §3); nothing project-specific lives here.
   Usage: ArchitectureBoard.mount(rootElement, board, {status, agents, target}) -> {setOverlay, setStatus, setAgents, setJob, openJob}
   `agents` (dashboard only) turns on issue agents: {act(request) -> Promise, load(fingerprint), difitHref(fingerprint)}.
   `target` names the project for per-browser state (which issues were read).
   `session` (dashboard session page only) turns on session focus: view modes, neighbours, a session panel; feed it with api.setFocus(focusDocument).
   Without it the board renders exactly as a plain project board. */
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
  const SESS=!!opts.session;
  root.classList.add("ab");
  try{const t=localStorage.getItem("ab-theme");if(t)root.dataset.theme=t}catch{}
  root.innerHTML=`
  <div class="ab-bar">
    <h1>${esc(B.title)} <span>· architecture board</span></h1>
    <span class="ab-status" data-r="status"><i></i><span></span></span>
    ${SESS?`<div class="ab-sess" data-r="sess" role="toolbar" aria-label="Board view">
      <div class="ab-seg" role="group" aria-label="What to show">
        <button type="button" data-mode="focus" aria-pressed="true">This session</button>
        <button type="button" data-mode="highlight" aria-pressed="false">Highlight on full map</button>
        <button type="button" data-mode="full" aria-pressed="false">Whole system</button>
      </div>
      <label class="ab-nb"><input type="checkbox" data-r="nb"> Show neighbors</label>
      <span class="ab-lock" data-r="lock">Only you can open this task</span>
    </div>`:""}
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
    <p class="ab-banner" data-r="banner" role="alert" hidden></p>
  </div>
  <div class="ab-main">
    <div class="ab-vp" data-r="vp" tabindex="0" aria-label="Architecture board. Drag to pan, wheel or pinch to zoom.">
      <div class="ab-world" data-r="world"><svg class="ab-edges" data-r="edges" aria-hidden="true"><defs data-r="defs"></defs></svg></div>
      ${SESS?'<div class="ab-rest" data-r="rest" hidden></div>':""}
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
  /* Session focus: which cards, frames and edges are drawn (all of them unless the focus view hides some). */
  let curNodes=B.nodes,curFrames=B.frames,curEdges=B.edges,focus=null,vmode="focus",showNb=false,sessSig="";
  const nodeHome=new Map(B.nodes.map(n=>[n.id,{col:n.col,row:n.row}])),frameHome=new Map(B.frames.map(f=>[f.id,{col:f.col,row:f.row,cols:f.cols,rows:f.rows}]));
  let view={x:0,y:0,k:1},selected=null,activeFlow=null,bounds={x:0,y:0,w:1,h:1};
  let overlay={edited:{},inScope:[],refreshing:[],unownedEdited:[],heat:null},lastOverlay={};
  let heatOn=true;try{heatOn=localStorage.getItem("ab-heat")!=="off"}catch{}
  const AG=opts.agents||null,jobs=new Map(),details=new Map(),openJobs=new Set(),notes=new Map(),cmdTab=new Map(),ovBadges=new Map(),painted=new WeakMap();
  let confirming=null,overlaySig=null;
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
    for(const f of curFrames){
      const r=frameRect(f),el=document.createElement("div");
      el.className="ab-frame"+(f.external?" external":"");el.style.cssText=`left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;--c:${tone(f.tone)}`;
      el.innerHTML=`<header><h2>${esc(f.title)}</h2><span>${esc(f.tag)}</span></header>`;
      world.insertBefore(el,svg);
      minX=Math.min(minX,r.x);minY=Math.min(minY,r.y);maxX=Math.max(maxX,r.x+r.w);maxY=Math.max(maxY,r.y+r.h);
    }
    for(const n of curNodes){
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
    bounds=curNodes.length||curFrames.length?{x:minX,y:minY,w:maxX-minX,h:maxY-minY}:{x:0,y:0,w:1,h:1};
  }

  /* Each edge leaves and enters through the card side facing the other card; several edges on one side are spread out. */
  const opp={l:"r",r:"l",t:"b",b:"t"};
  function side(a,b){const dx=b.col-a.col,dy=b.row-a.row;if(Math.abs(dx)>=Math.abs(dy))return dx>0?"r":"l";return dy>0?"b":"t"}
  function routeEdges(){
    const ports=new Map();
    const add=(key,e,end,other)=>{if(!ports.has(key))ports.set(key,[]);ports.get(key).push({e,end,other})};
    for(const e of curEdges){
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
    for(const e of curEdges){
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
      if(curNodes.some(n=>hits(m,nodeRect(n))))continue;
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
  /* The board area left visible between the issue ticker (top) and the open issue sheet or phone drawer (bottom). */
  function visibleBox(){
    const r=vp.getBoundingClientRect();let top=0,bot=r.height;
    if(isNarrow())bot-=panel.classList.contains("open")?panel.getBoundingClientRect().height:46;
    // offsetTop ignores the sheet's slide-in transform, so a sheet that is still animating counts at its final place.
    const tkEl=$("ticker"),shEl=$("sheet");
    if(tkEl&&!tkEl.hidden)top=tkEl.offsetTop+tkEl.offsetHeight;
    if(shEl&&!shEl.hidden&&!shEl.classList.contains("min"))bot=Math.min(bot,shEl.offsetTop);
    return {y:top,w:r.width,h:Math.max(60,bot-top)};
  }
  function fitRect(x,y,w,h,maxK=1.15,animate=true,minK=MINK){
    const b=visibleBox(),pad=24;
    const k=Math.max(minK,Math.min(maxK,(b.w-pad*2)/w,(b.h-pad*2)/h));
    animateTo({k,x:(b.w-w*k)/2-x*k,y:b.y+(b.h-h*k)/2-y*k},animate);
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
  function frameNeighbourhood(ids,focus){
    const r=boxOf(ids),b=visibleBox(),pad=24,minK=isNarrow()?.4:.5;
    const inView=r.x*view.k+view.x>=0&&(r.x+r.w)*view.k+view.x<=b.w&&r.y*view.k+view.y>=b.y&&(r.y+r.h)*view.k+view.y<=b.y+b.h;
    if(inView&&view.k>=.5)return;
    const k=Math.max(minK,Math.min(1,(b.w-pad*2)/r.w,(b.h-pad*2)/r.h));
    // Too large to show whole at a readable size: centre on the component itself so it is never left off screen.
    if(focus&&(r.w*k>b.w-pad*2+1||r.h*k>b.h-pad*2+1)){const c=nodeRect(nodeById.get(focus));animateTo({k,x:b.w/2-(c.x+c.w/2)*k,y:b.y+b.h/2-(c.y+c.h/2)*k},true);return}
    fitRect(r.x,r.y,r.w,r.h,1,true,minK);
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
  const neighbours=id=>B.edges.filter(e=>!hiddenKinds.has(e.kind)&&edgeEl.has(e.id)&&(e.from===id||e.to===id));
  function highlight(nodeIds,edgeIds){
    world.classList.toggle("dim",!!nodeIds);
    for(const [id,el] of cardEl){el.classList.toggle("lit",!!nodeIds&&nodeIds.has(id));el.classList.toggle("sel",id===selected)}
    for(const [id,el] of edgeEl){const on=!!edgeIds&&edgeIds.has(id);el.classList.toggle("lit",on);labelEl.get(id).classList.toggle("lit",on)}
  }
  function select(id,keep){
    const n=nodeById.get(id);if(!n)return;
    if(SESS&&!cardEl.has(id))setMode("highlight");
    if(!keep)dropIssue();
    selected=id;activeFlow=null;
    const es=neighbours(id),ns=new Set([id]);es.forEach(e=>{ns.add(e.from);ns.add(e.to)});
    highlight(ns,new Set(es.map(e=>e.id)));
    renderNode(n,es);openPanel();
    history.replaceState(null,"","#"+encodeURIComponent(id));
    frameNeighbourhood(ns,id);
  }
  function selectEdge(eid){
    const e=B.edges.find(x=>x.id===eid);if(!e)return;
    dropIssue();selected=null;activeFlow=null;
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
  // A link to the source's logs around the issue, set only for projects marked with a logs skill.
  const logsUrl=i=>typeof i.logs_url==="string"&&/^https:\/\//.test(i.logs_url)?i.logs_url:null;
  function issueItem(i,card){
    const lv=LEVELS.includes(i.level)?i.level:"error",n=+i.count||0,u=+i.users||0;
    const url=typeof i.url==="string"&&/^https?:\/\//.test(i.url)?i.url:null;
    return `<li class="lv-${lv}"><span class="lv">${lv}</span><b>${esc(i.title)}</b>
      <small>${n} event${n===1?"":"s"}${u?` · ${u} user${u===1?"":"s"}`:""}${i.last_seen?` · last ${esc(ago(i.last_seen))}`:""}${i.share==="caller"?" · on the call path":""}${i.new?" · <em>new</em>":""}${i.source?` · ${esc(String(i.source).split(":")[0])}`:""}</small>
      ${url?`<a class="ab-ibtn" href="${esc(url)}" target="_blank" rel="noopener noreferrer">Open ↗</a>`:""}${logsUrl(i)?`<a class="ab-ibtn" href="${esc(logsUrl(i))}" target="_blank" rel="noopener noreferrer" title="Logs around this issue">Logs ↗</a>`:""}${jobSlot(i,card)}</li>`;
  }

  /* ---------- Issue agents (only with opts.agents) ---------- */
  const FPRINT=/^[0-9a-f]{64}$/;
  const isLive=j=>j.status==="queued"||j.status==="running";
  const QUIET_MS=10*60000,TICK_STALE_MS=2*60000;
  function chipOf(j){
    if(j.status==="running"&&j.stopping)return ["Stopping","run"];
    // Observation closes taken-over jobs too: how a job ended matters more than who held it.
    if(j.status==="ended")return ["Ended","end"];
    if(j.status==="diagnosed")return ["Diagnosis ready","ok"];
    if(j.owner==="you")return ["Yours","you"];
    return {queued:["Queued",""],running:["Analyzing","run"],stopped:["Stopped","warn"],failed:["Failed","hot"],detached:["Detached","hot"]}[j.status]||["Unknown",""];
  }
  /* Minutes an unfinished job's session has been quiet, once past ten. */
  function quietMinutes(j){
    const t=new Date(j.last_activity_at||NaN).getTime();
    if(!isLive(j)||!Number.isFinite(t))return 0;
    const q=Date.now()-t;return q>QUIET_MS?Math.floor(q/60000):0;
  }
  const jobOf=fp=>((details.get(fp)||{}).job)||jobs.get(fp)||null;
  // Analyze exists only for a problem with a root cause (a fingerprint); there is no Fix in this phase.
  function jobSlot(i,card){
    if(!AG||!FPRINT.test(i.fingerprint||""))return "";
    return `<div class="ab-jobslot" data-slot="${i.fingerprint}" data-issue="${esc(i.key||"")}" data-card="${esc(card)}">${slotHtml(i.fingerprint)}</div>`;
  }
  function slotHtml(fp){
    const j=jobOf(fp),note=notes.get(fp);
    const noteHtml=note?`<p class="ab-jnote" role="alert">${esc(note)}</p>`:"";
    if(!j)return `<div class="ab-jrow"><button type="button" class="ab-jbtn" data-agent="analyze" data-fp="${fp}">Analyze</button><small>A read-only agent investigates in its own worktree.</small></div>${noteHtml}`;
    const [label,cls]=chipOf(j),open=openJobs.has(fp);
    const toggle=open?"Hide job":j.status==="diagnosed"?"Read diagnosis":"Job details";
    return `<div class="ab-jrow"><span class="ab-jchip ${cls}"><i aria-hidden="true"></i>${esc(label)}</span><button type="button" class="ab-jbtn" data-agent="toggle" data-fp="${fp}" aria-expanded="${open}" aria-controls="ab-job-${fp}">${toggle}</button></div>${noteHtml}${open?jobPanel(fp,j,details.get(fp)||{}):""}`;
  }
  function kv(label,value,copyText,extra){
    const copy=copyText?`<button type="button" class="ab-jcopy" data-copy="${esc(copyText)}" aria-label="Copy ${esc(label.toLowerCase())}">Copy</button>`:"";
    return `<dt>${esc(label)}</dt><dd>${value}</dd><dd>${extra||copy}</dd>`;
  }
  function jobPanel(fp,j,d){
    const fp8=fp.slice(0,8),s=j.session_id||"",wt=j.worktree||{},tab=j.herdr_tab,mine=j.owner==="you",live=isLive(j),settling=mine&&j.status==="running"&&!!j.stopping,wait=+d.takeover_wait_seconds||30;
    const port=j.difit&&j.difit.port,on=!!(d.difit_running!=null?d.difit_running:j.difit_running);
    const difitCtl=on?`<span class="ab-jbtns"><a class="ab-jbtn" href="${esc(AG.difitHref(fp))}" target="_blank" rel="noopener noreferrer">Open</a><button type="button" class="ab-jbtn" data-agent="difit-stop" data-fp="${fp}">Stop</button></span>`
      :`<button type="button" class="ab-jbtn" data-agent="difit-start" data-fp="${fp}">Run difit</button>`;
    const rows=kv("Session",s?esc(s):`<span class="ab-jfaint">assigned when the agent starts</span>`,s)
      +kv("Worktree",esc(wt.path||""),wt.path)
      +kv("Branch",`${esc(wt.branch||"")} <span class="ab-jfaint">from ${esc(wt.base_branch||"")}</span>`,wt.branch)
      +kv("Diff viewer",on&&port?`running on 127.0.0.1:${+port}`:"not running","",difitCtl)
      +kv("Machine",esc(j.machine||""),"")
      +kv("Runs in",tab?`Herdr tab ${esc(tab.tab_id)}`:"background",tab?tab.tab_id:"");
    const events=(j.events||[]).slice(-5).map(e=>`<div><span class="tm">${esc(String(e.at||"").slice(11,19))}</span>${esc(e.summary)}</div>`).join("");
    const btn=(act,text,cls)=>`<button type="button" class="ab-jbtn ${cls||""}" data-agent="${act}" data-fp="${fp}">${text}</button>`;
    const closed=j.status==="ended"||j.status==="diagnosed";
    const buttons=settling?""
      :closed?btn("reanalyze","Analyze again")+(s&&!mine?btn("takeover","Take over","primary"):"")
      :mine?btn("handback","Hand back to runner")
      :live?btn("stop","Stop","danger")+(s?btn("takeover","Take over","primary"):"")
      :btn("reanalyze","Analyze again")+(s?btn("takeover","Take over","primary"):"");
    const footNote=settling?`Stopping: the agent finishes its current step (at most ${wait} s), then its session is yours to resume.`
      :j.status==="ended"?"Ended: the agent is gone or went quiet after its last reply without a diagnosis; the events say how it was seen."
      :mine&&!closed?"Yours: the runner only watches this job, and never stops it, until you hand it back.":"The agent stays read-only until you take over.";
    const quiet=quietMinutes(j);
    const confirm=confirming===fp?`<div class="ab-jconfirm"><p><b>Take over job ${fp8}?</b> ${j.launcher==="herdr"?"The runner stops supervising it; nothing is stopped and the agent keeps running in its Herdr tab.":`The background agent stops after its current step (at most ${wait} s), then its session is yours to resume.`} The runner won't touch this job again unless you hand it back.</p><span class="ab-jbtns">${btn("takeover-no","Cancel")}${btn("takeover-yes",j.launcher==="herdr"?"Take over":"Stop after this step and take over","primary")}</span></div>`:"";
    const last=[j.last_activity_at?`last activity ${esc(ago(j.last_activity_at))}`:"",j.observed_at?`checked ${esc(ago(j.observed_at))}`:""].filter(Boolean).join(" · ");
    return `<div class="ab-job" id="ab-job-${fp}">
      <div class="ab-jhead"><span><b>Analyze job ${fp8}</b> <span class="ab-jchip">${esc(j.agent==="codex"?"Codex":"Claude")}</span></span><small>${last}</small></div>
      ${quiet?`<p class="ab-jquiet" role="status">No activity for ${quiet} min: the session has not been written to since ${esc(ago(j.last_activity_at))}.</p>`:""}
      <dl class="ab-kv">${rows}</dl>
      ${j.error?`<p class="ab-jerr">${esc(j.error)}</p>`:""}
      ${events?`<div class="ab-feed" aria-label="Recent job events">${events}</div>`:""}
      ${mine&&!settling?resumeBox(fp,d.resume_commands):""}
      ${diagnosisBox(fp8,d.diagnosis)}
      <div class="ab-jfoot"><small>${footNote}</small><span class="ab-jbtns">${buttons}</span></div>${confirm}</div>`;
  }
  function resumeBox(fp,rc){
    if(!rc||typeof rc!=="object")return "";
    const {local,ssh,herdr}=rc;
    const tabs=[["local","On this machine",local],["ssh","From another machine",ssh],["herdr","In a Herdr tab",herdr&&herdr.command]];
    let cur=cmdTab.get(fp)||"local";if(!(tabs.find(t=>t[0]===cur)||[])[2])cur="local";
    const cmd=(tabs.find(t=>t[0]===cur)||[])[2]||"";
    const note=cur==="ssh"?"Run it on the machine you are viewing from.":cur==="herdr"?(rc.herdr&&rc.herdr.note)||"":"Run it in any terminal on this machine.";
    return `<div class="ab-resume"><div class="ab-tabs" role="tablist" aria-label="Where you are">${tabs.map(([k,l,c])=>`<button type="button" role="tab" aria-selected="${k===cur}" data-agent="tab" data-tab="${k}" data-fp="${fp}"${c?"":` disabled title="${k==="ssh"?"Set issue_agents.ssh in signals.json":"Not available"}"`}>${l}</button>`).join("")}</div>
      <pre class="ab-cmd">${esc(cmd)}</pre><div class="ab-jrow"><small>${esc(note)}</small><button type="button" class="ab-jbtn" data-copy="${esc(cmd)}">Copy command</button></div></div>`;
  }
  function diagnosisBox(fp8,dg){
    const sections=dg&&Array.isArray(dg.sections)?dg.sections:[];
    if(!sections.length)return "";
    const conf=sections.find(x=>x.heading==="Confidence");
    return `<div class="ab-diag"><div class="kicker">Diagnosis · job ${fp8}${conf?` · confidence ${esc(conf.text)}`:""}</div><dl>${sections.filter(x=>x.heading!=="Confidence").map(x=>`<dt>${esc(x.heading)}</dt><dd>${esc(x.text)}</dd>`).join("")}</dl></div>`;
  }
  function agentBadge(id){
    if(!AG)return "";
    let n=0;for(const j of jobs.values())if(j.card_id===id&&isLive(j))n++;
    return n?`<span class="ab-badge ab-badge--agent" title="${n} issue agent${n>1?"s":""} on this card">agent · ${n}</span>`:"";
  }
  function paintBadges(){for(const [id,el] of cardEl)el.querySelector(".ab-badges").innerHTML=(ovBadges.get(id)||"")+agentBadge(id)}
  /* Repaint only the job slots whose content changed, keeping keyboard focus on the same control. */
  function paintSlots(){
    for(const slot of root.querySelectorAll("[data-slot]")){
      const html=slotHtml(slot.dataset.slot);
      if(painted.get(slot)===html)continue;
      const f=slot.contains(document.activeElement)?document.activeElement:null;
      const key=f&&f.dataset?[f.dataset.agent||"",f.dataset.tab||"",f.dataset.copy||""]:null;
      slot.innerHTML=html;painted.set(slot,html);
      if(key){const again=[...slot.querySelectorAll("button,a")].find(b=>[b.dataset.agent||"",b.dataset.tab||"",b.dataset.copy||""].join("\0")===key.join("\0"));if(again)again.focus()}
    }
  }
  async function agentAct(b){
    const fp=b.dataset.fp,act=b.dataset.agent;if(!FPRINT.test(fp||""))return;
    if(act==="toggle"){if(openJobs.has(fp))openJobs.delete(fp);else{openJobs.add(fp);AG.load(fp)}paintSlots();return}
    if(act==="takeover"){confirming=fp;paintSlots();return}
    if(act==="takeover-no"){confirming=null;paintSlots();return}
    if(act==="tab"){cmdTab.set(fp,b.dataset.tab);paintSlots();return}
    const slot=b.closest("[data-slot]");
    const req={act:act==="takeover-yes"?"takeover":act,fingerprint:fp,issue:slot?slot.dataset.issue:"",card:slot?slot.dataset.card:""};
    b.disabled=true;b.setAttribute("aria-busy","true");notes.delete(fp);
    try{await AG.act(req);confirming=null;if(req.act==="analyze"||req.act==="reanalyze"||req.act==="takeover")openJobs.add(fp)}
    catch(err){notes.set(fp,(err&&err.message)||String(err))}
    paintSlots();b.disabled=false;b.removeAttribute("aria-busy");
  }
  /* `meta` is the rest of /agents.json: {generated_at, runner: {ticked_at}|null}. */
  function setAgents(list,meta){
    if(!AG||!Array.isArray(list))return;
    jobs.clear();for(const j of list)if(j&&FPRINT.test(j.fingerprint||""))jobs.set(j.fingerprint,j);
    paintBadges();paintSlots();
    if(meta)paintBanner(meta);
  }
  function paintBanner(meta){
    const el=$("banner"),now=new Date(meta.generated_at||NaN).getTime(),ticked=new Date((meta.runner||{}).ticked_at||NaN).getTime();
    const ref=Number.isFinite(now)?now:Date.now();
    let text="";
    if(Number.isFinite(ticked)){if(ref-ticked>TICK_STALE_MS)text=`The signals service has not checked issue agents for ${Math.floor((ref-ticked)/60000)} min, so job states may be out of date. Is harness-signals running?`}
    else if([...jobs.values()].some(isLive))text="The signals service has never checked issue agents on this machine, so unfinished jobs are not being observed. Is harness-signals running?";
    el.textContent=text;el.hidden=!text;
  }
  function setJob(fp,shown){
    if(!AG||!FPRINT.test(fp||"")||!shown||!shown.job)return;
    details.set(fp,Object.assign({},details.get(fp)||{},shown));jobs.set(fp,shown.job);
    paintBadges();paintSlots();
  }
  /* Open a job's card and its panel, e.g. from the Agents page link. */
  function openJob(fp){
    const j=jobOf(fp);if(!j||!nodeById.has(j.card_id))return false;
    openJobs.add(fp);if(selected!==j.card_id)select(j.card_id);else paintSlots();
    AG.load(fp);return true;
  }
  function renderNode(n,es){
    const f=frameById.get(n.frame)||{title:"",tag:""};
    panel.style.setProperty("--c",colorOf(n.frame));
    const out=es.filter(e=>e.from===n.id),inn=es.filter(e=>e.to===n.id);
    const edited=overlay.edited[n.id]||[],editedSet=new Set(edited);
    const hc=heatOn&&overlay.heat&&overlay.heat.cards[n.id];
    const problems=hc&&Array.isArray(hc.top)&&hc.top.length?`<h4>Problems</h4><ul class="ab-issues">${hc.top.map(i=>issueItem(i,n.id)).join("")}</ul>${hc.issues>hc.top.length?`<p class="ab-note">${hc.issues-hc.top.length} more issue${hc.issues-hc.top.length>1?"s":""} touch this component.</p>`:""}`:"";
    const live=[
      edited.length?`<div class="live"><b>Edited now:</b> ${edited.length} uncommitted file${edited.length>1?"s":""} in this component.<ul class="files" style="margin-top:6px">${edited.map(p=>fileItem(p,"",true)).join("")}</ul></div>`:"",
      overlay.inScope.includes(n.id)?`<div class="live scope"><b>In scope</b> for the active harness run.</div>`:"",
      overlay.refreshing.includes(n.id)?`<div class="live scope"><b>Refreshing:</b> the background mapper is re-describing this card.</div>`:""
    ].join("");
    panel.innerHTML=`<button class="ab-close" data-act="clear" aria-label="Close">×</button>
      <div class="kicker">${esc(f.title)}${f.tag?" · "+esc(f.tag):""}</div>
      <h3>${esc(n.title)}</h3><p class="sub">${esc(n.sub)}</p>
      ${whyHtml(n)}
      ${live}
      ${problems}
      <p>${esc(n.summary)}</p>
      ${n.responsibility?`<div class="resp">${esc(n.responsibility)}</div>`:""}
      ${es.length?`<h4>Connections</h4><div class="ab-conns">${out.map(e=>connBtn(nodeById.get(e.to),"to",e.label)).join("")}${inn.map(e=>connBtn(nodeById.get(e.from),"from",e.label)).join("")}</div>`:""}
      ${n.files.length?`<h4>Source files</h4><ul class="files">${n.files.map(x=>fileItem(x.path,x.note,editedSet.has(x.path))).join("")}</ul>`:""}
      ${n.tests.length?`<h4>Tests</h4><ul class="files">${n.tests.map(t=>fileItem(t,"",editedSet.has(t))).join("")}</ul>`:""}
      ${!n.files.length&&!n.tests.length?`<p class="ab-note">Outside this repository: nothing to open here.</p>`:""}`;
    issueSummary();
    for(const slot of panel.querySelectorAll("[data-slot]"))painted.set(slot,slotHtml(slot.dataset.slot));
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
    panel.innerHTML=`${sessionHtml()}<div class="kicker">How to read this board</div>
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
    issueSummary();
  }
  function runFlow(id){
    if(activeFlow===id){clearSelection();return}
    dropIssue();selected=null;activeFlow=id;
    const fl=B.flows.find(f=>f.id===id),ns=new Set(),es=new Set();
    for(const [a,b] of fl.steps){if(!cardEl.has(a)||!cardEl.has(b))continue;ns.add(a);ns.add(b);const e=B.edges.find(x=>(x.from===a&&x.to===b)||(x.from===b&&x.to===a));if(e)es.add(e.id)}
    highlight(ns,es);renderOverview();openPanel();
    if(ns.size){const r=boxOf(ns);fitRect(r.x,r.y,r.w,r.h,1.1)}
  }
  function clearSelection(keep){
    if(keep!==true)dropIssue();
    const had=selected||activeFlow;
    selected=null;activeFlow=null;highlight(null,null);renderOverview();
    if(location.hash)history.replaceState(null,"",location.pathname+location.search);
    if(had)panel.classList.remove("open");
  }
  function openPanel(){panel.classList.add("open")}
  panel.addEventListener("click",e=>{
    if(e.target.closest("[data-isheet]")){const i=byKey(tk.sel);if(i)openSheet(i);return}
    const ic=e.target.closest("[data-icard]");if(ic){tk.cardOpen=!tk.cardOpen;ic.textContent="Card details "+(tk.cardOpen?"▴":"▾");ic.setAttribute("aria-expanded",tk.cardOpen);panel.classList.toggle("ab-compact",!tk.cardOpen);return}
    const go=e.target.closest("[data-go]");if(go){select(go.dataset.go);return}
    if(e.target.closest("[data-act=clear]")){clearSelection();return}
    const fl=e.target.closest("[data-flow]");if(fl){runFlow(fl.dataset.flow);return}
    const ag=AG&&e.target.closest("[data-agent]");if(ag){agentAct(ag);return}
    const cp=e.target.closest("[data-copy]");if(cp){copy(cp.dataset.copy);return}
    if(isNarrow()&&!panel.classList.contains("open")&&!e.target.closest("a,input,label"))openPanel();
  });
  panel.addEventListener("change",e=>{
    const k=e.target.dataset.kind;if(!k)return;
    e.target.checked?hiddenKinds.delete(k):hiddenKinds.add(k);
    for(const ed of curEdges){const h=hiddenKinds.has(ed.kind);edgeEl.get(ed.id).classList.toggle("hidden",h);labelEl.get(ed.id).classList.toggle("hidden",h)}
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
  $("frames").addEventListener("click",e=>{const b=e.target.closest("[data-frame]");if(!b)return;const fr=frameById.get(b.dataset.frame);if(!curFrames.includes(fr))return;const r=frameRect(fr);fitRect(r.x,r.y,r.w,r.h,1.2)});
  const zc=()=>{const b=visibleBox();return [b.w/2,b.y+b.h/2]};
  $("zin").onclick=()=>zoomAt(view.k*1.25,...zc());
  $("zout").onclick=()=>zoomAt(view.k/1.25,...zc());
  $("fit").onclick=()=>fitAll();
  $("theme").onclick=()=>{const next=root.dataset.theme==="dark"?"light":"dark";root.dataset.theme=next;try{localStorage.setItem("ab-theme",next)}catch{}};
  addEventListener("keydown",e=>{
    if(e.target.closest&&e.target.closest("input,textarea,select"))return;
    if(e.metaKey||e.ctrlKey||e.altKey)return;
    // Inside the ticker or the issue sheet, arrows scroll them and + - 0 stay theirs: the board neither pans nor zooms.
    const own=e.target.closest&&e.target.closest(".ab-ticker,.ab-sheet");
    if(own&&(e.key.startsWith("Arrow")||["+","=","-","_","0"].includes(e.key)))return;
    const s=60;
    if(e.key==="/"){e.preventDefault();q.focus()}
    else if(e.key==="Escape")clearSelection();
    else if(e.key==="+"||e.key==="=")zoomAt(view.k*1.25,...zc());
    else if(e.key==="-"||e.key==="_")zoomAt(view.k/1.25,...zc());
    else if(e.key==="0")fitAll();
    else if((e.key==="n"||e.key==="p")&&!ticker.hidden&&!tk.min)tkStep(e.key==="n"?1:-1);
    else if(e.key.startsWith("Arrow")&&!(e.target.closest&&e.target.closest(".ab-panel"))){e.preventDefault();view.x+=e.key==="ArrowLeft"?s:e.key==="ArrowRight"?-s:0;view.y+=e.key==="ArrowUp"?s:e.key==="ArrowDown"?-s:0;apply()}
  });
  let rT;addEventListener("resize",()=>{clearTimeout(rT);rT=setTimeout(()=>{if(!selected&&!activeFlow)fitAll(false)},120)});

  /* ---------- Issue ticker and issue sheet (heat.issues, dashboard and export alike) ---------- */
  /* Newest issues run along the top of the diagram. Arrivals wait behind "↑ N new" so the list never moves under the reader;
     picking one lights its card and opens a sheet over the lower diagram. Read state and the folded ticker live in this browser only. */
  const TK_MIN="ab-ticker-min",READ_TTL=14*864e5,READ_KEY="ab-read:"+(opts.target||B.title);
  const tk={list:[],pend:[],f:"all",cur:0,ck:null,min:false,scroll:false,fresh:new Set(),html:"",sel:null,cardOpen:false,sheet:null,sheetMin:false,tab:"overview",sheetSig:""};
  try{tk.min=localStorage.getItem(TK_MIN)==="1"}catch{}
  // Read marks: key -> {s: last_seen when read, t: when read}. Older plain strings count as read now.
  let readMap={};try{const r=JSON.parse(localStorage.getItem(READ_KEY)||"{}");if(r&&typeof r==="object")for(const [k,v] of Object.entries(r))readMap[k]=v&&typeof v==="object"?v:{s:v,t:Date.now()}}catch{}
  const tms=t=>{const v=new Date(t||NaN).getTime();return Number.isFinite(v)?v:0};
  const isSlow=i=>i.level==="warning"&&/exceeded [\d,.]+ ?ms threshold/i.test(i.title||"");
  const isNew=i=>i.first_seen?Date.now()-tms(i.first_seen)<864e5:!!i.new;
  const isRead=i=>Object.prototype.hasOwnProperty.call(readMap,i.key)&&tms((readMap[i.key]||{}).s)>=tms(i.last_seen);
  const lvTag=i=>{const sl=isSlow(i),w=!sl&&(i.level==="warning"||i.level==="info");return `<span class="ab-lvl ${sl?"slow":w?"warn":"err"}">${sl?"slow":w?"warn":"error"}</span>`};
  const newTag=i=>isNew(i)?'<span class="ab-new">new</span>':"";
  const inum=i=>"#"+String(i.key||"").split("#").pop();
  const cardOf=i=>typeof i.card==="string"&&nodeById.has(i.card)?i.card:null;
  const cardName=i=>cardOf(i)?nodeById.get(i.card).title:"No card owns it";
  const byKey=k=>k==null?null:tk.list.find(i=>i.key===k)||tk.pend.find(i=>i.key===k)||null;
  const srcName=i=>{const x=String(i.source||"").split(":")[0];return x?x[0].toUpperCase()+x.slice(1):""};
  const day=t=>{const d=new Date(t||NaN);return Number.isFinite(d.getTime())?d.toLocaleString(undefined,{month:"short",day:"numeric",hour:"2-digit",minute:"2-digit"}):"unknown"};
  const plural=(n,w)=>`${n} ${w}${n===1?"":"s"}`;
  const byNewest=a=>a.sort((x,y)=>tms(y.last_seen)-tms(x.last_seen));
  vp.insertAdjacentHTML("beforeend",'<div class="ab-ticker" data-r="ticker" hidden></div><section class="ab-sheet" data-r="sheet" aria-label="Issue details" hidden></section>');
  const ticker=$("ticker"),sheet=$("sheet");
  // The ticker and sheet are controls, not board: presses and wheels on them never pan, zoom or select.
  for(const el of [ticker,sheet])for(const ev of ["pointerdown","mousedown","wheel","dblclick"])el.addEventListener(ev,e=>e.stopPropagation());
  const filtered=()=>tk.list.filter(i=>tk.f==="all"||(tk.f==="err"&&!isSlow(i))||(tk.f==="slow"&&isSlow(i))||(tk.f==="new"&&isNew(i)));
  function setIssues(list){
    list=(Array.isArray(list)?list:[]).filter(i=>i&&typeof i==="object"&&typeof i.key==="string");
    const now=new Map(list.map(i=>[i.key,i])),shown=new Map(tk.list.map(i=>[i.key,i]));
    // Shown issues update in place; a new key or a later last_seen waits in pend until the reader merges it.
    tk.list=tk.list.filter(i=>now.has(i.key)).map(i=>{const n=now.get(i.key);return tms(n.last_seen)>tms(i.last_seen)?i:n});
    // Nothing is shown (first poll, or every shown issue went away): nothing can move under the reader, so take the list as is.
    if(!tk.list.length){tk.list=byNewest(list.slice());tk.pend=[];return}
    tk.pend=byNewest(list.filter(n=>{const o=shown.get(n.key);return !o||tms(n.last_seen)>tms(o.last_seen)}));
  }
  function flush(){
    const keys=new Set(tk.pend.map(i=>i.key));
    tk.list=tk.pend.concat(tk.list.filter(i=>!keys.has(i.key)));
    tk.fresh=keys;tk.pend=[];tk.ck=null;tk.scroll=true;
  }
  function markRead(i){
    const t=Date.now();readMap[i.key]={s:i.last_seen||"",t};
    // Prune by age, not by absence: one failed source poll must not make its issues unread again.
    for(const [k,v] of Object.entries(readMap))if(!(t-(+v.t||0)<READ_TTL))delete readMap[k];
    try{localStorage.setItem(READ_KEY,JSON.stringify(readMap))}catch{}
  }
  function ping(id){
    for(const el of world.querySelectorAll(".ab-card.ab-ping"))el.classList.remove("ab-ping");
    if(id&&cardEl.has(id))cardEl.get(id).classList.add("ab-ping");
  }
  function dropIssue(){tk.sel=null;tk.cardOpen=false;ping(null);panel.classList.remove("ab-compact")}
  function renderTicker(){
    const on=heatOn&&!!overlay.heat&&tk.list.length+tk.pend.length>0;
    ticker.hidden=!on;
    if(!on){tk.html="";ticker.innerHTML="";return}
    // The current item is tracked by key: a poll that drops earlier issues must not move it onto another one.
    const L=filtered();tk.cur=tk.ck==null?0:L.findIndex(i=>i.key===tk.ck);
    const unread=tk.list.filter(i=>!isRead(i)).length,np=tk.pend.length;
    ticker.classList.toggle("min",tk.min);
    const html=tk.min?`<button type="button" class="ab-pill${np?" alert":""}" data-tk="restore" title="Show the issue ticker"><span class="ab-live">Live</span><b>${plural(tk.list.length,"issue")}</b>${unread?`<span class="ab-count">${unread} unread</span>`:""}${np?`<span class="ab-count">+${np} new</span>`:""}<span aria-hidden="true">▾</span></button>`
      :`<div class="ab-tnav"><div class="ab-trow"><span class="ab-live">Live · newest issues</span><button type="button" class="ab-icon" data-tk="min" title="Minimise the ticker" aria-label="Minimise the ticker">–</button></div>
      <div class="ab-trow"><button type="button" class="ab-sbtn" data-step="-1" title="Newer (p)" aria-label="Newer issue">◂</button><button type="button" class="ab-sbtn" data-step="1" title="Older (n)" aria-label="Older issue">▸</button><span class="ab-m" data-r="tkpos">${L.length&&tk.cur>=0?tk.cur+1:"–"} / ${L.length}</span></div>
      <div class="ab-trow"><select class="ab-sbtn" data-tk="filter" aria-label="Show issues">${[["all","All"],["err","Errors"],["slow","Slow"],["new","New today"]].map(([v,l])=>`<option value="${v}"${tk.f===v?" selected":""}>${l}</option>`).join("")}</select>${np?`<button type="button" class="ab-sbtn primary" data-tk="flush">↑ ${np} new</button>`:""}</div></div>
      <div class="ab-track">${L.map((i,k)=>`<div class="ab-tk${k===tk.cur?" cur":""}${tk.fresh.has(i.key)?" fresh":""}${isRead(i)?" read":""}" data-k="${k}" data-key="${esc(i.key)}" role="button" tabindex="0"><div class="ab-tkh"><span class="ab-dot" title="Unread"></span>${lvTag(i)}${newTag(i)}<span class="ab-m">${esc(ago(i.last_seen))}</span></div><div class="ab-t">${esc(i.title)}</div><div class="ab-m ab-one">${esc(cardName(i))}</div></div>`).join("")}</div>`;
    if(html===tk.html)return;
    const tr=ticker.querySelector(".ab-track"),sx=tr?tr.scrollLeft:0,f=ticker.contains(document.activeElement)?document.activeElement:null;
    const fkey=f&&f.dataset?["tk","step","k"].map(a=>f.dataset[a]||"").join("\0"):null;
    ticker.innerHTML=html;tk.html=html;tk.fresh=new Set();
    const t2=ticker.querySelector(".ab-track");
    if(t2){t2.scrollLeft=sx;const c=t2.querySelector(".ab-tk.cur");if(c&&tk.scroll){const a=c.offsetLeft-t2.offsetLeft;if(a<t2.scrollLeft||a+c.offsetWidth>t2.scrollLeft+t2.clientWidth)t2.scrollLeft=Math.max(0,a-8)}}
    tk.scroll=false;
    if(fkey){const again=[...ticker.querySelectorAll("button,select,[data-k]")].find(b=>["tk","step","k"].map(a=>b.dataset[a]||"").join("\0")===fkey);if(again)again.focus()}
  }
  function tkGo(k,deeper=true){
    const L=filtered();if(!L.length)return;
    tk.cur=(k+L.length)%L.length;tk.scroll=true;
    // Open the sheet first so the card is framed in the space the sheet leaves.
    const i=L[tk.cur];tk.ck=i.key;if(deeper)openSheet(i);pickIssue(i);
    renderTicker();
  }
  // With no current item (it left the list), "next" starts at the first and "previous" at the last.
  const tkStep=d=>tkGo(tk.cur<0?(d>0?0:-1):tk.cur+d);
  /* Light the issue's card with the board's own selection; the panel shows only a summary until "Card details". */
  function pickIssue(i){
    markRead(i);tk.sel=i.key;tk.cardOpen=false;
    const c=cardOf(i);
    if(c){select(c,true);ping(c)}else{ping(null);clearSelection(true);openPanel()}
    if(isNarrow())panel.classList.remove("open");
  }
  function issueSummary(){
    const i=byKey(tk.sel),c=i&&cardOf(i);
    if(!i||(c&&selected!==c)||(!c&&(selected||activeFlow))){panel.classList.remove("ab-compact");return}
    const x=panel.querySelector(":scope>.ab-close");
    (x||panel).insertAdjacentHTML(x?"afterend":"afterbegin",`<div class="ab-isum"><div class="kicker">Selected issue ${esc(inum(i))}</div><div class="ab-t ab-clamp">${esc(i.title)}</div>
      <div class="ab-irow">${lvTag(i)}${newTag(i)}<span class="ab-m">${esc(ago(i.last_seen))} · ${+i.count||0} ev · ${plural(+i.users||0,"user")}</span></div>
      <div class="ab-irow"><button type="button" class="ab-sbtn primary" data-isheet>Open details ▴</button>${c?`<button type="button" class="ab-sbtn" data-icard aria-expanded="${tk.cardOpen}">Card details ${tk.cardOpen?"▴":"▾"}</button>`:""}</div></div>`);
    panel.classList.toggle("ab-compact",!tk.cardOpen);
  }
  function openSheet(i){tk.sheet=i.key;tk.sheetMin=false;tk.tab="overview";renderSheet(true)}
  function framesHtml(i,list){
    if(!(i.frames||[]).length)return `<p class="ab-m">${isSlow(i)?`No stack trace: this is a slow ${/job/i.test(i.title)?"job":"request"} threshold alert, not an exception, so there is no stack to show.`:"No stack frames were reported for this issue."}</p>`;
    return `<ol class="ab-stack">${list.map(f=>`<li class="${f.in_app?"":"vendor"}"><code>${esc(f.path)}${+f.line?":"+(+f.line):""}</code>${f.in_app?"":' <span class="ab-m">vendor</span>'}</li>`).join("")}</ol>`;
  }
  function cardHtml(i){
    const c=cardOf(i);if(!c)return `<p class="ab-m">No card on this board owns the code in this issue.</p>`;
    const n=nodeById.get(c);
    return `<div class="ab-cardref"><b>${esc(n.title)}</b><div class="ab-m">${esc(n.sub)}</div><p>${esc(n.summary)}</p>
      <div class="ab-irow"><button type="button" class="ab-sbtn" data-sh="showcard">Show on board</button><span class="ab-m">${plural(n.files.length,"file")} · ${plural(n.owns.length,"owned path")}</span></div></div>`;
  }
  const canAnalyze=i=>!!AG&&FPRINT.test(i.fingerprint||"");
  function factsHtml(i){
    const row=(k,v)=>`<dt>${k}</dt><dd>${v}</dd>`;
    return `<dl class="ab-ikv">${row("Level",lvTag(i)+" "+newTag(i))+row("Events",+i.count||0)+row("Users",+i.users||0)+row("First seen",esc(day(i.first_seen)))
      +row("Last seen",`${esc(day(i.last_seen))} (${esc(ago(i.last_seen))})`)+row("Card",esc(cardName(i)))+row("Source",`${esc(srcName(i))} ${esc(inum(i))}`.trim())}</dl>`;
  }
  function actionsHtml(i){
    const url=typeof i.url==="string"&&/^https?:\/\//.test(i.url)?i.url:null;
    return `<div class="ab-irow">${canAnalyze(i)?`<button type="button" class="ab-sbtn primary" data-sh="tab" data-stab="agent">Analyze</button>`:""}${url?`<a class="ab-sbtn" href="${esc(url)}" target="_blank" rel="noopener noreferrer">${srcName(i)?`Open in ${esc(srcName(i))}`:"Open the issue"} ↗</a>`:""}${logsUrl(i)?`<a class="ab-sbtn" href="${esc(logsUrl(i))}" target="_blank" rel="noopener noreferrer" title="Logs around this issue">Logs ↗</a>`:""}</div>`;
  }
  function agentHtml(i){
    if(!AG)return `<p class="ab-m">Issue agents run from the harness dashboard, not from an exported board.</p>`;
    if(!canAnalyze(i))return `<p class="ab-m">Analyze needs a root cause: an in-app stack frame, with a line, in code a card owns. This issue has none, so no agent can start for it.</p>`;
    return `<p class="ab-m">Analyze starts a read-only agent in its own worktree; its diagnosis appears here, on the card, and on the Agents page.</p>${jobSlot(i,i.card)}`;
  }
  function renderSheet(force){
    const i=heatOn&&byKey(tk.sheet);
    if(!i){tk.sheet=null;sheet.hidden=true;sheet.innerHTML="";tk.sheetSig="";return}
    const sig=JSON.stringify([i,tk.sheetMin,tk.tab]);if(!force&&sig===tk.sheetSig)return;
    const keepTab=tk.sheetSig&&JSON.parse(tk.sheetSig)[2]===tk.tab,old=sheet.querySelector(".ab-sbody"),sy=keepTab&&old?old.scrollTop:0;
    tk.sheetSig=sig;sheet.hidden=false;sheet.classList.toggle("min",tk.sheetMin);
    const close=`<button type="button" class="ab-icon" data-sh="close" title="Close" aria-label="Close the issue">×</button>`;
    if(tk.sheetMin){sheet.innerHTML=`<button type="button" class="ab-sheettab" data-sh="restore" title="Show the issue">${lvTag(i)}<span class="ab-t ab-one">${esc(i.title)}</span><span aria-hidden="true">▴</span></button>${close}`;return}
    const body=tk.tab==="stack"?framesHtml(i,i.frames||[]):tk.tab==="card"?cardHtml(i):tk.tab==="agent"?agentHtml(i)
      :`<div class="ab-cols"><div>${factsHtml(i)}${actionsHtml(i)}</div><div><h4 class="ab-h">Top of the stack</h4>${framesHtml(i,(i.frames||[]).slice(0,4))}</div></div>`;
    sheet.innerHTML=`<header><div class="ab-shead"><div class="kicker">Issue ${esc(inum(i))} · ${esc(cardName(i))}</div><div class="ab-t ab-stitle">${esc(i.title)}</div></div>
      <button type="button" class="ab-icon" data-sh="min" title="Minimise" aria-label="Minimise the issue">–</button>${close}</header>
      <nav class="ab-stabs" role="tablist" aria-label="Issue details">${[["overview","Overview"],["stack","Stack"],["card","Card"],["agent","Agent"]].map(([k,l])=>`<button type="button" role="tab" data-sh="tab" data-stab="${k}" aria-selected="${tk.tab===k}">${l}</button>`).join("")}</nav>
      <div class="ab-sbody">${body}</div>`;
    sheet.querySelector(".ab-sbody").scrollTop=sy;
    for(const slot of sheet.querySelectorAll("[data-slot]"))painted.set(slot,slotHtml(slot.dataset.slot));
  }
  ticker.addEventListener("click",e=>{
    const t=e.target,a=(t.closest("[data-tk]")||{dataset:{}}).dataset.tk;
    const save=()=>{try{localStorage.setItem(TK_MIN,tk.min?"1":"0")}catch{}};
    if(a==="min"){tk.min=true;save();renderTicker();return}
    if(a==="restore"){tk.min=false;save();if(tk.pend.length)flush();renderTicker();return}
    if(a==="flush"){flush();tkGo(0,false);return}
    const st=t.closest("[data-step]");if(st){tkStep(+st.dataset.step);return}
    const c=t.closest("[data-k]");if(c)tkGo(+c.dataset.k);
  });
  ticker.addEventListener("keydown",e=>{const c=e.target.closest&&e.target.closest("[data-k]");if(c&&(e.key==="Enter"||e.key===" ")){e.preventDefault();tkGo(+c.dataset.k)}});
  ticker.addEventListener("change",e=>{if(e.target.dataset.tk==="filter"){tk.f=e.target.value;tk.ck=null;renderTicker()}});
  sheet.addEventListener("click",e=>{
    const ag=AG&&e.target.closest("[data-agent]");if(ag){agentAct(ag);return}
    const cp=e.target.closest("[data-copy]");if(cp){copy(cp.dataset.copy);return}
    const b=e.target.closest("[data-sh]");if(!b)return;
    const a=b.dataset.sh;
    if(a==="min")tk.sheetMin=true;
    else if(a==="restore")tk.sheetMin=false;
    else if(a==="close")tk.sheet=null;
    else if(a==="tab")tk.tab=b.dataset.stab;
    else if(a==="showcard"){tk.sheetMin=true;renderSheet(true);const i=byKey(tk.sheet);if(i)pickIssue(i);return}
    renderSheet(true);
  });

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
      ovBadges.set(id,(hv>0?`<span class="ab-badge" style="--b:var(--hot);color:#fff">errors · ${+hc.issues||0}</span>`:"")+(ed?`<span class="ab-badge" style="--b:var(--edited)">edited · ${ed}</span>`:"")+(sc?`<span class="ab-badge" style="--b:var(--scope);color:#fff">in scope</span>`:"")+(rf?`<span class="ab-badge" style="--b:var(--refresh);color:#fff">refreshing</span>`:""));
    }
    paintBadges();
    setIssues(heat&&heat.issues);
    // The picked issue left the list, or Heat is off: drop the selection, ping and summary, and redraw the panel as normal.
    if(tk.sel!=null&&!(heatOn&&heat&&byKey(tk.sel))){dropIssue();overlaySig=null}
    renderTicker();renderSheet();
    const editedFrames=new Set(Object.keys(overlay.edited).map(id=>(nodeById.get(id)||{}).frame));
    root.querySelectorAll(".ab-chip").forEach(c=>c.classList.toggle("ov",editedFrames.has(c.dataset.frame)));
    // An unchanged overlay leaves the open panel (and any job panel in it) alone.
    const sig=JSON.stringify([ov,heatOn]);if(sig===overlaySig)return;overlaySig=sig;
    if(selected)renderNode(nodeById.get(selected),neighbours(selected));else renderOverview();
  }
  $("heat").onclick=()=>{heatOn=!heatOn;try{localStorage.setItem("ab-heat",heatOn?"on":"off")}catch{}setOverlay(lastOverlay)};
  function setStatus(s){
    const el=$("status");el.className="ab-status "+(s.kind||"");el.querySelector("span").textContent=s.text||"";el.title=s.title||"";
  }

  /* ---------- Session focus: which cards a session earns, and why ---------- */
  const sigOf=id=>(focus&&focus.signals&&focus.signals[id])||null;
  const list=a=>Array.isArray(a)?a:[];
  const isCore=sg=>!!sg&&(list(sg.edited).length>0||list(sg.scope).length>0);
  const focusIds=()=>focus&&focus.signals?Object.keys(focus.signals).filter(id=>nodeById.has(id)):[];
  const coreIds=()=>new Set(focusIds().filter(id=>isCore(sigOf(id))));
  /* Neighbours: one connection away from a changed or planned card, and not themselves in focus. */
  function neighbourIds(){
    const core=coreIds(),out=new Set();
    for(const e of B.edges){
      if(core.has(e.from)&&!sigOf(e.to))out.add(e.to);
      if(core.has(e.to)&&!sigOf(e.from))out.add(e.from);
    }
    return out;
  }
  const levelOf=id=>sigOf(id)?"focus":showNb&&neighbourIds().has(id)?"ctx":"rest";
  function applyLayout(){
    for(const n of B.nodes){const h=nodeHome.get(n.id);n.col=h.col;n.row=h.row}
    for(const f of B.frames)Object.assign(f,frameHome.get(f.id));
    curNodes=B.nodes;curFrames=B.frames;curEdges=B.edges;
    if(!focus||vmode!=="focus")return;
    const ids=new Set(focusIds());if(showNb)for(const id of neighbourIds())ids.add(id);
    curNodes=B.nodes.filter(n=>ids.has(n.id));
    const num=(a,b)=>a-b,cols=[...new Set(curNodes.map(n=>nodeHome.get(n.id).col))].sort(num),rows=[...new Set(curNodes.map(n=>nodeHome.get(n.id).row))].sort(num);
    for(const n of curNodes){const h=nodeHome.get(n.id);n.col=cols.indexOf(h.col);n.row=rows.indexOf(h.row)}
    curFrames=B.frames.filter(f=>curNodes.some(n=>n.frame===f.id));
    for(const f of curFrames){
      const mine=curNodes.filter(n=>n.frame===f.id),c0=Math.min(...mine.map(n=>n.col)),r0=Math.min(...mine.map(n=>n.row));
      f.col=c0;f.row=r0;f.cols=Math.max(...mine.map(n=>n.col))-c0+1;f.rows=Math.max(...mine.map(n=>n.row))-r0+1;
    }
    curEdges=B.edges.filter(e=>ids.has(e.from)&&ids.has(e.to));
  }
  function clearBoard(){
    for(const el of [...world.children])if(el!==svg)el.remove();
    for(const p of [...svg.querySelectorAll("path")])p.remove();
    cardEl.clear();edgeEl.clear();labelEl.clear();placed.length=0;
  }
  const kindOfSignal=sg=>list(sg.edited).length?"edited":list(sg.scope).length?"scope":"touched";
  function decorate(){
    for(const [id,el] of cardEl){
      const sg=sigOf(id),lv=vmode==="full"?"plain":levelOf(id);
      el.classList.remove("lv-focus","lv-ctx","lv-rest","scope-only","touched-only");
      if(vmode==="full"){ovBadges.delete(id);continue}
      el.classList.add("lv-"+lv);
      let pills="";
      if(sg){
        const k=kindOfSignal(sg);
        if(k==="scope")el.classList.add("scope-only");else if(k==="touched")el.classList.add("touched-only");
        if(list(sg.edited).length)pills+=`<span class="ab-badge" style="--b:var(--edited)">${list(sg.edited).length} changed</span>`;
        if(list(sg.scope).length&&!list(sg.edited).length)pills+=`<span class="ab-badge" style="--b:var(--scope);color:#fff">planned</span>`;
        if(list(sg.touched).length)pills+=`<span class="ab-badge" style="--b:var(--touched);color:#fff">read</span>`;
      }else if(lv==="ctx")pills=`<span class="ab-badge" style="--b:var(--faint);color:#fff">neighbor</span>`;
      ovBadges.set(id,pills);
    }
    paintBadges();
    for(const e of curEdges){
      const p=edgeEl.get(e.id),l=labelEl.get(e.id),a=!!sigOf(e.from),b=!!sigOf(e.to);
      const hot=vmode!=="full"&&a&&b,faded=vmode!=="full"&&!a&&!b;
      p.classList.toggle("hot",hot);p.classList.toggle("faded",faded);l.classList.toggle("faded",faded);
    }
    root.querySelectorAll(".ab-chip").forEach(c=>{c.hidden=!curFrames.includes(frameById.get(c.dataset.frame))});
    root.querySelector(".ab-empty")&&root.querySelector(".ab-empty").remove();
    if(!curNodes.length)vp.insertAdjacentHTML("beforeend",`<div class="ab-empty"><div><b>${focus?"Nothing on the map yet":"Loading the session"}</b>${focus?"This session has not changed, planned or read any file that a card owns.":""}</div></div>`);
    const rest=$("rest"),hidden=B.nodes.length-curNodes.length;
    rest.hidden=!(vmode==="focus"&&hidden>0);
    rest.innerHTML=hidden>0?`${hidden} other part${hidden===1?"":"s"} of the system ${hidden===1?"is":"are"} folded away. <button type="button" data-act="unfold">Show them dimmed</button>`:"";
    root.querySelectorAll("[data-mode]").forEach(b=>b.setAttribute("aria-pressed",String(b.dataset.mode===vmode)));
    $("nb").disabled=vmode==="full";
  }
  function reselect(){
    if(selected&&cardEl.has(selected)){
      const es=neighbours(selected),ns=new Set([selected]);es.forEach(e=>{ns.add(e.from);ns.add(e.to)});
      highlight(ns,new Set(es.map(e=>e.id)));renderNode(nodeById.get(selected),es);
    }else{selected=null;activeFlow=null;highlight(null,null);renderOverview()}
  }
  function rebuild(refit){
    applyLayout();clearBoard();buildBoard();decorate();reselect();
    if(refit)fitAll(false);
  }
  function setMode(m){
    if(!["focus","highlight","full"].includes(m)||m===vmode)return;
    vmode=m;rebuild(true);
  }
  function sessionHtml(){
    if(!SESS||!focus)return "";
    const se=focus.session||{},ow=se.owner||{},phases=["plan","build","review"];
    const at=phases.indexOf(se.phase),ph=phases.map((p,i)=>`<span class="${p===se.phase?"on":i<at||se.phase==="done"?"done":""}">${p}</span>`).join("");
    const kv=[["Owner",esc(ow.login||ow.user||"you")],["Branch",se.branch?`<code>${esc(se.branch)}</code>${se.base?" → <code>"+esc(se.base)+"</code>":""}`:""],["Worktree",se.root?`<code>${esc(se.root)}</code>`:""],
      ["Phase",se.phase?`<span class="ab-phase">${ph}</span>`:""],["Agent",se.live?'<span class="ab-live">live</span>':se.last_active?"last active "+esc(ago(se.last_active)):""]].filter(r=>r[1]);
    return `<div class="kicker">Task in a worktree</div><h3>${esc(se.title||se.branch||"Session")}</h3>
      <dl class="ab-kv">${kv.map(([k,v])=>`<dt>${k}</dt><dd>${v}</dd>`).join("")}</dl>
      ${focus.summary?`<h4>In plain words</h4><p class="ab-summary">${esc(focus.summary)}</p>`:""}
      <h4>Legend</h4>
      <div class="ab-key"><div><i style="background:var(--edited)"></i>Changed in this worktree</div><div><i style="background:var(--scope)"></i>Planned in the task, not changed yet</div><div><i style="background:var(--touched)"></i>Only read by the session</div><div><i class="ctx"></i>Neighbor, shown for context</div></div>
      ${unmappedHtml()}`;
  }
  function unmappedHtml(){
    const u=list(focus&&focus.unmapped);
    return u.length?`<h4>Not on the map</h4><p class="ab-note">${u.length===1?"This file belongs":"These files belong"} to no card.</p><ul class="files">${u.map(p=>fileItem(p,"",false)).join("")}</ul>`:"";
  }
  function whyHtml(n){
    if(!SESS||!focus)return "";
    const sg=sigOf(n.id),group=(label,cls,files)=>list(files).length?`<div class="ab-why"><span class="ab-badge" style="--b:var(--${cls});${cls==="edited"?"":"color:#fff"}">${label}</span><ul class="files">${list(files).map(p=>fileItem(p,"",cls==="edited")).join("")}</ul></div>`:"";
    let body;
    if(sg)body=group("changed","edited",sg.edited)+group("planned in task","scope",sg.scope)+group("read only","touched",sg.touched);
    else{
      const via=[...new Set(B.edges.filter(e=>(e.from===n.id&&sigOf(e.to))||(e.to===n.id&&sigOf(e.from))).map(e=>nodeById.get(sigOf(e.to)&&e.from===n.id?e.to:e.from).title))];
      body=via.length?`<p>Not touched. Shown because it connects to ${via.map(esc).join(", ")}.</p>`:`<p>Not touched by this session.</p>`;
    }
    return `<div class="ab-whybox"><h4>Why this card</h4>${body}</div>${unmappedHtml()}`;
  }
  function setFocus(doc){
    const sig=JSON.stringify(doc||null);if(sig===sessSig)return;
    const before=new Set(curNodes.map(n=>n.id));
    sessSig=sig;focus=doc&&typeof doc==="object"?doc:null;
    applyLayout();
    const same=before.size===curNodes.length&&curNodes.every(n=>before.has(n.id));
    // A new set of cards re-fits the board; otherwise the view stays where the reader left it.
    clearBoard();buildBoard();decorate();reselect();
    if(!same)fitAll(false);
  }
  if(SESS){
    root.querySelector('[data-r="sess"]').addEventListener("click",e=>{const b=e.target.closest("[data-mode]");if(b)setMode(b.dataset.mode)});
    $("nb").addEventListener("change",e=>{showNb=e.target.checked;rebuild(true)});
    $("rest").addEventListener("pointerdown",e=>e.stopPropagation());
    $("rest").addEventListener("click",e=>{if(e.target.closest('[data-act="unfold"]'))setMode("highlight")});
  }

  buildBoard();renderOverview();fitAll(false);
  if(opts.status)setStatus(opts.status);
  if(!B.nodes.length)vp.insertAdjacentHTML("beforeend",`<div class="ab-empty"><div><b>No cards yet</b>The board for this project has not been mapped.</div></div>`);
  const start=decodeURIComponent(location.hash.slice(1));if(nodeById.has(start))select(start);
  return {setOverlay,setStatus,select,fitAll,setAgents,setJob,openJob,setFocus,setMode,watched:()=>[...openJobs]};
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
