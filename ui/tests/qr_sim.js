// Rigorous ISO/IEC 18004 conformance & round-trip decoder test for static/qr.js (V1..V14, multi-block RS syndromes, BCH format/version, SVG).
const fs = require("fs"), vm = require("vm"), assert = require("assert");

const ctx = {window: {}, TextEncoder};
vm.createContext(ctx);
vm.runInContext(fs.readFileSync(__dirname + "/../static/qr.js", "utf8"), ctx);
const PBQR = ctx.window.PBQR;
assert(PBQR && typeof PBQR.matrix === "function" && typeof PBQR.svg === "function", "PBQR exported");

// GF(256) (0x11d) for independent syndrome verification
const EXP = new Uint8Array(512), LOG = new Uint8Array(256);
for (let x = 1, i = 0; i < 255; i++) { EXP[i] = EXP[i + 255] = x; LOG[x] = i; x = (x << 1) ^ (x & 128 ? 0x11d : 0); }
const gfMul = (a, b) => (a && b ? EXP[LOG[a] + LOG[b]] : 0);

const TABLE9_L = {
  1: [26, 7, 1, 19, 0, 0], 2: [44, 10, 1, 34, 0, 0], 3: [70, 15, 1, 55, 0, 0], 4: [100, 20, 1, 80, 0, 0],
  5: [134, 26, 1, 108, 0, 0], 6: [172, 18, 2, 68, 0, 0], 7: [196, 20, 2, 78, 0, 0], 8: [242, 24, 2, 97, 0, 0],
  9: [292, 30, 2, 116, 0, 0], 10: [346, 18, 2, 68, 2, 69], 11: [404, 20, 4, 81, 0, 0], 12: [466, 24, 2, 92, 2, 93],
  13: [532, 26, 4, 107, 0, 0], 14: [581, 30, 3, 115, 1, 116],
};
const ALIGN = {
  1: [], 2: [6, 18], 3: [6, 22], 4: [6, 26], 5: [6, 30], 6: [6, 34],
  7: [6, 22, 38], 8: [6, 24, 42], 9: [6, 26, 46], 10: [6, 28, 50],
  11: [6, 30, 54], 12: [6, 32, 58], 13: [6, 34, 62], 14: [6, 26, 46, 66],
};
const MASKS = [
  (r, c) => (r + c) % 2 === 0, (r, _) => r % 2 === 0, (_, c) => c % 3 === 0, (r, c) => (r + c) % 3 === 0,
  (r, c) => (((r / 2) | 0) + ((c / 3) | 0)) % 2 === 0, (r, c) => ((r * c) % 2) + ((r * c) % 3) === 0,
  (r, c) => (((r * c) % 2) + ((r * c) % 3)) % 2 === 0, (r, c) => (((r + c) % 2) + ((r * c) % 3)) % 2 === 0,
];

function checkFinderAndStructure(m, expectedVer) {
  const n = m.length, ver = (n - 17) / 4;
  assert.strictEqual(ver, expectedVer, `expected V${expectedVer}, got V${ver}`);
  for (const [fr, fc] of [[0, 0], [0, n - 7], [n - 7, 0]]) {
    for (let dr = -1; dr <= 7; dr++) for (let dc = -1; dc <= 7; dc++) {
      const r = fr + dr, c = fc + dc;
      if (r < 0 || r >= n || c < 0 || c >= n) continue;
      const in7 = dr >= 0 && dr <= 6 && dc >= 0 && dc <= 6;
      const want = in7 && (dr === 0 || dr === 6 || dc === 0 || dc === 6 || (dr >= 2 && dr <= 4 && dc >= 2 && dc <= 4)) ? 1 : 0;
      assert.strictEqual(m[r][c], want, `finder/separator mismatch at (${r},${c}) in V${ver}`);
    }
  }
  assert.strictEqual(m[n - 8][8], 1, `dark module missing in V${ver}`);
  const al = ALIGN[ver];
  const inAlign = (r, c) => al.some((ar, i) => al.some((ac, j) => {
    if ((i === 0 && j === 0) || (i === 0 && j === al.length - 1) || (i === al.length - 1 && j === 0)) return false;
    return Math.abs(r - ar) <= 2 && Math.abs(c - ac) <= 2;
  }));
  for (let i = 8; i < n - 8; i++) {
    if (!inAlign(6, i)) assert.strictEqual(m[6][i], i % 2 === 0 ? 1 : 0, `row timing at (6,${i}) in V${ver}`);
    if (!inAlign(i, 6)) assert.strictEqual(m[i][6], i % 2 === 0 ? 1 : 0, `col timing at (${i},6) in V${ver}`);
  }
}

function decodeQR(m) {
  const n = m.length, ver = (n - 17) / 4;
  // 1. Read both copies of 15-bit format info
  let fmt1 = 0, fmt2 = 0;
  for (let i = 0; i < 15; i++) {
    const b1 = i < 6 ? m[i][8] : i === 6 ? m[7][8] : i === 7 ? m[8][8] : i === 8 ? m[8][7] : m[8][14 - i];
    const b2 = i < 8 ? m[8][n - 1 - i] : m[n - 15 + i][8];
    fmt1 |= b1 << i;
    fmt2 |= b2 << i;
  }
  assert.strictEqual(fmt1, fmt2, "both 15-bit format copies match");
  // Known ISO Table C.1 format strings for EC Level L (01), masks 0..7
  const ISO_FMT_L = [0x77c4, 0x72f3, 0x7daa, 0x789d, 0x662f, 0x6318, 0x6c41, 0x6976];
  const rawFmt = fmt1 ^ 0x5412;
  let rem = rawFmt;
  for (let i = 14; i >= 10; i--) if ((rem >> i) & 1) rem ^= 0x537 << (i - 10);
  assert.strictEqual(rem, 0, "format BCH remainder is 0");
  const ecInd = (rawFmt >> 13) & 3, mask = (rawFmt >> 10) & 7;
  assert.strictEqual(ecInd, 1, "EC level indicator is 01 (L)");
  assert.strictEqual(fmt1, ISO_FMT_L[mask], "matches ISO Table C.1");

  // 2. For V7+, read both 18-bit version info blocks and check BCH (0x1f25) + ISO Table D.1
  if (ver >= 7) {
    let v1 = 0, v2 = 0;
    for (let i = 0; i < 18; i++) {
      const a = (i / 3) | 0, k = i % 3;
      v1 |= m[n - 11 + k][a] << i;
      v2 |= m[a][n - 11 + k] << i;
    }
    assert.strictEqual(v1, v2, `both 18-bit version blocks match in V${ver}`);
    let vrem = v1;
    for (let i = 17; i >= 12; i--) if ((vrem >> i) & 1) vrem ^= 0x1f25 << (i - 12);
    assert.strictEqual(vrem, 0, `version BCH remainder is 0 in V${ver}`);
    assert.strictEqual(v1 >> 12, ver, `decoded version ${v1 >> 12} === ${ver}`);
    const ISO_VER = {7: 0x07c94, 10: 0x0a4d3, 14: 0x0e60d};
    if (ISO_VER[ver]) assert.strictEqual(v1, ISO_VER[ver], `V${ver} matches ISO Table D.1`);
  }

  // 3. Build function module mask
  const fn = Array.from({length: n}, () => new Uint8Array(n));
  for (const [fr, fc] of [[0, 0], [0, n - 7], [n - 7, 0]]) {
    for (let dr = -1; dr <= 7; dr++) for (let dc = -1; dc <= 7; dc++) {
      const r = fr + dr, c = fc + dc;
      if (r >= 0 && r < n && c >= 0 && c < n) fn[r][c] = 1;
    }
  }
  const al = ALIGN[ver];
  for (let i = 0; i < al.length; i++) for (let j = 0; j < al.length; j++) {
    if ((i === 0 && j === 0) || (i === 0 && j === al.length - 1) || (i === al.length - 1 && j === 0)) continue;
    for (let dr = -2; dr <= 2; dr++) for (let dc = -2; dc <= 2; dc++) fn[al[i] + dr][al[j] + dc] = 1;
  }
  for (let i = 8; i < n - 8; i++) { fn[6][i] = 1; fn[i][6] = 1; }
  for (let i = 0; i < 9; i++) { fn[8][i] = 1; fn[i][8] = 1; }
  for (let i = 0; i < 8; i++) { fn[8][n - 1 - i] = 1; fn[n - 1 - i][8] = 1; }
  if (ver >= 7) for (let i = 0; i < 18; i++) { const a = (i / 3) | 0, k = i % 3; fn[n - 11 + k][a] = 1; fn[a][n - 11 + k] = 1; }

  // 4. Unmask and extract zigzag bit stream
  const bits = [];
  const cond = MASKS[mask];
  for (let right = n - 1, up = true; right >= 1; right -= 2, up = !up) {
    if (right === 6) right = 5;
    for (let step = 0; step < n; step++) {
      const r = up ? n - 1 - step : step;
      for (let dx = 0; dx < 2; dx++) {
        const c = right - dx;
        if (!fn[r][c]) bits.push(m[r][c] ^ (cond(r, c) ? 1 : 0));
      }
    }
  }

  const [totalCw, ecLen, b1, d1, b2, d2] = TABLE9_L[ver];
  assert(bits.length >= totalCw * 8, "enough module bits");
  const cw = new Uint8Array(totalCw);
  for (let i = 0; i < totalCw; i++) {
    let b = 0;
    for (let j = 0; j < 8; j++) b = (b << 1) | bits[i * 8 + j];
    cw[i] = b;
  }

  // 5. De-interleave RS blocks and verify RS syndromes == 0 for every root alpha^0 .. alpha^(ecLen-1)
  const numBlocks = b1 + b2;
  const blocks = Array.from({length: numBlocks}, (_, i) => ({data: [], ec: [], dLen: i < b1 ? d1 : d2}));
  let pos = 0;
  const maxD = Math.max(d1, d2);
  for (let c = 0; c < maxD; c++) for (const b of blocks) if (c < b.dLen) b.data.push(cw[pos++]);
  for (let c = 0; c < ecLen; c++) for (const b of blocks) b.ec.push(cw[pos++]);
  assert.strictEqual(pos, totalCw, "all codewords de-interleaved");

  for (let bi = 0; bi < blocks.length; bi++) {
    const poly = blocks[bi].data.concat(blocks[bi].ec);
    for (let root = 0; root < ecLen; root++) {
      let syn = 0;
      for (let j = 0; j < poly.length; j++) syn = gfMul(syn, EXP[root]) ^ poly[j];
      assert.strictEqual(syn, 0, `V${ver} block ${bi} RS syndrome[${root}] must be 0`);
    }
  }

  // 6. Reassemble data bitstream and decode Byte mode UTF-8 payload
  const dataBytes = [];
  for (const b of blocks) for (const x of b.data) dataBytes.push(x);
  const dbits = [];
  for (const x of dataBytes) for (let j = 7; j >= 0; j--) dbits.push((x >> j) & 1);
  const readBits = (off, len) => { let v = 0; for (let i = 0; i < len; i++) v = (v << 1) | dbits[off + i]; return v; };
  assert.strictEqual(readBits(0, 4), 0b0100, "Byte mode 0100");
  const ccBits = ver < 10 ? 8 : 16;
  const byteLen = readBits(4, ccBits);
  const out = new Uint8Array(byteLen);
  for (let i = 0; i < byteLen; i++) out[i] = readBits(4 + ccBits + i * 8, 8);
  return new TextDecoder().decode(out);
}

// Structural checks on V1, V4, V7, V10, V14
const vSamples = [
  [1, "HELLO PLAYBOX"],
  [4, "https://hub.playbox.example/pair#" + "A".repeat(30)],
  [7, "https://hub.playbox.example/i/9f8e7d6c5b4a3210#" + "k".repeat(95)],
  [10, "https://hub.playbox.example/i/9f8e7d6c5b4a3210#playbox://" + "x".repeat(190)],
  [14, "https://hub.playbox.example/join#" + "Z".repeat(400)],
];
for (const [wantVer, sample] of vSamples) {
  const m = PBQR.matrix(sample);
  assert(m, `matrix generated for V${wantVer}`);
  checkFinderAndStructure(m, wantVer);
  assert.strictEqual(decodeQR(m), sample, `round-trip decode for V${wantVer}`);
}
console.log("PASS structure, finders, separators, timing, dark module & BCH version/format across V1, V4, V7, V10, V14");

// Exhaustive exact-capacity round-trip across ALL versions V1..V14 + real Playbox URLs + UTF-8
const exactCaps = [17, 32, 53, 78, 106, 134, 154, 192, 230, 271, 321, 367, 425, 458];
exactCaps.forEach((len, idx) => {
  const s = Array.from({length: len}, (_, i) => String.fromCharCode(33 + ((i * 37 + idx) % 90))).join("");
  const m = PBQR.matrix(s);
  assert.strictEqual(m.version, idx + 1, `exact max capacity ${len}B selects V${idx + 1}`);
  assert.strictEqual(decodeQR(m), s, `exact capacity ${len}B round-trips with 0 RS syndrome`);
});
console.log("PASS all V1..V14 exact capacity boundaries (up to 458 bytes) round-trip with 0 RS syndrome");

const realPayloads = [
  "a",
  "Playbox — café 🚀✨",
  "https://playbox-demo.trycloudflare.com/pair#w8K9m2Qx7Lp4Vn6Zr1Ty3Bc5Df8Gh0Jk", // ~78 chars
  "https://playbox-demo.trycloudflare.com/i/4a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d#a.9f8e7d6c5b4a392817263544536271809f8e7d6c5b4a", // 121-char pairing/approval URL
  "https://playbox-demo.trycloudflare.com/i/4a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d#playbox%3A%2F%2F127.0.0.1%3A7717%2Fjoin%3Fgrant%3D" + "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCJ9.".repeat(5) + "sig_end", // ~320-char invite URL with #fragment
];
for (const p of realPayloads) {
  const m = PBQR.matrix(p);
  assert.strictEqual(decodeQR(m), p);
}
console.log("PASS short ASCII, UTF-8 multi-byte, 120-char pairing URL, and 320-char invite URL with #fragment");

// SVG output & bounds validation
const svg = PBQR.svg("https://hub/pair#code123", 180);
assert(svg.startsWith('<svg class="pbqr" width="180" height="180" viewBox="0 0 33 33"'), "SVG header + 4-module quiet zone viewBox");
assert(svg.includes('fill="#ffffff"') && svg.includes('fill="#111111"'), "high-contrast white bg + dark modules");
assert.strictEqual(PBQR.svg(""), "", "empty string returns empty SVG");
assert.strictEqual(PBQR.svg("x".repeat(459)), "", ">458 UTF-8 bytes returns empty SVG");
console.log("PASS SVG quiet-zone rendering and payload bounds");
