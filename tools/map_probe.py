"""Carrega um mapa 3D do Yo-kai Watch 1 (data/map/<id>/<id>.xc + auxiliares), exporta OBJ + texturas + scene.json
e renderiza vistas de verificacao SEM janela (rasterizador numpy -> PNG).

Uso (de qualquer pasta):
    python map_probe.py t101i01            # tudo em work/map_t101i01/
    python map_probe.py t101i02 --no-render

Formatos decifrados aqui (ver work/notes/map-t101i01.md):
  * cfg.bin (T2B)         -> parse_cfg()  (mapenv / funcpt / funcdat / npcbin / map_config...)
  * PTREE/PTVAL(S)        -> ptree()      (mapenv e funcpt sao arvores de propriedades dentro do cfg.bin)
  * RES.bin (CHRC00)      -> parse_res()  (manifesto do .xc: hash->nome, ordem dos arquivos, materiais->textura)
  * .mbn (168 B)          -> parse_mbn()  (no de transformacao; so translacao e nao-identidade nestes mapas)
  * .prm (XMPR) estendido -> parse_prm_ext() (hashes de malha/material/no, AABB, nomes)
  * .xi                   -> imgc.decode  (ja existente)
Saidas so em work/ (gitignore).
"""
import sys, os, struct, zlib, json, csv, math, argparse
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import numpy as np
from paths import FA, ROOT, WORK
import xpck, l5, prm, imgc

# --------------------------------------------------------------------------------------------- FA
_rows = None
def fa_get(path):
    global _rows
    if _rows is None:
        _rows = {r[0]: (int(r[1]), int(r[2])) for r in csv.reader(open(ROOT / 'data' / 'romfs' / 'yw1_a_files.csv')) if r[0] != 'path'}
    if path not in _rows: return None
    o, s = _rows[path]
    with open(FA, 'rb') as f:
        f.seek(o); return f.read(s)

# --------------------------------------------------------------------------------------------- cfg.bin (T2B)
def _al(x, a): return (x + a - 1) // a * a

def parse_cfg(d):
    """Formato 'cfg.bin' da Level-5 (rodape 01 't2b' fe).  Devolve lista [(nome_chave, [params])].
    0x00: u32 nEntradas, u32 offStrings (= fim das entradas), u32 tamStrings, u32 nStrings; entradas a partir de 0x10.
    entrada: u32 crc32(nome) | u8 nParams | ceil(n/4) bytes de tipos (2 bits/param, LSB primeiro) | pad 0xFF ate multiplo de 4
             (contado desde o crc) | n * 4 bytes.  tipo 0 = offset de string (rel. a offStrings), 1 = i32, 2 = f32.
    Depois das strings (alinhado a 16): tabela de nomes de chave: u32 tam, u32 n, u32 offNomes, u32 tamNomes; n x (crc32, off).
    """
    n, so, sl, sc = struct.unpack_from('<4I', d, 0)
    strs = d[so:so + sl]
    kt = _al(so + sl, 16)
    ksz, nk, kso, ksl = struct.unpack_from('<4I', d, kt)
    names = {}
    for i in range(nk):
        crc, off = struct.unpack_from('<II', d, kt + 16 + 8 * i)
        s = kt + kso + off
        names[crc] = d[s:d.index(0, s)].decode('ascii', 'replace')
    def cs(o):
        if o == 0xFFFFFFFF: return None
        return strs[o:strs.index(0, o)].decode('shift-jis', 'replace')
    out = []; p = 16
    for _ in range(n):
        crc, np_ = struct.unpack_from('<IB', d, p)
        nt = (np_ + 3) // 4
        tb = d[p + 5:p + 5 + nt]
        p += _al(5 + nt, 4)
        ps = []
        for i in range(np_):
            t = (tb[i // 4] >> (2 * (i % 4))) & 3
            raw = d[p:p + 4]; p += 4
            if t == 0: ps.append(cs(struct.unpack('<I', raw)[0]))
            elif t == 1: ps.append(struct.unpack('<i', raw)[0])
            elif t == 2: ps.append(round(struct.unpack('<f', raw)[0], 6))
            else: ps.append('?t%d:%s' % (t, raw.hex()))
        out.append((names.get(crc, '%08x' % crc), ps))
    return out

def ptree(entries):
    """PTREE [nome, ...] abre no; _PTREE fecha; PTVAL [valor, nome] = atributo nomeado; PTVALS [v...] = lista de valores do no.
    Devolve dict aninhado: no -> {'_args':[...], '_vals':[...], filhos..., 'nome-de-PTVAL': valor}."""
    root = {'_args': [], '_kids': []}; st = [root]
    for k, ps in entries:
        cur = st[-1]
        if k == 'PTREE':
            node = {'_args': ps[1:], '_kids': [], '_name': ps[0]}
            cur['_kids'].append(node)
            cur.setdefault(ps[0], node)   # primeira ocorrencia por nome; todas em _kids
            st.append(node)
        elif k == '_PTREE': st.pop()
        elif k == 'PTVAL':
            cur[ps[1] if len(ps) > 1 else '_v'] = ps[0]
        elif k == 'PTVALS': cur['_vals'] = ps
        else: cur.setdefault('_other', []).append((k, ps))
    return root

def tree_json(n):
    if not isinstance(n, dict): return n
    o = {}
    for k, v in n.items():
        if k in ('_kids',): continue
        if isinstance(v, dict) and '_name' in v: continue   # filhos saem por _kids
        o[k] = v
    if n.get('_kids'): o['_kids'] = [dict(name=c['_name'], **tree_json(c)) for c in n['_kids']]
    return o

# --------------------------------------------------------------------------------------------- RES.bin (CHRC00)
def parse_res(buf):
    """Manifesto do .xc de mapa/personagem. Header: 'CHRC00\\0\\0' ; u16[0] = offset da tabela de strings /4 ; u16[1]=1 ;
    u16[2],[3] = (offset/4, n) do diretorio A ; u16[4],[5] = (offset/4, n) do diretorio B.
    Cada entrada de diretorio (8 B): u16 off/4, u16 n, u16 tag, u16 tamanho_da_entrada.
      Dir A: nomes de material (hash,stroff) | nomes de material (copia) | texturas (20 B) | registros de material (224 B) | fim(9999)
      Dir B: malhas (hash,0) | nos mbn (hash,0) | txp (hash,0) | nomes de atributo | .. | fim
    Todo hash = crc32(string ASCII).  A ordem de cada tabela == numeracao dos arquivos (NNN.prm, NNN.mbn, NNN.txp, NNN.mtr/.atr/NNN.xi).
    """
    assert buf[:6] == b'CHRC00'
    u = struct.unpack_from('<6H', buf, 8)
    S = u[0] * 4
    strs = {}
    p = S
    while p < len(buf):
        e = buf.index(0, p); strs[p - S] = buf[p:e].decode('latin1'); p = e + 1
    def name(o): return strs.get(o)
    tabs = []
    for (off, n) in ((u[2], u[3]), (u[4], u[5])):
        for i in range(n):
            a, c, tag, esz = struct.unpack_from('<4H', buf, off * 4 + 8 * i)
            tabs.append(dict(off=a * 4, n=c, tag=tag, esz=esz))
    R = dict(strings=strs, tabs=tabs, S=S)
    def hn(t):   # tabela (hash, stroff)
        return [struct.unpack_from('<II', buf, t['off'] + 8 * i) for i in range(t['n'])]
    mats1, mats2, texs, matrecs = tabs[0], tabs[1], tabs[2], tabs[3]
    R['materials'] = [dict(hash=h, name=name(o)) for h, o in hn(mats1)]
    R['textures'] = []
    for i in range(texs['n']):
        h, o, flags = struct.unpack_from('<IIH', buf, texs['off'] + texs['esz'] * i)
        R['textures'].append(dict(hash=h, name=name(o), flags=flags))
    R['matrec'] = []
    for i in range(matrecs['n']):
        b = matrecs['off'] + matrecs['esz'] * i
        h, so, h2, h3, th, one = struct.unpack_from('<6I', buf, b)
        R['matrec'].append(dict(hash=h, name=name(so), tex_hash=th, one=one, raw=buf[b:b + matrecs['esz']]))
    tab_h = lambda t: [struct.unpack_from('<I', buf, t['off'] + 8 * i)[0] for i in range(t['n'])]
    # diretorio B comeca em tabs[5] (apos a entrada de fim do A)
    B = tabs[5:]
    R['mesh_hashes'] = tab_h(B[0]); R['mbn_hashes'] = tab_h(B[1]); R['txp_hashes'] = tab_h(B[2])
    R['attr_names'] = [name(struct.unpack_from('<II', buf, B[3]['off'] + 8 * i)[1]) for i in range(B[3]['n'])]
    R['byhash'] = {zlib.crc32(s.encode('latin1')): s for s in strs.values()}
    return R

# --------------------------------------------------------------------------------------------- mbn / prm
def parse_mbn(d):
    """168 B = 42 u32. w0 = crc32(nome do no), w1 = hash do pai (0 = raiz), w3..w5 = translacao (x,y,z) f32, repetida em w39..w41.
    Todo o resto (w6..w38) e identidade neste mapa (quaternios (0,0,0,1)? e escala 1) -> HIPOTESE: rotacao/escala locais."""
    w = struct.unpack('<42I', d); f = struct.unpack('<42f', d)
    return dict(hash=w[0], parent=w[1], t=np.array(f[3:6], np.float32), t2=np.array(f[39:42], np.float32), raw=w)

def parse_prm_ext(d):
    """XMPR header (0x40 B): [1]=0x40 off XPRM, [2]=tam XPRM, [3]=off INFO, depois 6 pares (off,tam): os dois ultimos sao
    strings: nome da malha e nome do material (ASCIIZ).  INFO (0x3C B): u32 h_malha=crc32(nome), u32 h_material, u32 h_?(=0x1567a3ec ou 0x547e69f1: constante por grupo),
    u32 h_no_mbn, u32 n_prim?(info[4]), u32 0, f32 centro xyz, f32 meia-extensao xyz (AABB), u32 flags (255/5/..), u32 1, u32 0."""
    h = struct.unpack_from('<16I', d, 0)
    a = h[3]
    info = struct.unpack_from('<15I', d, a); fl = struct.unpack_from('<15f', d, a)
    s1 = d[h[12]:h[12] + h[13]].split(b'\0')[0].decode(); s2 = d[h[14]:h[14] + h[15]].split(b'\0')[0].decode()
    return dict(mesh_hash=info[0], mat_hash=info[1], aux_hash=info[2], node_hash=info[3], info4=info[4],
                center=np.array(fl[6:9]), half=np.array(fl[9:12]), flags=info[12], mesh_name=s1, mat_name=s2)

# --------------------------------------------------------------------------------------------- carga do mapa
def load_map(mid):
    base = 'data/map/%s/' % mid
    xc = fa_get(base + mid + '.xc')
    ents = dict(xpck.read(xc))
    res = parse_res(l5.l5dec(ents['RES.bin']))
    M = dict(id=mid, ents=ents, res=res)
    M['mbn'] = {}
    for i, h in enumerate(res['mbn_hashes']):
        m = parse_mbn(ents['%03d.mbn' % i]); assert m['hash'] == h, (i, hex(m['hash']), hex(h))
        M['mbn'][h] = m
    def world(h):
        t = np.zeros(3, np.float32); seen = 0
        while h and h in M['mbn'] and seen < 32:
            t = t + M['mbn'][h]['t']; h = M['mbn'][h]['parent']; seen += 1
        return t
    M['world'] = world
    M['prms'] = []
    for i, h in enumerate(res['mesh_hashes']):
        d = ents['%03d.prm' % i]
        e = parse_prm_ext(d); assert e['mesh_hash'] == h
        r = prm.parse(d)
        uv = r['vertices'].get(4)
        if uv is None: uv = np.zeros((len(r['vertices'][0]), 2), np.float32)
        e.update(index=i, pos=r['vertices'][0].astype(np.float32), color=r['vertices'].get(1), uv=uv, has_uv=4 in r['vertices'], idx=r['indices'],
                 attrs=r['attrs'], fixed=r['fixed'])
        mat_i = next(k for k, m in enumerate(res['materials']) if m['hash'] == e['mat_hash'])
        e['mat_index'] = mat_i
        e['tex_hash'] = res['matrec'][mat_i]['tex_hash']
        e['tex_index'] = next((k for k, t in enumerate(res['textures']) if t['hash'] == e['tex_hash']), None)
        e['node_world'] = world(e['node_hash'])
        M['prms'].append(e)
    M['aux'] = {}
    for suf in ('_mapenv.bin', '_funcpt.bin'):
        b = fa_get(base + mid + suf)
        if b: M['aux'][suf] = ptree(parse_cfg(b))
    b = fa_get('data/res/map/%s/%s_funcdat.cfg.bin' % (mid, mid))
    if b: M['aux']['funcdat'] = parse_cfg(b)
    return M

def load_textures(M):
    out = {}
    for i, t in enumerate(M['res']['textures']):
        img, H = imgc.decode(M['ents']['%03d.xi' % i])
        out[i] = img
    return out

# --------------------------------------------------------------------------------------------- export
def tris_of(e):
    return np.array(prm.strip_to_tris(e['idx']), np.int32).reshape(-1, 3)

def export_obj(M, outdir, sel, fname):
    lines = []; vo = 1; mtl = set()
    for e in sel:
        P = e['pos'] + e['node_world']
        lines.append('o %s' % e['mesh_name'])
        lines.append('usemtl %s' % e['mat_name'])
        for p in P: lines.append('v %.5f %.5f %.5f' % tuple(p))
        for u in e['uv']: lines.append('vt %.5f %.5f' % (u[0], u[1]))   # v NAO invertido: ver notas (PICA v=0 embaixo)
        for a, b, c in tris_of(e): lines.append('f %d/%d %d/%d %d/%d' % (a + vo, a + vo, b + vo, b + vo, c + vo, c + vo))
        vo += len(P); mtl.add((e['mat_name'], e['tex_index']))
    (outdir / (fname + '.obj')).write_text('mtllib %s.mtl\n' % fname + '\n'.join(lines))
    (outdir / (fname + '.mtl')).write_text(''.join('newmtl %s\nKd 1 1 1\nmap_Kd tex/%03d.png\n' % (n, i if i is not None else 0) for n, i in sorted(mtl)))

# --------------------------------------------------------------------------------------------- rasterizador de verificacao
def look_at(eye, tgt, up=(0, 1, 0)):
    eye = np.array(eye, float); tgt = np.array(tgt, float)
    f = tgt - eye; f /= np.linalg.norm(f)
    r = np.cross(f, up); r /= np.linalg.norm(r)
    u = np.cross(r, f)
    return eye, r, u, f

def render(M, texs, sel, eye, tgt, W=640, H=480, fov=50, ortho=None, vcolor=True, bg=(30, 30, 40), cull=False, flat=None):
    eye, r, u, f = look_at(eye, tgt)
    img = np.zeros((H, W, 3), np.float32); img[:] = bg
    zb = np.full((H, W), 1e30, np.float32)
    asp = W / H; th = math.tan(math.radians(fov) / 2)
    for e in sel:
        P = (e['pos'] + e['node_world']).astype(np.float64)
        rel = P - eye
        x = rel @ r; y = rel @ u; z = rel @ f
        if ortho:
            sx = (x / (ortho * asp) * 0.5 + 0.5) * W; sy = (0.5 - y / ortho * 0.5) * H; zz = z
        else:
            zz = np.maximum(z, 1e-3)
            sx = (x / (zz * th * asp) * 0.5 + 0.5) * W; sy = (0.5 - y / (zz * th) * 0.5) * H
        T = tris_of(e)
        tex = texs.get(e['tex_index']) if e['tex_index'] is not None else None
        col = e['color'][:, :3] if (vcolor and e['color'] is not None) else np.ones((len(P), 3), np.float32)
        if flat is not None: col = np.ones_like(col) * flat
        uv = e['uv']
        for a, b, c in T:
            if z[a] <= 0.01 or z[b] <= 0.01 or z[c] <= 0.01: continue
            x0, y0, x1, y1, x2, y2 = sx[a], sy[a], sx[b], sy[b], sx[c], sy[c]
            den = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2)
            if abs(den) < 1e-9: continue
            if cull and den > 0: continue
            xmin = max(int(math.floor(min(x0, x1, x2))), 0); xmax = min(int(math.ceil(max(x0, x1, x2))), W - 1)
            ymin = max(int(math.floor(min(y0, y1, y2))), 0); ymax = min(int(math.ceil(max(y0, y1, y2))), H - 1)
            if xmin > xmax or ymin > ymax: continue
            gx, gy = np.meshgrid(np.arange(xmin, xmax + 1) + 0.5, np.arange(ymin, ymax + 1) + 0.5)
            l0 = ((y1 - y2) * (gx - x2) + (x2 - x1) * (gy - y2)) / den
            l1 = ((y2 - y0) * (gx - x2) + (x0 - x2) * (gy - y2)) / den
            l2 = 1 - l0 - l1
            m = (l0 >= -1e-4) & (l1 >= -1e-4) & (l2 >= -1e-4)
            if not m.any(): continue
            d = l0 * zz[a] + l1 * zz[b] + l2 * zz[c]
            sub = zb[ymin:ymax + 1, xmin:xmax + 1]
            m &= d < sub
            if not m.any(): continue
            c3 = (l0[..., None] * col[a] + l1[..., None] * col[b] + l2[..., None] * col[c])
            if tex is not None:
                uu = l0 * uv[a, 0] + l1 * uv[b, 0] + l2 * uv[c, 0]
                vv = l0 * uv[a, 1] + l1 * uv[b, 1] + l2 * uv[c, 1]
                th_, tw_ = tex.shape[:2]
                tx = np.clip((uu % 1.0) * tw_, 0, tw_ - 1).astype(int)
                ty = np.clip(((1 - vv) % 1.0) * th_, 0, th_ - 1).astype(int)
                tc = tex[ty, tx].astype(np.float32) / 255.0
                alpha = tc[..., 3]
                m &= alpha > 0.4
                c3 = c3 * tc[..., :3]
            sub[m] = d[m]
            img[ymin:ymax + 1, xmin:xmax + 1][m] = np.clip(c3[m], 0, 1) * 255
    return img.astype(np.uint8)

# --------------------------------------------------------------------------------------------- main
def groups(M):
    """Agrupa as malhas pelo prefixo do nome: base (sem prefixo), jmden23, jn1, cl (colisao/sombra)."""
    g = {}
    for e in M['prms']:
        n = e['mesh_name']
        if '.jmden23-' in n: k = 'jmden23'
        elif '.jn1-' in n: k = 'jn1'
        elif n.startswith(M['id'] + 'cl.'): k = 'cl'
        else: k = 'base'
        g.setdefault(k, []).append(e)
    return g

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('map_id', nargs='?', default='t101i01')
    ap.add_argument('--no-render', action='store_true')
    a = ap.parse_args()
    from PIL import Image
    mid = a.map_id
    M = load_map(mid)
    out = WORK / ('map_' + mid); (out / 'tex').mkdir(parents=True, exist_ok=True)
    texs = load_textures(M)
    for i, img in texs.items():
        Image.fromarray(img).save(out / 'tex' / ('%03d.png' % i))
    G = groups(M)
    for k, sel in G.items(): export_obj(M, out, sel, '%s_%s' % (mid, k))
    # scene.json
    sc = dict(id=mid, textures=[dict(index=i, name=t['name'], hash=t['hash'], flags=t['flags'], w=int(texs[i].shape[1]), h=int(texs[i].shape[0]))
                                for i, t in enumerate(M['res']['textures'])],
              meshes=[dict(index=e['index'], name=e['mesh_name'], material=e['mat_name'], mat_index=e['mat_index'], tex_index=e['tex_index'],
                           node=hex(e['node_hash']), node_world=[float(x) for x in e['node_world']],
                           aabb_center=[float(x) for x in e['center']], aabb_half=[float(x) for x in e['half']],
                           nverts=int(len(e['pos'])), ntris=int(len(tris_of(e))), group=next(k for k, s in G.items() if e in s)) for e in M['prms']],
              nodes=[dict(hash=hex(h), parent=hex(n['parent']), t=[float(x) for x in n['t']]) for h, n in M['mbn'].items()],
              attr_names=M['res']['attr_names'])
    sc['aux'] = {k: (tree_json(v) if isinstance(v, dict) else v) for k, v in M['aux'].items()}
    (out / 'scene.json').write_text(json.dumps(sc, indent=1, ensure_ascii=False))
    allp = np.concatenate([e['pos'] + e['node_world'] for e in M['prms'] if e['mesh_name'].find('cl') < 0])
    print('map', mid, 'meshes', len(M['prms']), 'texturas', len(texs), 'AABB', allp.min(0).round(1), allp.max(0).round(1))
    for k, sel in G.items(): print(' grupo', k, len(sel), 'malhas,', sum(len(e['pos']) for e in sel), 'verts')
    if a.no_render: return
    lo, hi = allp.min(0), allp.max(0); c = (lo + hi) / 2; ext = hi - lo
    for k in ('base', 'jmden23', 'jn1'):
        if k not in G: continue
        sel = [e for e in G[k]]
        v1 = render(M, texs, sel, (c[0], hi[1] + 100, c[2] + 1e-3), (c[0], 0, c[2]), ortho=max(ext[0], ext[2]) * 1.05, W=640, H=480, fov=40)
        Image.fromarray(v1).save(out / ('render_%s_top.png' % k))
        v2 = render(M, texs, sel, (c[0] + ext[0] * 0.9, hi[1] + ext[1] * 0.9, c[2] + ext[2] * 1.0), (c[0], c[1] - 3, c[2]), fov=45)
        Image.fromarray(v2).save(out / ('render_%s_iso.png' % k))
    print('renders em', out)

if __name__ == '__main__':
    main()
