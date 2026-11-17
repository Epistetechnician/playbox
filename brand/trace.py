"""Trace the delivered logo (raster mask inside an SVG) into clean vector paths -> brand/logo.svg.
Usage: python3 brand/trace.py /path/to/3.svg   (needs rsvg-convert, numpy, scipy, Pillow, contourpy)"""
import subprocess, sys, tempfile, os, math
import numpy as np
from PIL import Image
from scipy import ndimage
import contourpy

SRC = sys.argv[1]
W = 3000
tmp = os.path.join(tempfile.mkdtemp(), "l.png")
subprocess.run(["rsvg-convert", "-w", str(W), SRC, "-o", tmp], check=True)
im = np.array(Image.open(tmp)).astype(float)
alpha = im[..., 3] / 255
lum = im[..., :3].mean(-1)
a = ndimage.gaussian_filter(alpha, 1.2)
lab, n = ndimage.label(alpha > .5)
print("components", n)
ys, xs = np.where(alpha > .5)
x0, x1, y0, y1 = xs.min(), xs.max(), ys.min(), ys.max()
S = max(x1 - x0, y1 - y0)
ox, oy = (x0 + x1) / 2, (y0 + y1) / 2   # center on the mark's bbox

def dp(pts, eps):
    if len(pts) < 3: return pts
    a, b = pts[0], pts[-1]
    d = np.abs(np.cross(b - a, pts - a)) / (np.linalg.norm(b - a) + 1e-9)
    i = d.argmax()
    if d[i] > eps:
        l = dp(pts[:i + 1], eps); r = dp(pts[i:], eps)
        return np.vstack([l[:-1], r])
    return np.array([a, b])

def simplify(c, eps):
    c = c[:-1]
    i = int(np.argmax(c[:, 0] + 0.001 * c[:, 1]))
    c = np.roll(c, -i, 0)
    h = len(c) // 2
    p = np.vstack([dp(c[:h + 1], eps)[:-1], dp(np.vstack([c[h:], c[:1]]), eps)[:-1]])
    return p

def rounded(p, rad):
    out = []; n = len(p)
    for i in range(n):
        a, b, c = p[i - 1], p[i], p[(i + 1) % n]
        v1, v2 = a - b, c - b
        l1, l2 = np.linalg.norm(v1), np.linalg.norm(v2)
        r = min(rad, l1 / 2.2, l2 / 2.2)
        s, e = b + v1 / l1 * r, b + v2 / l2 * r
        out.append((s, b, e))
    d = "M%.1f %.1f" % tuple(out[0][0])
    for k, (s, b, e) in enumerate(out):
        if k: d += "L%.1f %.1f" % tuple(s)
        d += "Q%.1f %.1f %.1f %.1f" % (*b, *e)
    return d + "Z"

U = 512 / S * 0.80   # mark spans 80% of the 512 canvas
def norm(p): return (p - [ox, oy]) * U + 256

paths = []
for k in range(1, n + 1):
    m = (lab == k)
    if m.sum() < 500: continue
    cg = contourpy.contour_generator(z=ndimage.gaussian_filter(m.astype(float), 1.5))
    lines = cg.lines(0.5)
    outer = max(lines, key=len)
    col = im[..., :3][m].mean(-1).mean()
    p = simplify(outer[:, ::1], 3.0)
    p = norm(p)
    paths.append((col, rounded(p, 6.0)))
    # also holes inside the component (e.g. inner counters)
    for hole in sorted(lines, key=len)[:-1]:
        if len(hole) > 200:
            paths[-1] = (col, paths[-1][1] + rounded(norm(simplify(hole, 3.0)), 5.0))
paths.sort(key=lambda t: -t[0])
print([round(c) for c, _ in paths])
open("/tmp/claude-501/paths.txt", "w").write("\n".join(d for _, d in paths))
for c, d in paths: print(round(c), len(d))

# ---- emit assets ----
D_C, D_CUBE = paths[0][1], paths[1][1]   # lighter (outer ring) first, darker (cube) second
def svg(body, bg=""):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">{bg}{body}</svg>\n'
def two_tone(c_fill, cube_fill, stroke, sw=5):
    defs = f'<defs><clipPath id="a"><path d="{D_C}"/></clipPath><clipPath id="b"><path d="{D_CUBE}"/></clipPath></defs>'
    return (defs +
      f'<path d="{D_C}" fill="{c_fill}"/><path d="{D_C}" fill="none" stroke="{stroke}" stroke-width="{sw*2}" clip-path="url(#a)"/>'
      f'<path d="{D_CUBE}" fill="{cube_fill}"/><path d="{D_CUBE}" fill="none" stroke="{stroke}" stroke-width="{sw*2}" clip-path="url(#b)"/>')
here = os.path.dirname(os.path.abspath(__file__))
w = lambda n, s: open(os.path.join(here, n), "w").write(s)
w("logo.svg", svg(two_tone("#202124", "#000", "#3c3c3e")))
w("mark.svg", svg(f'<path d="{D_C}{D_CUBE}" fill="currentColor"/>'))
# app icon: paper tile, full-colour mark scaled to sit in the maskable safe zone
inner = f'<g transform="translate(256 256) scale(.78) translate(-256 -256)">{two_tone("#202124", "#000", "#3c3c3e")}</g>'
w("icon.svg", svg(inner, '<rect width="512" height="512" rx="112" fill="#f4f4f1"/>'))
w("icon-maskable.svg", svg(inner.replace('.78', '.66'), '<rect width="512" height="512" fill="#f4f4f1"/>'))
