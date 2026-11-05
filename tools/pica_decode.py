"""Decodifica listas de comandos PICA200 (pares dado/cabecalho).
Uso: python pica_decode.py cmdlist_0000.bin [--all]
Formato (por comando): palavra0 = primeiro dado; palavra1 = cabecalho;
cabecalho: bits0-15 = registrador, 16-19 = mascara de bytes,
           20-27 = parametros extras, 31 = modo consecutivo.
Depois do cabecalho vem 'extra' palavras; o conjunto e alinhado em 8 bytes.
"""
import struct,sys,collections
# Nomes que conheco com boa confianca (de memoria; confirme no 3dbrew): demais ficam so com o ID.
import os,json
NAMES={}
_p=os.path.join(os.path.dirname(os.path.abspath(__file__)),"pica_regnames.json")
if os.path.exists(_p):   # nomes oficiais copiados do depurador do Azahar
    NAMES={int(k,16):v.replace("GPUREG_","") for k,v in json.load(open(_p)).items()}
NAMES.setdefault(0x22e,"DRAWARRAYS")
def decode(buf):
    n=len(buf)//4; w=struct.unpack("<%dI"%n,buf[:n*4]); i=0; out=[]
    while i+1<n:
        data=w[i]; hdr=w[i+1]; i+=2
        reg=hdr&0xFFFF; mask=(hdr>>16)&0xF; extra=(hdr>>20)&0xFF; consec=hdr>>31
        params=[data]
        for _ in range(extra):
            if i>=n: break
            params.append(w[i]); i+=1
        if i%2: i+=1                      # alinhamento de 8 bytes
        out.append((reg,mask,consec,params))
    return out
def main():
    f=sys.argv[1]; show_all="--all" in sys.argv
    cmds=decode(open(f,"rb").read())
    hist=collections.Counter(); draws=0
    for reg,mask,consec,params in cmds:
        for k,p in enumerate(params):
            r=reg+(k if consec else 0); hist[r]+=1
            if r in (0x22e,0x22f): draws+=1
        if show_all:
            nm=NAMES.get(reg,"")
            print("reg 0x%03x mask=%x%s %-22s %s"%(reg,mask," seq" if consec else "    ",nm," ".join("%08x"%p for p in params[:8])+(" ..." if len(params)>8 else "")))
    print("# comandos:",len(cmds),"| escritas de registrador:",sum(hist.values()),"| disparos de desenho:",draws)
    print("# registradores mais escritos:",", ".join("0x%03x(%d)"%(r,c) for r,c in hist.most_common(12)))
main()
