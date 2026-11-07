"""Probe de layouts de menu (.xa = XPCK com RES.bin ANMC00). Lista texturas (tamanho/formato), nos,
materiais, instancias (com nº de keyframes e duracao total) de qualquer data/menu/*.xa do yw1_a.fa.

Uso: python menu_probe.py data/menu/mm_top_d00_en.xa [--keys] [--json]
     python menu_probe.py --all-mm      (resumo de todos os mm_* / title_* / etc em work/notes/menu_probe_all.txt)
Validado: contra title_{u,d}00_en.xa (soma de keyframes = total da secao 14, igual ao anmc.rs).
"""
import struct, sys, os, zlib, csv, io
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import FA, ROOT, WORK
import l5, xpck

_rows = None
def fa_read(path):
    global _rows
    if _rows is None:
        _rows = {r['path']: r for r in csv.DictReader(open(ROOT / 'data' / 'romfs' / 'yw1_a_files.csv'))}
    r = _rows[path]
    with open(FA, 'rb') as f:
        f.seek(int(r['offset_in_romfs_fa'])); return f.read(int(r['size']))

def parse_anmc(d):
    assert d[:6] == b'ANMC00', d[:8]
    W = lambda w: struct.unpack_from('<I', d, w * 4)[0]
    F = lambda w: struct.unpack_from('<f', d, w * 4)[0]
    secs = [(W(w) & 0xffff, W(w) >> 16) for w in range(5, 0x19)]   # (off palavras, n)
    st = secs[18][0] * 4
    S = d[st:]
    tab = {zlib.crc32(s): s.decode('shift-jis', 'replace') for s in S.split(b'\0') if s}
    def nm(o): return S[o:S.index(b'\0', o)].decode('shift-jis', 'replace')
    out = dict(secs=secs, tab=tab, strings=[s.decode('shift-jis','replace') for s in S.split(b'\0') if s])
    # texturas / materiais / malhas / nos
    o, n = secs[0]; out['tex'] = [W(o + i * secs[1][1] // 4) for i in range(n)]
    o, n = secs[2]; es = secs[3][1] // 4
    out['mats'] = [dict(h=W(o + i * es), tex=W(o + i * es + 4), sx=F(o + i * es + 6), sy=F(o + i * es + 7)) for i in range(n)]
    o, n = secs[8]; es = secs[9][1] // 4
    out['meshes'] = [dict(h=W(o + i * es), mat=W(o + i * es + 3)) for i in range(n)]
    o, n = secs[12]; es = secs[13][1] // 4
    out['nodes'] = [dict(h=W(o + i * es), name=nm(W(o + i * es + 1) & 0xffff), nmesh=W(o + i * es + 1) >> 16) for i in range(n)]
    ko, kn = secs[14]; ks = secs[15][1] // 4
    io_, inn = secs[16]; isz = secs[17][1] // 4
    k = ko; insts = []
    for i in range(inn):
        w = io_ + i * isz; h = W(w); v = W(w + 1); cnt = v >> 16
        keys = []
        for j in range(cnt):
            keys.append(dict(node=W(k), frames=W(k + 1), off=(F(k + 2), F(k + 3), F(k + 4)))); k += ks
        insts.append(dict(h=h, name=nm(v & 0xffff), f0=W(w + 2), pos=(F(w + 3), F(w + 4), F(w + 5)),
                          box=[F(w + 6 + j) for j in range(6)], keys=keys))
    out['insts'] = insts; out['nkeys_total'] = kn; out['keys_used'] = (k - ko) // ks
    return out

def load_xa(path):
    ents = xpck.read(fa_read(path))
    files = dict(ents)
    a = parse_anmc(l5.l5dec(files['RES.bin']))
    tex = []
    for n, dta in ents:
        if n.endswith('.xi'):
            tex.append((n, struct.unpack_from('<BBBBhhh', dta, 10) if False else (dta[10], dta[12], dta[13], struct.unpack_from('<h', dta, 16)[0], struct.unpack_from('<h', dta, 18)[0])))
    return ents, a, tex

def dump(path, keys=False):
    ents, a, tex = load_xa(path)
    print('== %s  (%d arquivos no XPCK: %s)' % (path, len(ents), ' '.join(n for n, _ in ents if not n.endswith(('.xi', '.pbi')))))
    print('  secoes (off,n):', a['secs'][0::2][:10])
    for (n, (fmt, mips, bd, w, h)), th in zip(tex, a['tex'] + [0] * 99):
        label = a['tab'].get(th, '') if th else 'SEM-TEX (material sem textura)'
        print('  tex %-8s %4dx%-4d fmt=%02x mips=%d  hash=%08x %s' % (n, w, h, fmt, mips, th, label))
    print('  nos: ' + ', '.join('%s(%d)' % (x['name'], x['nmesh']) for x in a['nodes']))
    for x in a['insts']:
        tot = sum(k['frames'] for k in x['keys'])
        nodes = sorted(set(a['tab'].get(k['node'], '%08x' % k['node']) for k in x['keys']))
        print('  INST %-22s keys=%-3d frames=%-4d pos=(%g,%g,%g) f0=%08x nodes=%s' % (
            x['name'], len(x['keys']), tot, *x['pos'], x['f0'], ','.join(nodes[:6]) + ('...' if len(nodes) > 6 else '')))
        if keys:
            print('      ', ' '.join('%s:%d' % (a['tab'].get(k['node'], '%08x' % k['node']), k['frames']) for k in x['keys']))

if __name__ == '__main__':
    if sys.argv[1] == '--all-mm':
        rows = [r['path'] for r in csv.DictReader(open(ROOT / 'data' / 'romfs' / 'yw1_a_files.csv'))
                if r['path'].startswith('data/menu/') and r['path'].endswith('.xa') and '/' not in r['path'][10:]]
        buf = io.StringIO(); so = sys.stdout; sys.stdout = buf
        for p in rows:
            try: dump(p)
            except Exception as e: print('!!', p, e)
        sys.stdout = so
        (WORK / 'notes').mkdir(exist_ok=True); (WORK / 'notes' / 'menu_probe_all.txt').write_text(buf.getvalue(), encoding='utf-8')
        print(len(rows), 'layouts ->', WORK / 'notes' / 'menu_probe_all.txt')
    else:
        dump(sys.argv[1], '--keys' in sys.argv)
