"""Leitor de .cfg.bin (Level-5 "CfgBin", rodape 't2b'). Decifrado a partir de data/txt/op00a_en.cfg.bin e
validado em ~231 arquivos .cfg.bin (ver work/notes/cfgbin_validation.txt).

Layout (little endian):
  0x00 u32 nº de entradas | u32 off. tabela de strings | u32 tamanho | u32 nº de strings(?)
  0x10 entradas ate a tabela de strings. Entrada: u32 crc32(nome da chave) | u8 nº de params |
       ceil(n/4) bytes de tipos (2 bits por param, LSB primeiro: 0 = string(offset), 1 = int, 2 = float, 3 = ?) |
       preenchimento 0xFF ate multiplo de 4 (contado desde o inicio da entrada) | n × u32 parametros
  tabela de strings (off. acima, preenchida com 0xFF ate 16 B)
  tabela de chaves: u32 tam. total | u32 nº de chaves | u32 off. das strings (rel. ao inicio da tabela) | u32 tam. strings |
       n × (u32 crc, u32 off. da string) | strings das chaves (ex.: TEXT_INFO_BEGIN, TEXT_INFO, TEXT_INFO_END)
  rodape: 01 'tb2'... ("t2b\xfe 01 01 00 01 00")
O nome de cada chave vem da tabela de chaves (quando existir); caso contrario fica '%08x'.
"""
import struct, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

def decode_str(b):
    try: return b.decode('utf-8')
    except UnicodeDecodeError: return b.decode('shift-jis', 'replace')

def parse(d):
    n, so, sl, sc = struct.unpack_from('<4I', d, 0)
    S = d[so:so + sl]
    def string(o): return decode_str(S[o:S.index(b'\0', o)]) if 0 <= o < len(S) else None
    # tabela de chaves: alinhada a 16 depois das strings
    ko = (so + sl + 15) & ~15
    keys = {}
    try:
        ksz, kn, kso, ksl = struct.unpack_from('<4I', d, ko)
        for i in range(kn):
            c, o = struct.unpack_from('<II', d, ko + 16 + 8 * i)
            e = d.index(b'\0', ko + kso + o); keys[c] = d[ko + kso + o:e].decode('ascii', 'replace')
    except Exception: pass
    p = 0x10; rows = []
    for _ in range(n):
        crc, pc = struct.unpack_from('<IB', d, p)
        nt = (pc + 3) // 4
        tb = d[p + 5:p + 5 + nt]
        hdr = (4 + 1 + nt + 3) & ~3
        params = []
        for i in range(pc):
            t = (tb[i // 4] >> (2 * (i % 4))) & 3
            raw = struct.unpack_from('<I', d, p + hdr + 4 * i)[0]
            if t == 0: params.append(('s', string(raw) if raw != 0xffffffff else None))
            elif t == 1: params.append(('i', struct.unpack('<i', struct.pack('<I', raw))[0]))
            elif t == 2: params.append(('f', struct.unpack('<f', struct.pack('<I', raw))[0]))
            else: params.append(('?', raw))
        rows.append((keys.get(crc, '%08x' % crc), crc, params))
        p += hdr + 4 * pc
    assert p <= so, (p, so)
    return rows, keys

def fmt_rows(rows):
    out = []
    for k, c, ps in rows:
        out.append('%s: %s' % (k, ' | '.join(repr(v) if t == 's' else ('%g' % v if t == 'f' else str(v) if t == 'i' else '?%x' % v) for t, v in ps)))
    return '\n'.join(out)

if __name__ == '__main__':
    import menu_probe as m
    d = m.fa_read(sys.argv[1]) if not os.path.exists(sys.argv[1]) else open(sys.argv[1], 'rb').read()
    print(fmt_rows(parse(d)[0]))
