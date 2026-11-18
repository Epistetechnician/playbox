/* Playbox QR encoder: self-contained, zero-dependency ISO/IEC 18004 QR Code (Byte mode, EC Level L, Versions 1–14, up to 458 UTF-8 bytes).
   Used for pairing links and room invite URLs (#fragment stays client-side; nothing ever leaves the browser). */
(function () {
  const EXP = new Uint8Array(512), LOG = new Uint8Array(256);
  for (let x = 1, i = 0; i < 255; i++) {
    EXP[i] = EXP[i + 255] = x;
    LOG[x] = i;
    x = (x << 1) ^ (x & 128 ? 0x11d : 0);
  }
  const gfMul = (a, b) => (a && b ? EXP[LOG[a] + LOG[b]] : 0);

  /* ISO/IEC 18004 Table 9 — EC Level L for V1..V14: [totalCw, ecPerBlock, g1Blocks, g1DataCw, g2Blocks, g2DataCw] */
  const V_ECL = [
    null,
    [26, 7, 1, 19, 0, 0],
    [44, 10, 1, 34, 0, 0],
    [70, 15, 1, 55, 0, 0],
    [100, 20, 1, 80, 0, 0],
    [134, 26, 1, 108, 0, 0],
    [172, 18, 2, 68, 0, 0],
    [196, 20, 2, 78, 0, 0],
    [242, 24, 2, 97, 0, 0],
    [292, 30, 2, 116, 0, 0],
    [346, 18, 2, 68, 2, 69],
    [404, 20, 4, 81, 0, 0],
    [466, 24, 2, 92, 2, 93],
    [532, 26, 4, 107, 0, 0],
    [581, 30, 3, 115, 1, 116],
  ];

  /* ISO/IEC 18004 Annex E — Alignment pattern center coordinates for V1..V14 */
  const ALIGN = [
    null, [], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34],
    [6, 22, 38], [6, 24, 42], [6, 26, 46], [6, 28, 50],
    [6, 30, 54], [6, 32, 58], [6, 34, 62], [6, 26, 46, 66],
  ];

  const MASKS = [
    (r, c) => (r + c) % 2 === 0,
    (r, _) => r % 2 === 0,
    (_, c) => c % 3 === 0,
    (r, c) => (r + c) % 3 === 0,
    (r, c) => (((r / 2) | 0) + ((c / 3) | 0)) % 2 === 0,
    (r, c) => ((r * c) % 2) + ((r * c) % 3) === 0,
    (r, c) => (((r * c) % 2) + ((r * c) % 3)) % 2 === 0,
    (r, c) => (((r + c) % 2) + ((r * c) % 3)) % 2 === 0,
  ];

  function utf8(str) {
    if (typeof TextEncoder !== "undefined") return new TextEncoder().encode(str);
    const out = [];
    for (let i = 0; i < str.length; i++) {
      let c = str.charCodeAt(i);
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < str.length) {
        const c2 = str.charCodeAt(i + 1);
        if (c2 >= 0xdc00 && c2 <= 0xdfff) { c = 0x10000 + ((c - 0xd800) << 10) + (c2 - 0xdc00); i++; }
      }
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
      else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
      else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    }
    return Uint8Array.from(out);
  }

  function rsRem(data, ecLen) {
    let g = [1];
    for (let i = 0; i < ecLen; i++) {
      const nxt = new Array(g.length + 1).fill(0);
      for (let j = 0; j < g.length; j++) { nxt[j] ^= g[j]; nxt[j + 1] ^= gfMul(g[j], EXP[i]); }
      g = nxt;
    }
    const gen = g.slice(1), rem = new Uint8Array(ecLen);
    for (let i = 0; i < data.length; i++) {
      const f = data[i] ^ rem[0];
      rem.copyWithin(0, 1); rem[ecLen - 1] = 0;
      if (f) for (let j = 0; j < ecLen; j++) rem[j] ^= gfMul(gen[j], f);
    }
    return rem;
  }

  function bchFormat(mask) {
    const data = (1 << 3) | mask; // EC Level L = 01
    let rem = data << 10;
    for (let i = 14; i >= 10; i--) if ((rem >> i) & 1) rem ^= 0x537 << (i - 10);
    return ((data << 10) | rem) ^ 0x5412;
  }

  function bchVersion(ver) {
    let rem = ver << 12;
    for (let i = 17; i >= 12; i--) if ((rem >> i) & 1) rem ^= 0x1f25 << (i - 12);
    return (ver << 12) | rem;
  }

  function drawFormat(m, fn, n, mask) {
    const bits = bchFormat(mask);
    for (let i = 0; i < 15; i++) {
      const b = (bits >> i) & 1;
      // Top-left copy (LSB at (0,8) .. MSB at (8,0), skipping timing line 6)
      if (i < 6) { m[i][8] = b; fn[i][8] = 1; }
      else if (i === 6) { m[7][8] = b; fn[7][8] = 1; }
      else if (i === 7) { m[8][8] = b; fn[8][8] = 1; }
      else if (i === 8) { m[8][7] = b; fn[8][7] = 1; }
      else { m[8][14 - i] = b; fn[8][14 - i] = 1; }
      // Split copy (top-right bits 0..7, bottom-left bits 8..14)
      if (i < 8) { m[8][n - 1 - i] = b; fn[8][n - 1 - i] = 1; }
      else { m[n - 15 + i][8] = b; fn[n - 15 + i][8] = 1; }
    }
    m[n - 8][8] = 1; fn[n - 8][8] = 1; // Dark module
  }

  function penalty(m, n) {
    let score = 0, dark = 0;
    for (let r = 0; r < n; r++) {
      let runC = m[r][0], runL = 1, colC = m[0][r], colL = 1;
      for (let c = 1; c < n; c++) {
        if (m[r][c] === runC) runL++; else { if (runL >= 5) score += runL - 2; runC = m[r][c]; runL = 1; }
        if (m[c][r] === colC) colL++; else { if (colL >= 5) score += colL - 2; colC = m[c][r]; colL = 1; }
      }
      if (runL >= 5) score += runL - 2;
      if (colL >= 5) score += colL - 2;
      for (let c = 0; c < n; c++) {
        if (m[r][c]) dark++;
        if (r < n - 1 && c < n - 1) {
          const v = m[r][c];
          if (v === m[r][c + 1] && v === m[r + 1][c] && v === m[r + 1][c + 1]) score += 3;
        }
        if (c <= n - 11) {
          let hr = 0, hc = 0;
          for (let k = 0; k < 11; k++) { hr = (hr << 1) | m[r][c + k]; hc = (hc << 1) | m[c + k][r]; }
          if (hr === 0x05d || hr === 0x5d0) score += 40;
          if (hc === 0x05d || hc === 0x5d0) score += 40;
        }
      }
    }
    const total = n * n;
    score += (((Math.abs(dark * 20 - total * 10) / total) | 0) * 10);
    return score;
  }

  function matrix(text) {
    if (typeof text !== "string" || !text.length) return null;
    const bytes = utf8(text);
    if (!bytes.length || bytes.length > 458) return null;

    let ver = 0;
    for (let v = 1; v <= 14; v++) {
      const [, , b1, d1, b2, d2] = V_ECL[v];
      const capBits = (b1 * d1 + b2 * d2) * 8;
      const need = 4 + (v < 10 ? 8 : 16) + bytes.length * 8;
      if (need <= capBits) { ver = v; break; }
    }
    if (!ver) return null;

    const [totalCw, ecLen, b1, d1, b2, d2] = V_ECL[ver];
    const dataCwCount = b1 * d1 + b2 * d2, capBits = dataCwCount * 8;
    const bits = [];
    const pushBits = (val, len) => { for (let i = len - 1; i >= 0; i--) bits.push((val >>> i) & 1); };
    pushBits(0b0100, 4);
    pushBits(bytes.length, ver < 10 ? 8 : 16);
    for (let i = 0; i < bytes.length; i++) pushBits(bytes[i], 8);
    const term = Math.min(4, capBits - bits.length);
    for (let i = 0; i < term; i++) bits.push(0);
    while (bits.length & 7) bits.push(0);

    const dataCw = new Uint8Array(dataCwCount);
    for (let i = 0; i < bits.length >>> 3; i++) {
      let b = 0;
      for (let j = 0; j < 8; j++) b = (b << 1) | bits[i * 8 + j];
      dataCw[i] = b;
    }
    for (let i = bits.length >>> 3, p = 0; i < dataCwCount; i++, p ^= 1) dataCw[i] = p ? 0x11 : 0xec;

    const blocks = [];
    let off = 0;
    for (let i = 0; i < b1 + b2; i++) {
      const len = i < b1 ? d1 : d2;
      const slice = dataCw.subarray(off, off + len); off += len;
      blocks.push({data: slice, ec: rsRem(slice, ecLen)});
    }
    const cw = new Uint8Array(totalCw);
    let pos = 0;
    const maxD = Math.max(d1, d2);
    for (let c = 0; c < maxD; c++) for (const b of blocks) if (c < b.data.length) cw[pos++] = b.data[c];
    for (let c = 0; c < ecLen; c++) for (const b of blocks) cw[pos++] = b.ec[c];

    const n = 17 + ver * 4;
    const base = Array.from({length: n}, () => new Uint8Array(n));
    const fn = Array.from({length: n}, () => new Uint8Array(n));
    const setFn = (r, c, v) => { if (r >= 0 && r < n && c >= 0 && c < n) { base[r][c] = v ? 1 : 0; fn[r][c] = 1; } };

    // Finder patterns + separators
    for (const [fr, fc] of [[0, 0], [0, n - 7], [n - 7, 0]]) {
      for (let dr = -1; dr <= 7; dr++) for (let dc = -1; dc <= 7; dc++) {
        const in7 = dr >= 0 && dr <= 6 && dc >= 0 && dc <= 6;
        const dark = in7 && (dr === 0 || dr === 6 || dc === 0 || dc === 6 || (dr >= 2 && dr <= 4 && dc >= 2 && dc <= 4));
        setFn(fr + dr, fc + dc, dark);
      }
    }
    // Alignment patterns
    const al = ALIGN[ver];
    for (let i = 0; i < al.length; i++) for (let j = 0; j < al.length; j++) {
      if ((i === 0 && j === 0) || (i === 0 && j === al.length - 1) || (i === al.length - 1 && j === 0)) continue;
      const cr = al[i], cc = al[j];
      for (let dr = -2; dr <= 2; dr++) for (let dc = -2; dc <= 2; dc++) {
        setFn(cr + dr, cc + dc, Math.max(Math.abs(dr), Math.abs(dc)) !== 1);
      }
    }
    // Timing patterns
    for (let i = 8; i < n - 8; i++) {
      if (!fn[6][i]) setFn(6, i, i % 2 === 0);
      if (!fn[i][6]) setFn(i, 6, i % 2 === 0);
    }
    // Reserve format areas + dark module
    drawFormat(base, fn, n, 0);
    // Version info for V7..V14
    if (ver >= 7) {
      const vb = bchVersion(ver);
      for (let i = 0; i < 18; i++) {
        const b = (vb >> i) & 1, a = (i / 3) | 0, k = i % 3;
        setFn(n - 11 + k, a, b);
        setFn(a, n - 11 + k, b);
      }
    }

    // Zigzag data placement
    let bitIdx = 0;
    const totalBits = cw.length * 8;
    for (let right = n - 1, up = true; right >= 1; right -= 2, up = !up) {
      if (right === 6) right = 5;
      for (let step = 0; step < n; step++) {
        const r = up ? n - 1 - step : step;
        for (let dx = 0; dx < 2; dx++) {
          const c = right - dx;
          if (!fn[r][c]) {
            base[r][c] = bitIdx < totalBits ? (cw[bitIdx >>> 3] >>> (7 - (bitIdx & 7))) & 1 : 0;
            bitIdx++;
          }
        }
      }
    }

    // Evaluate all 8 masks and choose lowest penalty
    let best = null, bestMask = 0, bestScore = Infinity;
    for (let mask = 0; mask < 8; mask++) {
      const cand = base.map(row => Uint8Array.from(row));
      const cond = MASKS[mask];
      for (let r = 0; r < n; r++) for (let c = 0; c < n; c++) if (!fn[r][c] && cond(r, c)) cand[r][c] ^= 1;
      drawFormat(cand, fn, n, mask);
      const sc = penalty(cand, n);
      if (sc < bestScore) { bestScore = sc; best = cand; bestMask = mask; }
    }

    const out = best.map(row => Array.from(row));
    out.version = ver; out.size = n; out.mask = bestMask;
    return out;
  }

  function svg(text, size = 168) {
    const m = matrix(text);
    if (!m) return "";
    const n = m.length;
    let d = "";
    for (let y = 0; y < n; y++) {
      for (let x = 0; x < n; x++) {
        if (!m[y][x]) continue;
        let w = 1;
        while (x + w < n && m[y][x + w]) w++;
        d += `M${x + 4} ${y + 4}h${w}v1h-${w}z`;
        x += w - 1;
      }
    }
    return `<svg class="pbqr" width="${size}" height="${size}" viewBox="0 0 ${n + 8} ${n + 8}" shape-rendering="crispEdges" role="img" aria-label="QR code"><rect width="${n + 8}" height="${n + 8}" fill="#ffffff"/><path fill="#111111" d="${d}"/></svg>`;
  }

  (typeof window !== "undefined" ? window : globalThis).PBQR = {matrix, svg};
})();
