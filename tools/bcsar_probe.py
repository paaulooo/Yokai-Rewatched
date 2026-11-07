"""Sonda do banco de som `snd/product/*.bcsar` (CSAR do 3DS): cabeçalho, blocos STRG/INFO/FILE, nomes e contagens.
Formato conferido contra o arquivo real; layout geral conforme a documentação pública do CSAR (3dbrew / vgmstream)."""
import struct, sys, os, re, glob, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import BASE

def find():
    return glob.glob(str(BASE / 'romfs' / '*' / 'snd' / 'product' / '*.bcsar'))[0]

def main():
    p = find(); d = open(p, 'rb').read()
    print(os.path.basename(p), len(d), d[:4], 'bom', d[4:6].hex())
    hsize, ver, fsize, nblocks = struct.unpack_from('<HIIH', d, 6)[0:4] if False else (struct.unpack_from('<H', d, 6)[0], struct.unpack_from('<I', d, 8)[0], struct.unpack_from('<I', d, 12)[0], struct.unpack_from('<H', d, 16)[0])
    print('header', hsize, 'versao %08x' % ver, 'tam', fsize, 'blocos', nblocks)
    blocks = {}
    for i in range(nblocks):
        t, _, off, sz = struct.unpack_from('<HHII', d, 0x14 + i * 12)
        blocks[t] = (off, sz); print('  bloco %04x off %d tam %d magic %s' % (t, off, sz, d[off:off + 4]))
    # STRG: tabela de nomes
    so, ss = blocks[0x2000]
    strg = d[so:so + ss]
    names = [s.decode('ascii', 'replace') for s in re.findall(rb'[A-Za-z0-9_\-\./]{3,}', strg)]
    print('strings', len(names))
    pre = collections.Counter(re.split(r'[_\-]', n)[0] for n in names)
    print(pre.most_common(25))
    print([n for n in names if n.startswith('SE_') or n.startswith('se_')][:12])
    # INFO: 8 referencias para tabelas (tipo, offset relativo ao corpo do bloco); cada tabela comeca com u32 contagem
    io, isz = blocks[0x2001]
    body = io + 8
    tabs = {}
    for i in range(8):
        t, _, off = struct.unpack_from('<HHI', d, body + i * 8)
        if off == 0xFFFFFFFF: continue
        n = struct.unpack_from('<I', d, body + off)[0]
        tabs[t] = n
    print('INFO tabelas (tipo: n)', {hex(k): v for k, v in tabs.items()})

if __name__ == "__main__":
    main()
