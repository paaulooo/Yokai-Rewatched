"""Cruza chara_param -> chara_base -> chara_text (nomes) e mostra modelos/estatisticas dos Yo-kai.

Layouts dos registros: Albatross (Tiniifan, github.com/Tiniifan/Albatross, YW1/Logic/*.cs), conferidos aqui
contra os dados reais (os nomes/modelos precisam bater com os arquivos em data/chara/).
Uso: python yokai_probe.py [hash_ou_nome ...]   (sem args: lista todos)
"""
import sys, os, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import cfgbin, menu_probe as m

PREFIX = {0: 'c', 5: 'x', 6: 'y', 7: 'z'}
TRIBES = ['?', 'Brave', 'Mysterious', 'Tough', 'Charming', 'Heartful', 'Shady', 'Eerie', 'Slippery', 'Wicked', 'Boss']

def s32(x): return x - (1 << 32) if x >= 1 << 31 else x
def h(name): return s32(zlib.crc32(name.encode()))

def variant(x):
    if 0 <= x < 10: return '0%d0' % x
    if x < 100: return '%d0' % x
    return str(x)[::-1]

def rows(path, key):
    r, _ = cfgbin.parse(m.fa_read(path))
    return [[v for _, v in ps] for k, c, ps in r if k == key]

def load():
    names = {}
    for v in rows('data/res/text/chara_text_en.cfg.bin', 'NOUN_INFO'):
        names[v[0]] = v[5]
    base = {}
    for v in rows('data/res/character/chara_base_0.04j.cfg.bin', 'CHARA_BASE_INFO') + \
             rows('data/res/character/chara_base_0.04j.cfg.bin', 'CHARA_BASE_YOKAI_INFO'):
        model = PREFIX.get(v[1], '?') + '%03d' % v[2] + variant(v[3])
        base[v[0]] = dict(model=model, name=names.get(v[4], '#%08x' % (v[4] & 0xffffffff)), raw=v)
    params = {}
    for v in rows('data/res/character/chara_param_0.02.cfg.bin', 'CHARA_PARAM_INFO'):
        b = base.get(v[1], {})
        params[v[0]] = dict(base=b, tribe=v[2], hp=v[3], str=v[4], spr=v[5], dfn=v[6], spd=v[7],
                            attack=v[9], technique=v[11], inspirit=v[13], guard=v[15],
                            res=v[18:24], soul=v[24], ability=v[28], money=v[29], exp=v[30], raw=v)
    return names, base, params

if __name__ == '__main__':
    names, base, params = load()
    keys = rows('data/res/character/chara_base_0.04j.cfg.bin', 'CHARA_BASE_YOKAI_INFO')
    print('base:', len(base), 'yokai base rows:', len(keys), 'params:', len(params))
    want = sys.argv[1:]
    for ph, p in params.items():
        tag = ''
        if want:
            hs = [int(w[1:], 16) if w.startswith('#') else h(w) for w in want]
            hs = [s32(x & 0xffffffff) for x in hs]
            if ph not in hs: continue
        b = p['base']
        print('%11d %-8s %-14s %-10s HP%3d STR%3d SPR%3d DEF%3d SPD%3d exp%3d $%3d' % (
            ph, b.get('model', '?'), b.get('name', '?'), TRIBES[p['tribe']] if p['tribe'] < len(TRIBES) else p['tribe'],
            p['hp'], p['str'], p['spr'], p['dfn'], p['spd'], p['exp'], p['money']))
