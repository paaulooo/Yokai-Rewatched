"""Rasterizador de teste (numpy, so para VALIDAR dados): desenha as malhas de um .xc com a textura resolvida por mtr_probe.
Uso: python render_probe.py CAMINHO.xc SAIDA.png [--view front|top|iso] [--exclude substr,...] [--only substr,...] [--size 512]
Convencoes validadas aqui: strip de triangulos (prm.strip_to_tris), UV com v invertido na amostragem (origem sup. esq.), Y para cima."""
import sys, os, struct, zlib
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import yw_fa, xpck, prm, imgc, mtr_probe as mp
from PIL import Image

def load_tex(es, i, cache={}):
    k = (id(es), i)
    if k not in cache:
        img, H = imgc.decode(es['%03d.xi' % i]); cache[k] = img
    return cache[k]

def render(es, out, view='front', size=512, only=None, exclude=None, extra_tex=None):
    ch, res = mp.resolve(es)
    tris_all = []
    for n, sk, mt, xi, tn, ok in res:
        nm = sk + ' ' + mt
        if only and not any(o in nm for o in only): continue
        if exclude and any(o in nm for o in exclude): continue
        if xi is None: continue
        p = prm.parse(es[n]); V = p['vertices']
        P = V[0][:, :3]; UV = V[4][:, :2] if 4 in V else np.zeros((len(P), 2))
        t = prm.strip_to_tris(p['indices'])
        tex = load_tex(es, xi)
        CC = V[1] if 1 in V else np.ones((len(P), 4))
        tris_all.append((P, UV, np.array(t, dtype=np.int64).reshape(-1, 3), tex, CC))
    allp = np.concatenate([a[0] for a in tris_all])
    lo, hi = allp.min(0), allp.max(0); c = (lo + hi) / 2
    # camera
    if view == 'front': R = np.eye(3)
    elif view == 'top': R = np.array([[1, 0, 0], [0, 0, 1], [0, -1, 0]], float)
    else:
        a, b = np.radians(35), np.radians(-30)
        Ry = np.array([[np.cos(a), 0, np.sin(a)], [0, 1, 0], [-np.sin(a), 0, np.cos(a)]])
        Rx = np.array([[1, 0, 0], [0, np.cos(b), -np.sin(b)], [0, np.sin(b), np.cos(b)]])
        R = Rx @ Ry
    q = (allp - c) @ R.T; ext = np.abs(q[:, :2]).max() * 1.05
    img = np.full((size, size, 3), 40, np.uint8); zb = np.full((size, size), -1e30)
    for P, UV, T, tex, CC in tris_all:
        Q = (P - c) @ R.T
        sx = (Q[:, 0] / ext * 0.5 + 0.5) * (size - 1); sy = (0.5 - Q[:, 1] / ext * 0.5) * (size - 1); z = Q[:, 2]
        th, tw = tex.shape[:2]
        for a, b, cc in T:
            xs = sx[[a, b, cc]]; ys = sy[[a, b, cc]]
            x0, x1 = int(max(xs.min(), 0)), int(min(xs.max(), size - 1)); y0, y1 = int(max(ys.min(), 0)), int(min(ys.max(), size - 1))
            if x1 < x0 or y1 < y0: continue
            den = (ys[1] - ys[2]) * (xs[0] - xs[2]) + (xs[2] - xs[1]) * (ys[0] - ys[2])
            if abs(den) < 1e-9: continue
            gx, gy = np.meshgrid(np.arange(x0, x1 + 1), np.arange(y0, y1 + 1))
            w0 = ((ys[1] - ys[2]) * (gx - xs[2]) + (xs[2] - xs[1]) * (gy - ys[2])) / den
            w1 = ((ys[2] - ys[0]) * (gx - xs[2]) + (xs[0] - xs[2]) * (gy - ys[2])) / den
            w2 = 1 - w0 - w1
            m = (w0 >= 0) & (w1 >= 0) & (w2 >= 0)
            if not m.any(): continue
            zz = w0 * z[a] + w1 * z[b] + w2 * z[cc]
            m &= zz > zb[y0:y1 + 1, x0:x1 + 1]
            if not m.any(): continue
            u = w0 * UV[a, 0] + w1 * UV[b, 0] + w2 * UV[cc, 0]; v = w0 * UV[a, 1] + w1 * UV[b, 1] + w2 * UV[cc, 1]
            tx = np.clip((u * tw).astype(int), 0, tw - 1); ty = np.clip(((v) * th).astype(int), 0, th - 1)
            col = tex[ty, tx].copy()
            cr = w0[..., None] * CC[a, :3] + w1[..., None] * CC[b, :3] + w2[..., None] * CC[cc, :3]
            col[..., :3] = np.clip(col[..., :3] * cr, 0, 255).astype(np.uint8)
            al = col[..., 3] > 8
            m &= al
            sub = img[y0:y1 + 1, x0:x1 + 1]; sub[m] = col[..., :3][m]
            zb[y0:y1 + 1, x0:x1 + 1][m] = zz[m]
    Image.fromarray(img).save(out)
    return len(tris_all)

if __name__ == '__main__':
    a = sys.argv[1:]
    path, out = a[0], a[1]
    def opt(k, d=None): return a[a.index(k) + 1] if k in a else d
    es = dict(xpck.read(yw_fa.get(path)))
    n = render(es, out, opt('--view', 'front'), int(opt('--size', 512)),
               opt('--only').split(',') if opt('--only') else None, opt('--exclude').split(',') if opt('--exclude') else None)
    print('malhas desenhadas:', n, '->', out)
