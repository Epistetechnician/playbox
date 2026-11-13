/* Playbox ASCII renderer.
 * A low-res "source" canvas (one pixel per character cell) is turned into text by a WebGL shader:
 * luminance picks a glyph from an atlas sorted by measured ink density, chroma picks ink vs signal colour.
 * Output backing store goes up to 3840x2160, so the glyphs stay sharp on a 4K panel. */
(() => {
const RAMP = " .'`,:;~-_+<>i!lI?/|()1{}[]rcvunxzjftLCJUYXZO0Qoahkbdpqwm*WMB8&%$#@";
const GW = 24, GH = 48; // atlas glyph size in px (2:1 cell)

function buildAtlas(font) {
  const cv = document.createElement("canvas"), cx = cv.getContext("2d", { willReadFrequently: true });
  const meas = [];
  const tmp = document.createElement("canvas"); tmp.width = GW; tmp.height = GH;
  const t = tmp.getContext("2d", { willReadFrequently: true });
  for (const ch of RAMP) {
    t.clearRect(0, 0, GW, GH); t.fillStyle = "#fff"; t.font = `${GH * 0.78}px ${font}`; t.textAlign = "center"; t.textBaseline = "middle";
    t.fillText(ch, GW / 2, GH / 2 + 1);
    const d = t.getImageData(0, 0, GW, GH).data; let s = 0; for (let i = 3; i < d.length; i += 4) s += d[i];
    meas.push([ch, s]);
  }
  meas.sort((a, b) => a[1] - b[1]);
  const glyphs = meas.map(m => m[0]); const n = glyphs.length;
  cv.width = GW * n; cv.height = GH;
  cx.fillStyle = "#000"; cx.fillRect(0, 0, cv.width, cv.height);
  cx.fillStyle = "#fff"; cx.font = `${GH * 0.78}px ${font}`; cx.textAlign = "center"; cx.textBaseline = "middle";
  glyphs.forEach((g, i) => cx.fillText(g, i * GW + GW / 2, GH / 2 + 1));
  return { cv, n };
}

const VS = `attribute vec2 p;varying vec2 v;void main(){v=p*.5+.5;gl_Position=vec4(p,0.,1.);}`;
const FS = `precision highp float;varying vec2 v;
uniform sampler2D src,atlas;uniform vec2 cells;uniform float n,gamma,inv,time;uniform vec3 ink,sig,bg;uniform float flick;
float hash(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
void main(){
  vec2 uv=vec2(v.x,1.-v.y)*cells; vec2 c=floor(uv), f=fract(uv);
  vec3 s=texture2D(src,(c+.5)/cells).rgb;
  float l=dot(s,vec3(.299,.587,.114)); if(inv>.5) l=1.-l;
  l=pow(clamp(l,0.,1.),gamma);
  float idx=floor(l*(n-1.)+.5+(hash(c+floor(time*8.))-.5)*flick);
  idx=clamp(idx,0.,n-1.);
  float a=texture2D(atlas,vec2((idx+f.x)/n,f.y)).r;
  float chroma=clamp((max(s.r,max(s.g,s.b))-min(s.r,min(s.g,s.b)))*2.2,0.,1.);
  vec3 col=mix(ink,sig,chroma);
  gl_FragColor=vec4(mix(bg,col,a*step(.5/(n-1.),l)),1.);
}`;

class AsciiView {
  constructor(canvas, opts) {
    this.cv = canvas; this.o = Object.assign({ colsPerCssPx: 1 / 7, minCols: 120, maxCols: 420, gamma: 0.9, invert: false, flicker: 0, maxW: 3840, maxH: 2160, fps: 60 }, opts);
    const gl = this.gl = canvas.getContext("webgl", { antialias: false, alpha: false, powerPreference: "high-performance" });
    if (!gl) throw new Error("webgl unavailable");
    const mk = (t, s) => { const h = gl.createShader(t); gl.shaderSource(h, s); gl.compileShader(h); if (!gl.getShaderParameter(h, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(h)); return h; };
    const pr = this.pr = gl.createProgram(); gl.attachShader(pr, mk(gl.VERTEX_SHADER, VS)); gl.attachShader(pr, mk(gl.FRAGMENT_SHADER, FS)); gl.linkProgram(pr); gl.useProgram(pr);
    const b = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, b); gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const loc = gl.getAttribLocation(pr, "p"); gl.enableVertexAttribArray(loc); gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
    this.u = n => gl.getUniformLocation(pr, n);
    const tex = (unit) => { const t = gl.createTexture(); gl.activeTexture(gl.TEXTURE0 + unit); gl.bindTexture(gl.TEXTURE_2D, t);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR); gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR); return t; };
    this.srcTex = tex(0); this.atlasTex = tex(1);
    const font = getComputedStyle(document.documentElement).getPropertyValue("--mono") || "monospace";
    const at = buildAtlas(font); gl.activeTexture(gl.TEXTURE1); gl.bindTexture(gl.TEXTURE_2D, this.atlasTex); gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, at.cv);
    gl.uniform1i(this.u("src"), 0); gl.uniform1i(this.u("atlas"), 1); gl.uniform1f(this.u("n"), at.n);
    this.src = document.createElement("canvas"); this.sx = this.src.getContext("2d", { willReadFrequently: false });
    this.t0 = performance.now(); this.running = false; this.visible = true; this.force4k = false;
    this.ro = new ResizeObserver(() => this.resize()); this.ro.observe(canvas);
    this.io = new IntersectionObserver(e => { this.visible = e[0].isIntersecting; }); this.io.observe(canvas);
    this.resize();
  }
  setForce4k(v) { this.force4k = v; this.resize(); }
  resize() {
    const r = this.cv.getBoundingClientRect(); if (!r.width) return;
    const dpr = Math.min(window.devicePixelRatio || 1, 3);
    let w = Math.round(r.width * dpr), h = Math.round(r.height * dpr);
    if (this.force4k) { const k = Math.max(3840 / w, 2160 / h); w = Math.round(w * k); h = Math.round(h * k); }
    const k = Math.min(1, this.o.maxW / w, this.o.maxH / h); w = Math.round(w * k); h = Math.round(h * k);
    this.cv.width = w; this.cv.height = h; this.gl.viewport(0, 0, w, h);
    const cols = Math.max(this.o.minCols, Math.min(this.o.maxCols, Math.round(r.width * this.o.colsPerCssPx)));
    const rows = Math.max(1, Math.round(cols * (h / w) / 2));
    this.cols = cols; this.rows = rows; this.src.width = cols; this.src.height = rows;
    this.gl.uniform2f(this.u("cells"), cols, rows);
    this.info = `${w}×${h} · ${cols}×${rows} cells`; if (this.onInfo) this.onInfo(this.info);
  }
  colors() {
    const cs = getComputedStyle(document.documentElement);
    const dark = matchMedia("(prefers-color-scheme:dark)").matches ? document.documentElement.dataset.theme !== "light" : document.documentElement.dataset.theme === "dark";
    const hex = (s, f) => { s = (cs.getPropertyValue(s) || f).trim(); const m = /^#?([0-9a-f]{6})$/i.exec(s); const n = parseInt((m || [0, f.replace("#", "")])[1], 16); return [(n >> 16) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255]; };
    return dark ? { dark: true, bg: hex("--ascii-bg-d", "#0F0F0E"), ink: hex("--ascii-ink-d", "#ECE9E0"), sig: hex("--ascii-sig-d", "#FF6B2C") }
                : { bg: hex("--ascii-bg-l", "#E9E7E0"), ink: hex("--ascii-ink-l", "#171715"), sig: hex("--ascii-sig-l", "#FF5A1F") };
  }
  start(draw) { this.draw = draw; if (this.running) return; this.running = true; let last = 0; const step = (now) => {
      if (!this.running) return; requestAnimationFrame(step);
      if (!this.visible || document.hidden) return;
      if (now - last < 1000 / this.o.fps - 1) return; last = now;
      this.frame(now); }; requestAnimationFrame(step); }
  frame(now) {
    const gl = this.gl, t = (now - this.t0) / 1000;
    const sx = this.sx; sx.setTransform(1, 0, 0, 1, 0, 0); sx.fillStyle = "#000"; sx.fillRect(0, 0, this.cols, this.rows);
    this.draw(sx, this.cols, this.rows, t);
    gl.activeTexture(gl.TEXTURE0); gl.bindTexture(gl.TEXTURE_2D, this.srcTex); gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, this.src);
    const c = this.colors();
    gl.uniform3fv(this.u("bg"), c.bg); gl.uniform3fv(this.u("ink"), c.ink); gl.uniform3fv(this.u("sig"), c.sig);
    gl.uniform1f(this.u("gamma"), this.o.gamma); gl.uniform1f(this.u("inv"), (this.o.invert !== (this.o.autoInvert && !c.dark)) ? 1 : 0); gl.uniform1f(this.u("time"), t); gl.uniform1f(this.u("flick"), this.o.flicker);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }
}

/* ---------- source 1: the box (procedural, runs at any resolution) ---------- */
function boxScene() {
  const V = [[-1,-1,-1],[1,-1,-1],[1,1,-1],[-1,1,-1],[-1,-1,1],[1,-1,1],[1,1,1],[-1,1,1]];
  const F = [[0,1,2,3],[4,7,6,5],[0,4,5,1],[3,2,6,7],[0,3,7,4],[1,5,6,2]];
  const rot = (p, a, b) => { let [x, y, z] = p; let c = Math.cos(a), s = Math.sin(a); [y, z] = [y * c - z * s, y * s + z * c]; c = Math.cos(b); s = Math.sin(b); [x, z] = [x * c + z * s, -x * s + z * c]; return [x, y, z]; };
  const agents = 5, trails = Array.from({ length: agents }, () => []);
  return (g, cols, rows, t) => {
    const W = cols, H = rows * 2; g.scale(1, 0.5);
    const wide = W / H > 1.2, S = Math.min(W * 0.14, H * 0.22), cx = W * (wide ? 0.68 : 0.62), cy = H * (wide ? 0.36 : 0.3), cam = 5.2;
    const proj = (p) => { const k = cam / (cam + p[2]); return [cx + p[0] * S * k, cy + p[1] * S * k, p[2]]; };
    // faint field, so the page is never empty
    g.fillStyle = "rgba(255,255,255,.05)"; for (let y = 0; y < H; y += 6) for (let x = ((y / 6) & 1) * 3; x < W; x += 6) g.fillRect(x, y, 1.4, 1.4);
    // orbit rings + agents
    const a = t * 0.35, b = 0.5 + Math.sin(t * 0.17) * 0.12;
    for (let r = 0; r < 2; r++) { g.strokeStyle = `rgba(255,255,255,${0.12 - r * 0.04})`; g.lineWidth = 1.1; g.beginPath();
      for (let i = 0; i <= 96; i++) { const th = i / 96 * Math.PI * 2, R = 2.6 + r * 0.9; const q = proj(rot([Math.cos(th) * R, 0, Math.sin(th) * R], b + r * 0.5, 0.2)); i ? g.lineTo(q[0], q[1]) : g.moveTo(q[0], q[1]); } g.stroke(); }
    const pts = [];
    for (let i = 0; i < agents; i++) { const th = a * (1 + i * 0.13) + i * Math.PI * 2 / agents, R = 2.6 + (i % 2) * 0.9;
      const q = proj(rot([Math.cos(th) * R, 0, Math.sin(th) * R], b + (i % 2) * 0.5, 0.2)); pts.push(q);
      const tr = trails[i]; tr.push(q); if (tr.length > 26) tr.shift();
      for (let j = 1; j < tr.length; j++) { g.strokeStyle = `rgba(255,255,255,${j / tr.length * 0.35})`; g.lineWidth = 1.2; g.beginPath(); g.moveTo(tr[j - 1][0], tr[j - 1][1]); g.lineTo(tr[j][0], tr[j][1]); g.stroke(); } }
    // box
    const vs = V.map(p => rot(p, t * 0.5, t * 0.37)), pv = vs.map(proj);
    const faces = F.map(f => { const n = cross(sub(vs[f[1]], vs[f[0]]), sub(vs[f[2]], vs[f[0]])); const z = f.reduce((s, i) => s + vs[i][2], 0) / 4; return { f, n, z }; }).sort((p, q) => q.z - p.z);
    const L = norm([-0.4, -0.7, -0.6]);
    for (const { f, n, z } of faces) { const nn = norm(n); if (nn[2] > 0.02) continue; // back face
      const d = Math.max(0, -(nn[0] * L[0] + nn[1] * L[1] + nn[2] * L[2])); const v = 0.04 + d * 0.26;
      g.fillStyle = `rgb(${v * 255 | 0},${v * 255 | 0},${v * 255 | 0})`; g.beginPath(); f.forEach((i, k) => k ? g.lineTo(pv[i][0], pv[i][1]) : g.moveTo(pv[i][0], pv[i][1])); g.closePath(); g.fill();
      g.strokeStyle = "rgba(255,255,255,.95)"; g.lineWidth = 1.1; g.stroke(); }
    // the lid slides open on a loop: the "room" glowing inside
    const open = (Math.sin(t * 0.6) + 1) / 2, glow = 0.35 + open * 0.65; const c = proj(rot([0, 0, 0], 0, 0));
    const gr = g.createRadialGradient(c[0], c[1], 0, c[0], c[1], S * 0.55); gr.addColorStop(0, `rgba(255,${110 + open * 40 | 0},40,${glow * 0.55})`); gr.addColorStop(1, "rgba(255,90,31,0)"); g.fillStyle = gr; g.globalCompositeOperation = "lighter"; g.beginPath(); g.arc(c[0], c[1], S * 0.55, 0, 7); g.fill(); g.globalCompositeOperation = "source-over";
    // signed packets travelling agent -> box (the only orange: "look here")
    for (let i = 0; i < agents; i++) { const ph = (t * 0.45 + i * 0.37) % 1, p = pts[i]; const x = p[0] + (c[0] - p[0]) * ph, y = p[1] + (c[1] - p[1]) * ph;
      g.strokeStyle = "rgba(255,255,255,.14)"; g.lineWidth = 1; g.beginPath(); g.moveTo(p[0], p[1]); g.lineTo(c[0], c[1]); g.stroke();
      g.fillStyle = "rgb(255,110,40)"; g.fillRect(x - 2.2, y - 2.2, 4.4, 4.4);
      g.fillStyle = "#fff"; g.beginPath(); g.arc(p[0], p[1], 3.2, 0, 7); g.fill(); }
  };
}
const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const norm = a => { const l = Math.hypot(...a) || 1; return [a[0] / l, a[1] / l, a[2] / l]; };

/* ---------- source 2: a <video>, sampled to the cell grid ---------- */
function videoScene(video) {
  return (g, cols, rows) => {
    if (video.readyState < 2) return;
    const vr = video.videoWidth / video.videoHeight, cr = cols * 2 / rows; let w = cols, h = rows, x = 0, y = 0;
    if (vr > cr) { w = rows * vr / 2; x = (cols - w) / 2; } else { h = cols / vr / 2 * 2; h = cols / vr / 2; y = (rows - h) / 2; }
    g.imageSmoothingEnabled = true; g.imageSmoothingQuality = "high"; g.drawImage(video, x, y, w, h);
    // contrast: push mids apart so UI edges read as text
    g.globalCompositeOperation = "source-over";
  };
}

window.PlayboxAscii = { AsciiView, boxScene, videoScene };
})();
