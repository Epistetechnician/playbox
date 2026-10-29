"""A self-contained HTML page: the room's events as points; click one to see its signed entry."""

import html
import json

PAGE = """<!doctype html><meta charset=utf-8><title>Playbox world</title>
<style>body{margin:0;background:#000;color:#ddd;font:13px system-ui;display:flex;height:100vh}
canvas{flex:1;cursor:crosshair}#side{width:340px;padding:12px;overflow:auto;border-left:1px solid #333}
.linked{color:#6c6}.bad{color:#e66}.none{color:#999}code{word-break:break-all;color:#aaa}</style>
<canvas id=c></canvas><div id=side>Click a point.</div>
<script>
const D=__DATA__,cv=document.getElementById('c'),cx=cv.getContext('2d'),side=document.getElementById('side');
const COL=['#f25','#4af','#5d7','#fc3'];
function draw(sel){cv.width=cv.clientWidth;cv.height=cv.clientHeight;cx.clearRect(0,0,cv.width,cv.height);
D.points.forEach((p,i)=>{cx.beginPath();cx.fillStyle=COL[p.cluster%4];cx.globalAlpha=p.status=='linked'?1:.45;
cx.arc(p.x*cv.width,(1-p.y)*cv.height,i==sel?8:3+p.mass,0,7);cx.fill();});}
function show(i){const p=D.points[i],e=p.entry;draw(i);
side.innerHTML=`<b>${p.agent}</b> ${p.kind}<p>${p.text}</p>`+
(p.status=='linked'?`<p class=linked>linked to signed entry #${e.seq}</p><code>kind ${e.kind}<br>payload_hash ${e.payload_hash}<br>signer ${e.signer}<br>sig ${e.sig}</code>`
:`<p class=${p.status=='mismatch'||p.status=='missing'?'bad':'none'}>${p.status}: no signed entry backs this point</p>`);}
cv.onclick=ev=>{const r=cv.getBoundingClientRect(),x=(ev.clientX-r.left)/r.width,y=1-(ev.clientY-r.top)/r.height;
let b=-1,d=.03;D.points.forEach((p,i)=>{const k=Math.hypot(p.x-x,p.y-y);if(k<d){d=k;b=i}});if(b>=0)show(b)};
addEventListener('resize',()=>draw());draw();
</script>"""


def write_html(path, events, positions, labels, status, entries, masses):
    by_seq = {e.seq: e for e in entries}
    pts = []
    for ev, p, lab, st, m in zip(events, positions, labels, status, masses):
        e = by_seq.get(ev.seq) if st == "linked" else None
        pts.append({"x": float(p[0]), "y": float(p[1]), "cluster": int(lab), "mass": float(m), "agent": html.escape(ev.agent),
                    "kind": ev.kind, "text": html.escape(ev.text), "status": st,
                    "entry": e and {"seq": e.seq, "kind": e.kind, "payload_hash": e.payload_hash, "signer": e.signer, "sig": e.sig}})
    data = json.dumps({"points": pts}).replace("</", "<\\/")
    with open(path, "w") as f:
        f.write(PAGE.replace("__DATA__", data))
