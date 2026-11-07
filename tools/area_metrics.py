"""Medidas por área (flags, objetos 3D, câmera, som, texturas/materiais) para a página de progresso.

Saída: work/notes/area_metrics.json + resumo no terminal. Roda de qualquer pasta: python area_metrics.py
Precisa de work/xq/*.txt (python xq32_catalog.py dumpall) e do RomFS. Os números de texturas vêm do teste Rust
`texture_stats` (cargo test --release texture_stats -- --ignored --nocapture), anotados à mão em TEXTURES abaixo.
"""
import collections, csv, glob, json, os, re, struct, sys, zlib
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import ROOT, WORK, BASE
import cfgbin, menu_probe as m
import progress_metrics as P

XQ = WORK / 'xq'
GROUPS = [('seq__battle__spcmnd', 'batalha: golpes'), ('seq__battle__encount', 'batalha: encontros'),
          ('seq__battle', 'batalha: sistema'), ('seq__event', 'eventos'), ('seq__menu', 'menus'),
          ('seq__map', 'mapas'), ('seq__phs', 'fases'), ('seq__sys', 'sistema'), ('seq__prog', 'comum')]


def s32(x):
    return x - (1 << 32) if x >= 1 << 31 else x


def group(name):
    return next((g for p, g in GROUPS if name.startswith(p)), 'outros')


def op_calls():
    """opcode -> Counter(grupo) a partir de `= opNNNN(` ou `= nome#NNNN(`."""
    pat = re.compile(r'=\s*(?:op|[a-z_0-9]+#)(\d+)\(')
    out = collections.defaultdict(collections.Counter)
    for f in XQ.glob('*.txt'):
        g = group(f.name)
        for line in open(f, encoding='utf8', errors='replace'):
            mm = pat.search(line)
            if mm:
                out[int(mm.group(1))][g] += 1
    return out


def flags():
    ents, _ = cfgbin.parse(m.fa_read('data/res/sys/flag_config_0.01.cfg.bin'))
    defined = {[v for _, v in ps][1]: [v for _, v in ps][0] for k, c, ps in ents if k == 'FLAG_INFO'}
    used = collections.Counter(); where = collections.Counter(); readers = collections.Counter(); named = set()
    for f in XQ.glob('*.txt'):
        g = group(f.name)
        for line in open(f, encoding='utf8', errors='replace'):
            mm = re.search(r'=\s*(?:op|flag_set#)(14000|14003|14001)\(#?([A-Za-z0-9_\-\']+)', line)
            if not mm:
                continue
            n = mm.group(2).strip("'")
            h = s32(int(n, 16) if re.fullmatch(r'[0-9a-f]{8}', n) else zlib.crc32(n.encode()))
            if mm.group(1) != '14001' and not re.fullmatch(r'[0-9a-f]{8}', n):
                named.add(h)
            if mm.group(1) == '14001':
                readers[h] += 1
            else:
                used[h] += 1; where[g] += 1
    d = set(defined)
    old_ents, _ = cfgbin.parse(m.fa_read('data/res/sys/flag_config.cfg.bin'))
    old_defined = {[v for _, v in ps][1] for k, c, ps in old_ents if k == 'FLAG_INFO'}
    both = d | old_defined
    return dict(keys_in_either_table=len(both), set_undefined_in_both=len(set(used) - both),
        set_only_in_old_table=len((set(used) - d) & old_defined),defined=len(d), max_bit=max(defined.values()), set_by_scripts=len(used), set_and_defined=len(set(used) & d),
                set_undefined=len(set(used) - d), defined_never_set=len(d - set(used)), set_calls=sum(used.values()),
                set_named=len(named), read_distinct=len(readers), read_calls=sum(readers.values()), set_where=dict(where))


def subcodes_in_events():
    ops = P.implemented_ops()
    sub = collections.Counter()
    for f in XQ.glob('seq__event__*.txt'):
        for line in open(f, encoding='utf8', errors='replace'):
            mm = re.search(r'event_cmd#2110\((\d+)', line)
            if mm:
                sub[int(mm.group(1))] += 1
    # subcomandos tratados = braços numéricos de 5 dígitos em `event_host.rs` (inclui os que só registram o estado)
    src = (ROOT / 'engine' / 'src' / 'event_host.rs').read_text(encoding='utf8')
    arm = re.compile(r'^\s*((?:\d{5}\s*\|\s*)*\d{5})\s*=>', re.M)
    real = {int(n) for m_ in arm.finditer(src) for n in re.findall(r'\d+', m_.group(1))}
    tot = sum(sub.values()); done = sum(c for s, c in sub.items() if s in real)
    bands = {}
    for s, c in sub.items():
        b = bands.setdefault(s // 1000 * 1000, dict(subcodes=0, handled=0, calls=0, handled_calls=0))
        b['subcodes'] += 1; b['calls'] += c
        if s in real:
            b['handled'] += 1; b['handled_calls'] += c
    return dict(distinct=len(sub), handled=len(real & set(sub)), calls=tot, handled_calls=done, bands=bands,
                top_missing=sorted(((c, s) for s, c in sub.items() if s not in real), reverse=True)[:14])


def camera_calls():
    cam = collections.Counter()
    for f in XQ.glob('seq__event__*.txt'):
        for line in open(f, encoding='utf8', errors='replace'):
            for mm in re.finditer(r'call\(#([A-Za-z0-9_]*[Cc]am[A-Za-z0-9_]*)', line):
                cam[mm.group(1)] += 1
    return dict(cam.most_common(12))


def sound():
    p = glob.glob(str(BASE / 'romfs' / '*' / 'snd' / 'product' / '*.bcsar'))[0]
    d = open(p, 'rb').read()
    blocks = {}
    for i in range(struct.unpack_from('<H', d, 16)[0]):
        t, _, off, sz = struct.unpack_from('<HHII', d, 0x14 + i * 12)
        blocks[t] = (off, sz)
    names = {s.decode() for s in re.findall(rb'[A-Za-z0-9_\-\./]{3,}', d[blocks[0x2000][0]:blocks[0x2000][0] + blocks[0x2000][1]])}
    body = blocks[0x2001][0] + 8
    tabs = {}
    for i in range(8):
        t, _, off = struct.unpack_from('<HHI', d, body + i * 8)
        if off != 0xFFFFFFFF:
            tabs[hex(t)] = struct.unpack_from('<I', d, body + off)[0]
    used = collections.Counter(); nums = set()
    for f in XQ.glob('*.txt'):
        for line in open(f, encoding='utf8', errors='replace'):
            mm = re.search(r"se_play#14804\(('([^']+)'|(\d+))", line)
            if mm:
                (used.update([mm.group(2)]) if mm.group(2) else nums.add(int(mm.group(3))))
    cues = [r for r in csv.DictReader(open(ROOT / 'data' / 'romfs' / 'yw1_a_files.csv')) if r['path'].startswith('data/snd/cfg/event/')]
    nonempty = events = 0
    for r in cues:
        e, _ = cfgbin.parse(m.fa_read(r['path']))
        n = sum(1 for x in e if x[0] == 'EVENT')
        events += n; nonempty += bool(n)
    return dict(bcsar_bytes=len(d), strings=len(names), info_tables=tabs, se_named_distinct=len(used),
                se_named_in_bank=sum(1 for n in used if n in names), se_named_calls=sum(used.values()), se_numeric_ids=len(nums),
                cue_cfgs=len(cues), cue_cfgs_nonempty=nonempty, cue_events=events)


# texturas: saída do teste Rust `texture_stats` (formato IMGC fmt/bd -> nº de texturas)
TEXTURES = {'total': 7958, 'etc1_family': 3697, 'rgba8': 1357, 'etc_pixel_share': 0.818}


def main():
    calls = op_calls()
    watch = {'flags (14000)': 14000, 'flags (14003)': 14003, 'dados de golpe (14060)': 14060, 'som (14804)': 14804,
             'objeto 3D (6001)': 6001, 'objeto 3D (6046)': 6046, 'câmera (4010)': 4010, 'câmera (4500)': 4500,
             'câmera (4602)': 4602}
    where = {k: dict(total=sum(calls[v].values()), **dict(calls[v].most_common(4))) for k, v in watch.items()}
    out = dict(flags=flags(), events_op2110=subcodes_in_events(), camera_calls_events=camera_calls(), sound=sound(),
               opcode_usage_by_group=where, textures=TEXTURES)
    json.dump(out, open(WORK / 'notes' / 'area_metrics.json', 'w', encoding='utf8'), indent=1, ensure_ascii=False)
    f, e, s = out['flags'], out['events_op2110'], out['sound']
    print(f"flags: {f['defined']} definidas (bit máx {f['max_bit']}); scripts escrevem {f['set_by_scripts']} ({f['set_and_defined']} definidas, {f['set_undefined']} não)")
    print(f"eventos op2110: {e['handled']}/{e['distinct']} subcomandos tratados; {e['handled_calls']}/{e['calls']} chamadas ({100*e['handled_calls']/e['calls']:.1f}%)")
    print('camera:', out['camera_calls_events'])
    print(f"som: {s['strings']} nomes, tabelas {s['info_tables']}, se_play por nome {s['se_named_distinct']} (todos no banco: {s['se_named_in_bank']}), cues {s['cue_cfgs_nonempty']}/{s['cue_cfgs']}")
    print('uso por grupo:', {k: v['total'] for k, v in where.items()})


if __name__ == '__main__':
    main()
