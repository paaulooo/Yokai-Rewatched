"""Renderizador de referencia (Python/numpy) de layouts de menu 2D (.xa) -> PNG em work/notes/render/.
Mesma regra do engine/src/layout.rs: tela = posicao da instancia + deslocamento do keyframe + posicao local (pvb);
ordem = z decrescente. Serve para conferir visualmente o que o Rust deve desenhar.

Uso: python menu_render.py data/menu/title_u00_en.xa bg00@0 title_00@0 youkai_wa@40 --size 400x240 -o title_u.png
     (NOME@QUADRO; QUADRO em 60 Hz, ciclico; sufixo ,dx,dy desloca)   --list lista instancias
Validado: title_u/title_d (mesma composicao do Rust). Atributos XPVB: slot0=pos(float3), 1o float2 nao constante = UV.
"""
import sys, os, struct, zlib, argparse
import numpy as np
from PIL import Image
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import WORK
import l5, xpck, imgc, menu_probe as mp

def xpvb_parse(buf):
    assert buf[:4] == b'XPVB'
    oa, of, ov, stride, count = struct.unpack_from('<HHHHI', buf, 4)
    attrs = l5.l5dec(buf[oa:])
    data = l5.l5dec(buf[ov:])
    al = [struct.unpack_from('4B', attrs, i * 4) for i in range(len(attrs) // 4)]
    return dict(stride=stride, count=count, attrs=al, data=np.frombuffer(data, np.uint8))

def xpvi_parse(buf):
    off, count = struct.unpack_from('<HI', buf, 6)
    raw = l5.l5dec(buf[off:])
    return np.frombuffer(raw, '<u2')[:count].astype(int) if len(raw) >= count * 2 else np.frombuffer(raw, np.uint8)[:count].astype(int)

class Layout:
    def __init__(self, path):
        ents, a, _ = mp.load_xa(path)
        self.a = a; files = dict(ents)
        self.tex = []
        for i in range(len(a['tex'])):
            img, _H = imgc.decode(files['%03d.xi' % i]); self.tex.append(img)
        vb = xpvb_parse(files['000.pvb'])
        uvslot = next(i for i, t in enumerate(vb['attrs']) if t[2] and t[0] == 2 and not t[3] & 1)
        def vattr(slot, n):
            t = vb['attrs'][slot]; off = t[1]
            return np.array([struct.unpack_from('<%df' % n, vb['data'].tobytes(), k * vb['stride'] + off) for k in range(vb['count'])])
        self.pos = vattr(0, 3); self.uv = vattr(uvslot, 2)
        self.meshes = []
        for i, m in enumerate(a['meshes']):
            mat = next(x for x in a['mats'] if x['h'] == m['mat'])
            # hash 0 = material sem textura (ex. `#cover 0`): desenha cor sólida, como o Rust.
            ti = a['tex'].index(mat['tex']) if mat['tex'] else None
            idx = xpvi_parse(files['%03d.pbi' % i]); self.meshes.append((ti, idx.reshape(-1, 3)))
        by_hash = {m['h']: i for i, m in enumerate(a['meshes'])}
        self.node_meshes = {n['h']: [by_hash[zlib.crc32(('#%s %d' % (n['name'], k)).encode())] for k in range(n['nmesh'])
                                      if zlib.crc32(('#%s %d' % (n['name'], k)).encode()) in by_hash] for n in a['nodes']}
        self.inst = {x['name']: x for x in a['insts']}

    def key_at(self, name, frame):
        x = self.inst[name]; tot = max(1, sum(k['frames'] for k in x['keys'])); f = frame % tot
        for k in x['keys']:
            if f < k['frames']: return x, k
            f -= k['frames']
        return x, x['keys'][-1]

    def draw(self, canvas, items):
        tris = []
        for name, frame, dx, dy in items:
            x, k = self.key_at(name, frame)
            ox = x['pos'][0] + k['off'][0] + dx; oy = x['pos'][1] + k['off'][1] + dy; oz = x['pos'][2] + k['off'][2]
            for mi in self.node_meshes.get(k['node'], []):
                ti, idx = self.meshes[mi]
                for t in idx:
                    z = oz + self.pos[t, 2].mean()
                    tris.append((z, name, mi, ox, oy, ti, t))
        order = {n: i for i, (n, *_r) in enumerate(items)}
        tris.sort(key=lambda r: (-r[0], -order[r[1]]))
        H, W = canvas.shape[:2]
        for z, name, mi, ox, oy, ti, t in tris:
            if ti is None:  # material sem textura: sólido preto (HIPÓTESE, igual ao Rust)
                L.tri_flat(canvas, self.pos[t, :2] + [ox, oy], (0, 0, 0))
            else:
                L.tri(canvas, self.tex[ti], self.pos[t, :2] + [ox, oy], self.uv[t])

    @staticmethod
    def tri_flat(canvas, P, color):
        H, W = canvas.shape[:2]
        x0 = max(int(np.floor(P[:, 0].min())), 0); x1 = min(int(np.ceil(P[:, 0].max())), W - 1)
        y0 = max(int(np.floor(P[:, 1].min())), 0); y1 = min(int(np.ceil(P[:, 1].max())), H - 1)
        if x1 < x0 or y1 < y0: return
        (ax, ay), (bx, by), (cx, cy) = P
        den = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy)
        if abs(den) < 1e-9: return
        xs, ys = np.meshgrid(np.arange(x0, x1 + 1) + 0.5, np.arange(y0, y1 + 1) + 0.5)
        l1 = ((by - cy) * (xs - cx) + (cx - bx) * (ys - cy)) / den
        l2 = ((cy - ay) * (xs - cx) + (ax - cx) * (ys - cy)) / den
        l3 = 1 - l1 - l2; m = (l1 >= -1e-4) & (l2 >= -1e-4) & (l3 >= -1e-4)
        ys0, xs0 = np.nonzero(m)
        canvas[y0 + ys0, x0 + xs0, :3] = color

    @staticmethod
    def tri(canvas, tex, P, UV):
        H, W = canvas.shape[:2]; th, tw = tex.shape[:2]
        x0 = max(int(np.floor(P[:, 0].min())), 0); x1 = min(int(np.ceil(P[:, 0].max())), W - 1)
        y0 = max(int(np.floor(P[:, 1].min())), 0); y1 = min(int(np.ceil(P[:, 1].max())), H - 1)
        if x1 < x0 or y1 < y0: return
        (ax, ay), (bx, by), (cx, cy) = P
        den = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy)
        if abs(den) < 1e-9: return
        xs, ys = np.meshgrid(np.arange(x0, x1 + 1) + 0.5, np.arange(y0, y1 + 1) + 0.5)
        l1 = ((by - cy) * (xs - cx) + (cx - bx) * (ys - cy)) / den
        l2 = ((cy - ay) * (xs - cx) + (ax - cx) * (ys - cy)) / den
        l3 = 1 - l1 - l2; m = (l1 >= -1e-4) & (l2 >= -1e-4) & (l3 >= -1e-4)
        if not m.any(): return
        u = l1 * UV[0, 0] + l2 * UV[1, 0] + l3 * UV[2, 0]; v = l1 * UV[0, 1] + l2 * UV[1, 1] + l3 * UV[2, 1]
        # UV em texels, origem no canto superior esquerdo (conferido contra o Rust)
        ui = np.clip(u.astype(int), 0, tw - 1); vi = np.clip(v.astype(int), 0, th - 1)
        src = tex[vi, ui].astype(np.float32); a = src[..., 3:4] / 255.0
        reg = canvas[y0:y1 + 1, x0:x1 + 1].astype(np.float32)
        reg[..., :3] = reg[..., :3] * (1 - a * m[..., None]) + src[..., :3] * a * m[..., None]
        canvas[y0:y1 + 1, x0:x1 + 1, :3] = np.clip(reg[..., :3], 0, 255).astype(np.uint8)

if __name__ == '__main__':
    ap = argparse.ArgumentParser(); ap.add_argument('xa'); ap.add_argument('items', nargs='*')
    ap.add_argument('--size', default='400x240'); ap.add_argument('-o', default=None); ap.add_argument('--list', action='store_true')
    ap.add_argument('--bg', default='40,40,48')
    a = ap.parse_args(); L = Layout(a.xa)
    if a.list:
        for n, x in L.inst.items(): print(n, sum(k['frames'] for k in x['keys']))
        sys.exit()
    W, H = map(int, a.size.split('x')); canvas = np.zeros((H, W, 3), np.uint8); canvas[:] = [int(c) for c in a.bg.split(',')]
    items = []
    for s in a.items:
        nm, _, rest = s.partition('@'); parts = rest.split(',') if rest else ['0']
        items.append((nm, int(parts[0]), float(parts[1]) if len(parts) > 1 else 0, float(parts[2]) if len(parts) > 2 else 0))
    L.draw(canvas, items)
    out = a.o or (os.path.basename(a.xa).replace('.xa', '') + '.png')
    od = WORK / 'notes' / 'render'; od.mkdir(parents=True, exist_ok=True)
    Image.fromarray(canvas).save(od / out); print(od / out)
