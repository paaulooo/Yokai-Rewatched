"""Converte as texturas IMGC que ficam DENTRO de contêineres para PNG em work/embedded_xi/.

O export_xi.py só cobre os .xi soltos do .fa; os .xi embutidos nos .xa (menus,
data/menu/) e nos .xc (mapas, data/map/<id>/) nunca foram exportados.

Uso:
    python export_embedded_xi.py                     # escopo padrão (menus + quartos t101i01..i04)
    python export_embedded_xi.py --map t101i02       # só um mapa
    python export_embedded_xi.py --xa data/menu/win_en.xa   # só um .xa

Escopo padrão: todos os .xa na raiz de data/menu/ (mm_*, title_*, chara_make_*, keyboard,
loading, win, rpg_common00, shop_*, result_*, battle_* …) e os .xc de todos os quartos
da casa t101i* (inclusive t101i04, quarto da menina). As texturas soltas do mapa
(<mapa>.xi e <mapa>n.xi — versão dia/noite) também saem, na raiz da pasta de cada mapa.
"""
import sys, os, json, struct, argparse, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import ROOT, WORK
import l5, xpck, imgc, yw_fa
from PIL import Image

OUT = WORK / 'embedded_xi'
ROOM_IDS = ('t101i01', 't101i02', 't101i03', 't101i04')


def header_info(buf):
    """(w, h, fmt, prof) do IMGC."""
    h = imgc.parse(buf)
    return dict(w=h['w'], h=h['h'], fmt='0x%02x' % h['fmt'], prof=h['bd'])


def save_png(buf, out, manifest, key, where):
    try:
        png, h = imgc.decode(buf)
        out.parent.mkdir(parents=True, exist_ok=True)
        Image.fromarray(png, 'RGBA').save(out)
        manifest.append(dict(id=key, container=where, size=[h['w'], h['h']],
                             fmt='0x%02x' % h['fmt'], bpp=h['bd'], nsub=h['cnt']))
        return 1, None
    except Exception as e:
        return 0, '%s: %s' % (key, str(e)[:60])


def extract_xa(path, manifest):
    buf = yw_fa.get(path)
    ents = xpck.read(buf)
    ok = 0; errs = []
    for name, data in ents:
        if not name.endswith('.xi'):
            continue
        base = OUT / path.rsplit('.', 1)[0]      # work/embedded_xi/data/menu/win_en
        png_out = base / name.replace('.xi', '.png')
        key = path + '/' + name
        if png_out.exists():                     # idempotente: já extraído
            continue
        a, e = save_png(data, png_out, manifest, key, path)
        if a: ok += 1
        elif e: errs.append(e)
    return len(ents), ok, errs


def extract_xc(path, manifest, loose=True):
    """.xc de mapa: XPCK com RES.bin, %03d.xi … + texturas soltas do mapa (<id>.xi, <id>n.xi)."""
    base_dir = path.rsplit('/', 1)[0]
    mid = path.rsplit('/', 1)[1].rsplit('.', 1)[0]
    ok = 0; errs = []
    for loose_path in sorted(p for p in yw_fa.index() if p.startswith(base_dir + '/') and p.endswith('.xi')):
        name = loose_path.rsplit('/', 1)[1]
        png_out = OUT / base_dir / (name[:-3] + '.png')
        if png_out.exists():
            continue
        a, e = save_png(yw_fa.get(loose_path), png_out, manifest, loose_path, loose_path)
        if a: ok += 1
        elif e: errs.append(e)
    buf = yw_fa.get(path)
    ents = xpck.read(buf)
    for name, data in ents:
        if not name.endswith('.xi'):
            continue
        png_out = OUT / base_dir / mid / name.replace('.xi', '.png')
        key = path + '/' + name
        if png_out.exists():
            continue
        a, e = save_png(data, png_out, manifest, key, path)
        if a: ok += 1
        elif e: errs.append(e)
    return len(ents), ok, errs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--map', help='só estes mapas (ex.: --map t101i02)')
    ap.add_argument('--xa', help='só este .xa (caminho do .fa)')
    ap.add_argument('--all', action='store_true', help='.xa de subpastas também (bustup, help, ev_pic, map_caption)')
    a = ap.parse_args()

    manifest = []
    tot = collections.Counter()
    todo_xa = []
    if a.xa:
        todo_xa = [a.xa]
    else:
        for p in yw_fa.ls('data/menu/'):
            if not p.endswith('.xa'):
                continue
            d, nm = p.rsplit('/', 1)
            if d != 'data/menu' and not a.all:
                continue
            todo_xa.append(p)
    for p in todo_xa:
        n, ok, errs = extract_xa(p, manifest)
        tot['xa_containers'] += 1; tot['xa_entries'] += n; tot['xa_png'] += ok
        if errs: print('XA ERRO', p, errs[:3])

    maps = [a.map] if a.map else list(ROOM_IDS)
    for mid in maps:
        xc = 'data/map/%s/%s.xc' % (mid, mid)
        if not xc in yw_fa.index():
            print('mapa ausente:', mid); continue
        n, ok, errs = extract_xc(xc, manifest)
        tot['xc_containers'] += 1; tot['xc_entries'] += n; tot['xc_png'] += ok
        if errs: print('XC ERRO', mid, errs[:3])

    OUT.mkdir(parents=True, exist_ok=True)
    mj = OUT / 'manifest.json'
    if mj.exists():
        old = json.load(open(mj))
        by_key = {e['id']: e for e in old}
        for e in manifest: by_key[e['id']] = e
        manifest = sorted(by_key.values(), key=lambda e: e['id'])
    json.dump(manifest, open(mj, 'w'), indent=1, ensure_ascii=False)
    print(dict(tot), 'manifest em', mj)


if __name__ == '__main__':
    main()
