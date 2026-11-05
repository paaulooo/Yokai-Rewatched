"""Le o indice do yw1_a.fa (formato ARC0 da Level-5) e, opcionalmente, extrai tudo.
Uso:  python fa_list.py            -> lista e grava work/fa_files.json
      python fa_list.py --extract  -> tambem extrai tudo para work/fa_out/ (~430 MB)
"""
import struct,json,sys,os,collections
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from paths import *
import l5
def read_index(f):
    hd=f.read(72)
    magic,de,dh,fe,no,da,dec,dhc,fec=struct.unpack_from('<4s5I2hI',hd,0)
    assert magic==b'ARC0',magic
    def seg(a,b): f.seek(a); return l5.l5dec(f.read(b-a))
    D=seg(de,dh); FE=seg(fe,no); N=seg(no,da)
    dirs=[struct.unpack_from('<IHhHhii',D,i*20) for i in range(dec)]
    ents=[struct.unpack_from('<IIII',FE,i*16) for i in range(fec)]
    def cstr(o):
        e=N.index(0,o); return N[o:e].decode('shift-jis',errors='replace')
    files=[]
    for crc,fdi,dc,ffi,fc,fnoff,dnoff in dirs:
        dn=cstr(dnoff)
        for e in ents[ffi:ffi+fc]:
            files.append((dn+cstr(fnoff+e[1]),da+e[2],e[3]))   # (caminho, offset absoluto, tamanho)
    return files
if __name__=='__main__':
    f=open(FA,'rb'); files=read_index(f)
    json.dump(files,open(WORK/'fa_files.json','w'))
    print(len(files),'arquivos')
    ext=collections.Counter(p.rsplit('.',1)[-1] if '.' in p else '' for p,_,_ in files)
    print(ext.most_common(25))
    if '--extract' in sys.argv:
        for p,o,s in files:
            out=FA_OUT/p; out.parent.mkdir(parents=True,exist_ok=True)
            f.seek(o); out.write_bytes(f.read(s))
        print('extraido em',FA_OUT)
