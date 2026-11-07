"""Desmontador de scripts XQ32 da Level-5 (`seq/**/*.xq`).

Formato (verificado em seq/menu/TitleMenu.xq, ver docs/FORMATS.md):
  cabeçalho: "XQ32", u16 nº de funções, depois pares u16 (offset em palavras, quantidade) para
  funções (24 B), saltos (12 B), instruções (12 B), argumentos (8 B); u16 nº de globais; u16 offset das strings.
  Cada tabela é um bloco L5.
  função : u32 nome, u32 crc32(nome), u16 instr. início, u16 instr. fim, u16 1º salto, u16 nº saltos, u16, u16
  salto  : u32 nome, u32 crc32(nome), u32 índice da instrução
  instr. : u16 1º argumento, u16 nº argumentos, u16 variável de retorno, u16 opcode, u32 0
  arg.   : u32 tipo (1 int, 2 hash, 3 float, 4 variável, 24 string), u32 valor
Opcodes conhecidos com certeza: 20 = chamar função do script pelo hash. O resto é mostrado cru.

Uso: python xq32_disasm.py ARQUIVO.xq            (arquivo solto)
     python xq32_disasm.py --fa seq/menu/TitleMenu.xq   (lido direto do yw1_a.fa)
"""
import struct, sys, os, zlib, csv
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import l5


def parse(d):
    if d[:4] != b'XQ32':
        raise ValueError('não é XQ32')
    h = struct.unpack_from('<10H', d, 4)
    nfunc, (fo, jo, jn, io, inn, ao, an, so) = h[0], (h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[9])
    F, J, I, A = (l5.l5dec(d[o * 4:]) for o in (fo, jo, io, ao))
    S = l5.l5dec(d[so * 4:])
    def s(o):
        return S[o:S.index(b'\0', o)].decode('shift-jis', 'replace')
    funcs = [struct.unpack_from('<IIHHHHHH', F, i * 24) for i in range(nfunc)]
    jumps = [struct.unpack_from('<III', J, i * 12) for i in range(jn)]
    insts = [struct.unpack_from('<HHHHI', I, i * 12) for i in range(inn)]
    args = [struct.unpack_from('<Ii', A, i * 8) for i in range(an)]
    names = {zlib.crc32(x): x.decode('shift-jis', 'replace') for x in S.split(b'\0') if x}
    return dict(funcs=funcs, jumps=jumps, insts=insts, args=args, s=s, names=names)


def fmt_arg(x, t, v):
    if t == 1:
        return str(v)
    if t == 2:
        n = x['names'].get(v & 0xffffffff)
        return '#%s' % n if n else '#%08x' % (v & 0xffffffff)
    if t == 3:
        return '%gf' % struct.unpack('<f', struct.pack('<i', v))[0]
    if t == 4:
        return '$%d' % v
    if t == 24:
        return repr(x['s'](v))
    return '?%d:%d' % (t, v)


def disasm(x):
    labels = {}
    for j in x['jumps']:
        labels.setdefault(j[2], []).append(x['s'](j[0]))
    out = []
    for f in sorted(x['funcs'], key=lambda f: f[2]):
        out.append('\nfunc %s  (instr %d..%d, extras %s)' % (x['s'](f[0]), f[2], f[3], f[4:]))
        for i in range(f[2], f[3]):
            for lb in labels.get(i, []):
                out.append('  %s:' % lb)
            a0, ac, ret, op, _ = x['insts'][i]
            args = ', '.join(fmt_arg(x, *x['args'][a0 + k]) for k in range(ac))
            name = 'call' if op == 20 else 'op%d' % op
            out.append('  %4d  $%-5d = %s(%s)' % (i, ret, name, args))
    return '\n'.join(out)


if __name__ == '__main__':
    if sys.argv[1] == '--fa':
        from paths import FA, ROOT
        rows = {r['path']: r for r in csv.DictReader(open(ROOT / 'data' / 'romfs' / 'yw1_a_files.csv'))}
        r = rows[sys.argv[2]]
        with open(FA, 'rb') as f:
            f.seek(int(r['offset_in_romfs_fa']))
            data = f.read(int(r['size']))
    else:
        data = open(sys.argv[1], 'rb').read()
    print(disasm(parse(data)))
