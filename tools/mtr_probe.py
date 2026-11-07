"""Sonda de recursos/materiais Level-5 (CHRC00 'RES.bin' + MTRC00 .mtr + .txp/.atr/.cmn).
Resolve a cadeia  malha(.prm) -> nome do material (string no .prm) -> CRC-32 -> registro de material (CHRC tipo 290)
-> hash da textura -> entrada de textura (tipo 240) -> NNN.xi  (indice = ordem na lista tipo 240).
Uso: python mtr_probe.py [caminho.xc ...]   (padrao: Nate, Whisper e o quarto t101i01)
"""
import sys, os, struct, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import yw_fa, xpck, l5

class Chrc:
    """RES.bin (L5 -> 'CHRC00').
    Cabecalho: 'CHRC00\0\0' | u16 strOff(palavras) | u16 1 | u16 dir1Off | u16 dir1Cnt | u16 dir2Off | u16 dir2Cnt
    dir1/dir2: registros de 8 B = u16 off(palavras) | u16 count | u16 tipo | u16 bytes_por_entrada (fim: tipo 9999)
    dir1 (tipos): 220 = (hash,0)x N lista de materiais; 230 = idem (atr); 240 = textura (20 B); 290 = material (224 B)
    dir2 (tipos): 100 prm (hash do nome de esqueleto/malha), 110 mbn (hash do osso, ordem = NNN.mbn), 120 lkat, 140 txp,
                  200 cmn (hash do nome da propriedade), 310 imminf, 410 imm2, 460 sil ...
    strings (Shift-JIS, '\0'): nomes cujos CRC-32 aparecem nas tabelas."""
    def __init__(self, raw):
        self.r = r = raw
        assert r[:6] == b'CHRC00'
        so, ver, d1o, d1c, d2o, d2c = struct.unpack_from('<6H', r, 8)
        self.strs = [s.decode('sjis', 'replace') for s in r[so*4:].split(b'\0') if s]
        self.names = {zlib.crc32(s.encode('sjis', 'replace')): s for s in self.strs}
        self.dir1 = [struct.unpack_from('<4H', r, d1o*4+8*i) for i in range(d1c)]
        self.dir2 = [struct.unpack_from('<4H', r, d2o*4+8*i) for i in range(d2c)]
    def section(self, ty):
        for off, cnt, t, el in self.dir1 + self.dir2:
            if t == ty: return off, cnt, el
        return None
    def hashes(self, ty):
        s = self.section(ty)
        if not s: return []
        off, cnt, el = s
        return [struct.unpack_from('<I', self.r, off*4+el*j)[0] for j in range(cnt)]
    def textures(self):      # [(hash, w1, w2)]  indice = NNN.xi
        s = self.section(240)
        if not s: return []
        off, cnt, el = s
        return [struct.unpack_from('<3I', self.r, off*4+el*j) for j in range(cnt)]
    def materials(self):     # hash do material -> dict(tex_hash, flag, words)
        s = self.section(290)
        out = {}
        if not s: return out
        off, cnt, el = s
        for j in range(cnt):
            w = struct.unpack_from('<56I', self.r, off*4+el*j)
            out[w[0]] = dict(index=j, ordinal=w[1], tex_hash=w[4], tex_flag=w[5], words=w)
        return out

def prm_strings(prm):
    """Nome do esqueleto/malha e nome do material do .prm (cabecalho XMPR, u32[12..15] = off,len,off,len)."""
    h = struct.unpack_from('<16I', prm, 0)
    sk = prm[h[12]:h[12]+h[13]].rstrip(b'\0').decode('sjis', 'replace')
    mt = prm[h[14]:h[14]+h[15]].rstrip(b'\0').decode('sjis', 'replace')
    return sk, mt

def prm_palette(prm):
    """Paleta de ossos: hashes (CRC-32 do nome do osso) em u32[10]=off, u32[11]=bytes. v8 do vertice indexa essa paleta."""
    h = struct.unpack_from('<16I', prm, 0)
    return [struct.unpack_from('<I', prm, h[10]+4*i)[0] for i in range(h[11]//4)]

def resolve(es):
    """es: dict nome->bytes (XPCK). Devolve lista por .prm: (nome_prm, esqueleto, material, indice_xi|None, nome_textura)."""
    ch = Chrc(l5.l5dec(es['RES.bin']))
    mats = ch.materials(); texs = ch.textures()
    thash = {t[0]: i for i, t in enumerate(texs)}
    res = []
    for n in sorted(es):
        if not n.endswith('.prm'): continue
        sk, mt = prm_strings(es[n])
        m = mats.get(zlib.crc32(mt.encode('sjis', 'replace')))
        xi = None
        if m and m['tex_hash'] in thash: xi = thash[m['tex_hash']]
        res.append((n, sk, mt, xi, ch.names.get(m['tex_hash']) if m else None, m is not None))
    return ch, res

if __name__ == '__main__':
    paths = sys.argv[1:] or ['data/character/c001000/c001000_p00.xc', 'data/character/y001000/y001000_p00.xc', 'data/map/t101i01/t101i01.xc']
    for p in paths:
        es = dict(xpck.read(yw_fa.get(p)))
        ch, res = resolve(es)
        print('==', p, '| texturas:', len(ch.textures()), '| materiais:', len(ch.materials()))
        for n, sk, mt, xi, tn, ok in res[:12]:
            print('  %s  %-28s %-45s xi=%s tex=%s %s' % (n, sk, mt, xi, tn, '' if ok else 'MATERIAL NAO ACHADO'))
