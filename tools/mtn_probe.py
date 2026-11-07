"""Sonda de animação Level-5 (XMTN `.mtn2` + clipes `MINF` `.mtninf`) e skinning de personagem — protótipo de
validação do que vai para o Rust (`engine/src/character.rs`).

XMTN (referência: Ploaj/Metanoia `Level5_MTN2.cs`, conferido aqui nos dados reais):
  0x08 u32 tamanho descomprimido · 0x0c u32 offset do nome (u32 hash + ASCIIZ, ex. 'out_00') · 0x10 u32 offset dos dados L5
  0x14 i32 nº de faixas de posição · 0x18 rotação · 0x1c escala · 0x20 ? · 0x24 nº de ossos · 0x54 i32 nº de quadros
  dados: u32 off. hashes de osso, u32 off. info de faixa, u32 off. tabela; info: 4 × u16 → (tipo, tipo de dado, ?, nº de
  componentes, início, fim). Tabela: por faixa 16 B (off. flags, off. quadros, off. valores, ?); flags: i16 osso,
  u16 (bits 0-11 nº de chaves, 12-15 flag). Quadros i16; valores: tipo 1 = i16/32767, 2 = f32.
MINF (96 B): 0x1c hash do nome, 0x20 nome Shift-JIS, 0x44 hash do movimento, 0x4c u32 quadro inicial, 0x50 final, 0x54 f32 velocidade (0.5).

Uso: python mtn_probe.py [nome do clipe]   → work/chara/ (PNG de um quadro do Nate animado)
"""
import sys, os, struct, math
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import yw_fa, xpck, l5, prm, imgc
from skel_probe import load_skeleton
from paths import WORK


def parse_mtn(m):
    assert m[:4] == b'XMTN'
    npos, nrot, nscl, _, nbones = struct.unpack_from('<5i', m, 0x14)
    frames = struct.unpack_from('<i', m, 0x54)[0]
    d = l5.l5dec(m[struct.unpack_from('<I', m, 0x10)[0]:])
    bh, ti, do = struct.unpack_from('<3I', d, 0)
    hashes = list(struct.unpack_from('<%dI' % ((ti - bh) // 4), d, bh))
    tracks = []
    for i in range(4):
        o = struct.unpack_from('<H', d, ti + 2 * i)[0]
        tracks.append(struct.unpack_from('<BBBBHH', d, o))
    out = {}   # (kind, bone hash) -> (frames[], values[N, comp])
    idx = 0
    for kind, count in ((1, npos), (2, nrot), (3, nscl)):
        typ, dtype, _, comp, _, _ = tracks[kind - 1]
        for i in range(idx, idx + count):
            fo, ko, vo = struct.unpack_from('<3I', d, do + 16 * i)
            bone, cf = struct.unpack_from('<hH', d, fo)
            nk, flag = cf & 0xFFF, cf >> 12   # u16: 12 bits contagem + 4 bits flag (o Metanoia lê só o byte baixo)
            fr = struct.unpack_from('<%dh' % nk, d, ko)
            if dtype == 1:
                vals = np.array(struct.unpack_from('<%dh' % (nk * comp), d, vo), np.float64) / 32767.0
            elif dtype == 2:
                vals = np.array(struct.unpack_from('<%df' % (nk * comp), d, vo), np.float64)
            else:
                vals = np.array(struct.unpack_from('<%dh' % (nk * comp), d, vo), np.float64)
            out[(kind, hashes[bone])] = (np.array(fr), vals.reshape(nk, comp), flag)
        idx += count
    return dict(frames=frames, hashes=hashes, tracks=tracks, keys=out)


def parse_minf(b):
    h = struct.unpack_from('<I', b, 0x1c)[0]
    raw = b[0x20:0x48].split(b'\0')[0]
    start, end = struct.unpack_from('<II', b, 0x4c)
    speed = struct.unpack_from('<f', b, 0x54)[0]
    return dict(hash=h, name=raw.decode('shift-jis', 'replace'), start=start, end=end, speed=speed)


def sample(keys, f):
    fr, vals, _ = keys
    if f <= fr[0]: return vals[0]
    if f >= fr[-1]: return vals[-1]
    k = np.searchsorted(fr, f) - 1
    t = (f - fr[k]) / max(1, fr[k + 1] - fr[k])
    a, b = vals[k], vals[k + 1]
    if len(a) == 4:  # quatérnion: nlerp com o hemisfério certo
        if np.dot(a, b) < 0: b = -b
        q = a * (1 - t) + b * t
        return q / np.linalg.norm(q)
    return a * (1 - t) + b * t


def quat_to_mat_row(q):
    """Quatérnion (x,y,z,w) → matriz 3×3 na convenção LINHA (v' = v·M), igual ao .mbn."""
    x, y, z, w = q
    m = np.array([[1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
                  [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
                  [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)]])
    return m.T


def pose(bones, mtn, frame):
    """Pose de mundo (R linha, P) de cada osso no quadro; osso sem faixa usa o local do .mbn."""
    W = []
    for i, b in enumerate(bones):
        h = b['hash']
        t = sample(mtn['keys'][(1, h)], frame) if (1, h) in mtn['keys'] else b['t']
        R = quat_to_mat_row(sample(mtn['keys'][(2, h)], frame)) if (2, h) in mtn['keys'] else b['A']
        s = sample(mtn['keys'][(3, h)], frame) if (3, h) in mtn['keys'] else b['s']
        R = np.diag(s) @ R
        if b['pi'] < 0:
            W.append((R, np.array(t)))
        else:
            PR, PP = W[b['pi']]
            W.append((R @ PR, np.array(t) @ PR + PP))
    return W


def main():
    clip_name = sys.argv[1] if len(sys.argv) > 1 else '歩き1L'
    body = dict(xpck.read(yw_fa.get('data/character/c001000/c001000_p00.xc')))
    mot = dict(xpck.read(yw_fa.get('data/character/c000010/c000010_p10.xc')))
    bones = load_skeleton(body)
    mtn = parse_mtn(mot['000.mtn2'])
    clips = [parse_minf(v) for k, v in sorted(mot.items()) if k.endswith('.mtninf')]
    bh = {b['hash'] for b in bones}
    print('ossos modelo', len(bones), '| ossos animados', len(mtn['hashes']), '| em comum', len(bh & set(mtn['hashes'])))
    print('clipes:', [(c['name'], c['start'], c['end']) for c in clips][:60])
    clip = next((c for c in clips if c['name'] == clip_name), clips[1])
    bind = pose(bones, {'keys': {}}, 0)
    # matrizes de skin: v_mundo = (v_bind - Pb)·Rb^-1 · Ra + Pa
    os.makedirs(WORK / 'chara', exist_ok=True)
    from PIL import Image
    import map_probe as mp
    tex = imgc.decode(body['000.xi'])[0]
    imgs = []
    for f in np.linspace(clip['start'], clip['end'], 4):
        cur = pose(bones, mtn, f)
        idx_of = {b['hash']: i for i, b in enumerate(bones)}
        meshes = []
        for name in sorted(k for k in body if k.endswith('.prm')):
            d = body[name]
            r = prm.parse(d)
            # tabela de nós: header 0x28 (offset, tamanho) com hashes de osso
            no, ns = struct.unpack_from('<II', d, 0x28)
            table = list(struct.unpack_from('<%dI' % (ns // 4), d, no)) if ns else []
            P = r['vertices'][0][:, :3].astype(np.float64)
            W8 = r['vertices'].get(7); I8 = r['vertices'].get(8)
            out = np.zeros_like(P)
            for v in range(len(P)):
                acc = np.zeros(3); ws = 0
                for k in range(4):
                    w = W8[v][k] if W8 is not None else (1.0 if k == 0 else 0.0)
                    if w <= 0: continue
                    bi = idx_of.get(table[int(I8[v][k])], 0) if (I8 is not None and table) else 0
                    Rb, Pb = bind[bi]; Ra, Pa = cur[bi]
                    acc += w * (((P[v] - Pb) @ np.linalg.inv(Rb)) @ Ra + Pa); ws += w
                out[v] = acc / ws if ws else P[v]
            uv = r['vertices'].get(4)
            if uv is not None and os.environ.get('UVFLIP'):
                uv = np.stack([uv[:, 0], 1 - uv[:, 1]], 1)
            meshes.append(dict(pos=out.astype(np.float32), color=None, uv=uv if uv is not None else np.zeros((len(P), 2)),
                               idx=r['indices'], node_world=np.zeros(3), tex_index=0, mesh_name=name, has_uv=True))
        M = {'prms': meshes}
        img = mp.render(M, {0: tex}, meshes, (0, 9, 30), (0, 5, 0), W=240, H=320, fov=40, vcolor=False)
        imgs.append(img)
    Image.fromarray(np.concatenate(imgs, 1)).save(WORK / 'chara' / 'nate_anim.png')
    print('clipe', clip, '→ work/chara/nate_anim.png')


if __name__ == '__main__':
    main()
