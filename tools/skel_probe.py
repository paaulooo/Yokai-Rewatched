"""Sonda do esqueleto Level-5 (.mbn, 168 B/osso). Valida: pose local -> mundo == matriz mundo gravada e inversa de bind.
Layout .mbn (u32/f32 little-endian, 42 palavras):
  0x00 u32 hash (CRC-32 do nome do osso)   0x04 u32 hash do pai (0 = raiz)   0x08 i32 flags (sempre 4 aqui)
  0x0c f32[3] translacao local   0x18 f32[9] rotacao local A (convencao LINHA: v' = v*A)   0x3c f32[3] escala local
  0x48 f32[9] B = inversa de bind (rot)   0x6c f32[3] C = inversa de bind (transl)
  0x78 f32[9] D = rotacao de mundo (bind)   0x9c f32[3] E = posicao de mundo (bind)
Uso: python skel_probe.py [caminho/no/fa/xx_p00.xc]   (padrao: Nate = data/character/c001000/c001000_p00.xc)
"""
import sys, os, struct
import numpy as np
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import yw_fa, xpck

def parse_mbn(b):
    h, p, fl = struct.unpack_from('<IIi', b, 0)
    f = np.array(struct.unpack_from('<39f', b, 12), dtype=np.float64)
    return dict(hash=h, parent=p, flags=fl, t=f[0:3], A=f[3:12].reshape(3,3), s=f[12:15],
                B=f[15:24].reshape(3,3), C=f[24:27], D=f[27:36].reshape(3,3), E=f[36:39])

def load_skeleton(es):
    """es: dict nome->bytes de um XPCK. Devolve lista de ossos na ordem do indice NNN.mbn."""
    names = sorted(n for n in es if n.endswith('.mbn'))
    bones = [parse_mbn(es[n]) for n in names]
    idx = {b['hash']: i for i, b in enumerate(bones)}
    for b in bones: b['pi'] = idx.get(b['parent'], -1)
    return bones

def world(bones):
    """Compoe a pose local (T,A) -> (Rw, Pw) com a convencao linha. Pais sempre antes dos filhos? (nao garantido: resolve recursivo)."""
    W = [None]*len(bones)
    def get(i):
        if W[i] is None:
            b = bones[i]
            if b['pi'] < 0: W[i] = (b['A'].copy(), b['t'].copy())
            else:
                R, P = get(b['pi'])
                W[i] = (b['A'] @ R, b['t'] @ R + P)
        return W[i]
    for i in range(len(bones)): get(i)
    return W

if __name__ == '__main__':
    path = sys.argv[1] if len(sys.argv) > 1 else 'data/character/c001000/c001000_p00.xc'
    es = dict(xpck.read(yw_fa.get(path)))
    bones = load_skeleton(es)
    W = world(bones)
    er_r = er_p = er_inv = 0
    for b, (R, P) in zip(bones, W):
        er_r = max(er_r, np.abs(R - b['D']).max()); er_p = max(er_p, np.abs(P - b['E']).max())
        # inversa de bind: p_bone = p_world*B + C  deve mapear E -> 0 e D -> I
        er_inv = max(er_inv, np.abs(b['E'] @ b['B'] + b['C']).max(), np.abs(b['D'] @ b['B'] - np.eye(3)).max())
    print(path, 'ossos:', len(bones))
    print('erro max rot mundo %.2e | pos mundo %.2e | inversa bind %.2e' % (er_r, er_p, er_inv))
    print('raizes:', [i for i, b in enumerate(bones) if b['pi'] < 0], '| pais < filho em todos:', all(b['pi'] < i for i, b in enumerate(bones)))
