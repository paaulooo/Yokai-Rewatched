import struct, sys, os, json, numpy as np
import sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from paths import *
from PIL import Image
C=str(CAP)+os.sep
LIN=np.fromfile(C+'linear.bin',dtype=np.uint8)
VRS=np.fromfile(C+'vram_start.bin',dtype=np.uint8)
VRE=np.fromfile(C+'vram_end.bin',dtype=np.uint8)
def mem(pa,n):
    if 0x18000000<=pa<0x18600000: return VRS[pa-0x18000000:pa-0x18000000+n]
    if 0x20000000<=pa<0x24000000: return LIN[pa-0x20000000:pa-0x20000000+n]
    return None
def decode(buf):
    n=len(buf)//4; w=struct.unpack("<%dI"%n,buf[:n*4]); i=0
    while i+1<n:
        data=w[i]; hdr=w[i+1]; i+=2
        reg=hdr&0xFFFF; mask=(hdr>>16)&0xF; extra=(hdr>>20)&0xFF; consec=hdr>>31
        params=[data]
        for _ in range(extra):
            if i>=n: break
            params.append(w[i]); i+=1
        if i%2: i+=1
        yield reg,mask,consec,params
# ---- texturas
MORT=np.zeros(64,int)
for i in range(64):
    x=(i&1)|((i>>1)&2)|((i>>2)&4); y=((i>>1)&1)|((i>>2)&2)|((i>>3)&4); MORT[i]=y*8+x
def untile(buf,W,H,bpp):
    a=buf[:W*H*bpp].reshape(H//8,W//8,64,bpp); o=np.zeros_like(a); o[:,:,MORT,:]=a
    return o.reshape(H//8,W//8,8,8,bpp).transpose(0,2,1,3,4).reshape(H,W,bpp)[::-1]
MOD=[[2,8,-2,-8],[5,17,-5,-17],[9,29,-9,-29],[13,42,-13,-42],[18,60,-18,-60],[24,80,-24,-80],[33,106,-33,-106],[47,183,-47,-183]]
def etc1_block(v):
    px=np.zeros((4,4,3),np.uint8)
    diff=(v>>33)&1; flip=(v>>32)&1
    if diff:
        r=(v>>59)&31; g=(v>>51)&31; b=(v>>43)&31
        dr=(v>>56)&7; dg=(v>>48)&7; db=(v>>40)&7
        dr-=8*(dr>>2); dg-=8*(dg>>2); db-=8*(db>>2)
        r2=r+dr; g2=g+dg; b2=b+db
        c1=[(x<<3)|(x>>2) for x in (r,g,b)]; c2=[((x&31)<<3)|((x&31)>>2) for x in (r2,g2,b2)]
    else:
        r=(v>>60)&15; g=(v>>52)&15; b=(v>>44)&15; r2=(v>>56)&15; g2=(v>>48)&15; b2=(v>>40)&15
        c1=[x*17 for x in (r,g,b)]; c2=[x*17 for x in (r2,g2,b2)]
    cw1=(v>>37)&7; cw2=(v>>34)&7
    for x in range(4):
        for y in range(4):
            i=x*4+y
            sub=(0 if x<2 else 1) if flip==0 else (0 if y<2 else 1)
            c=c1 if sub==0 else c2; t=MOD[cw1 if sub==0 else cw2]
            idx=(((v>>(16+i))&1)<<1)|((v>>i)&1)
            m=t[idx]
            px[y,x]=[max(0,min(255,c[k]+m)) for k in range(3)]
    return px
def tex_decode(pa,W,H,fmt):
    n={0:4,1:3,2:2,3:2,4:2,5:2,6:2,7:1,8:1,9:1,10:0.5,11:0.5}.get(fmt)
    if fmt in (12,13):
        bs=8 if fmt==12 else 16; nb=W*H//16
        raw=mem(pa,nb*bs)
        if raw is None or len(raw)<nb*bs: return None
        img=np.zeros((H,W,4),np.uint8); img[:,:,3]=255
        k=0
        # tiles 8x8 de 4 blocos (2x2), ordem x primeiro
        for ty in range(H//8):
            for tx in range(W//8):
                for sb in range(4):
                    off=k*bs; k+=1
                    if fmt==13:
                        al=raw[off:off+8]; v=int.from_bytes(bytes(raw[off+8:off+16]),'little')
                    else: v=int.from_bytes(bytes(raw[off:off+8]),'little')
                    blk=etc1_block(v)
                    x0=tx*8+(sb&1)*4; y0=ty*8+(sb>>1)*4
                    img[y0:y0+4,x0:x0+4,:3]=blk
                    if fmt==13:
                        for i in range(16):
                            a=(int(al[i//2])>>(4*(i&1)))&15  # coluna-major
                            img[y0+(i%4),x0+(i//4),3]=a*17
        return img[::-1]
    if n is None or n<1: return None
    raw=mem(pa,int(W*H*n))
    if raw is None or len(raw)<W*H*n: return None
    if fmt==0:
        a=untile(raw,W,H,4); return a[:,:,[3,2,1,0]]
    if fmt==1:
        a=untile(raw,W,H,3); return np.dstack([a[:,:,[2,1,0]],np.full((H,W),255,np.uint8)])
    a=untile(raw,W,H,int(n)).astype(np.uint32)
    if fmt==4:
        v=a[:,:,0]|(a[:,:,1]<<8); return (np.stack([(v>>12)&15,(v>>8)&15,(v>>4)&15,v&15],-1)*17).astype(np.uint8)
    if fmt==3:
        v=a[:,:,0]|(a[:,:,1]<<8); return np.stack([((v>>11)&31)*255//31,((v>>5)&63)*255//63,(v&31)*255//31,np.full(v.shape,255)],-1).astype(np.uint8)
    if fmt==2:
        v=a[:,:,0]|(a[:,:,1]<<8); return np.stack([((v>>11)&31)*255//31,((v>>6)&31)*255//31,((v>>1)&31)*255//31,(v&1)*255],-1).astype(np.uint8)
    if fmt==5: l=a[:,:,1]; al=a[:,:,0]; return np.stack([l,l,l,al],-1).astype(np.uint8)
    if fmt==7: l=a[:,:,0]; return np.stack([l,l,l,np.full(l.shape,255)],-1).astype(np.uint8)
    if fmt==8: al=a[:,:,0]; return np.stack([al*0+255]*3+[al],-1).astype(np.uint8)
    if fmt==6: return np.stack([a[:,:,1],a[:,:,0],a[:,:,0]*0,a[:,:,0]*0+255],-1).astype(np.uint8)
    return None
FMTN={0:'RGBA8',1:'RGB8',2:'RGBA5551',3:'RGB565',4:'RGBA4',5:'IA8',6:'RG8',7:'I8',8:'A8',9:'IA4',10:'I4',11:'A4',12:'ETC1',13:'ETC1A4'}
# ---- simulador
def run(lists):
    R=[0]*0x400; draws=[]
    for L,fn in enumerate(lists):
        for reg,mask,consec,params in decode(open(fn,'rb').read()):
            for k,p in enumerate(params):
                r=reg+(k if consec else 0)
                if mask==0 or r>=0x400: continue
                old=R[r]; m=0
                for b in range(4):
                    if mask&(1<<b): m|=0xFF<<(8*b)
                R[r]=(old&~m)|(p&m)
                if r==0x22f:
                    draws.append((L,list(R)))
    return draws
def attrs(R):
    base=R[0x200]*8; fl=R[0x201]|(R[0x202]<<32); nattr=((R[0x202]>>28)&15)+1
    fmts=[(fl>>(4*i))&15 for i in range(12)]
    bufs=[]
    for b in range(12):
        o=0x203+b*3
        if R[o+2]==0 and R[o]==0 and R[o+1]==0: continue
        off=R[o]; ids=R[o+1]|((R[o+2]&0xFFFF)<<32); bpv=(R[o+2]>>16)&0xFF; nc=(R[o+2]>>28)&15
        bufs.append(dict(off=off,ids=[(ids>>(4*i))&15 for i in range(nc)],bpv=bpv,nc=nc))
    return base,fmts,nattr,bufs
def vertices(R,nv):
    base,fmts,nattr,bufs=attrs(R)
    ibase=base+(R[0x227]&0x0FFFFFFF); u16=R[0x227]>>31
    idx=np.frombuffer(bytes(mem(ibase,nv*(2 if u16 else 1))),dtype='<u2' if u16 else 'u1')
    out={}
    for bf in bufs:
        mx=int(idx.max())+1; raw=mem(base+bf['off'],mx*bf['bpv'])
        if raw is None: continue
        raw=np.array(raw).reshape(mx,bf['bpv']); o=0
        for a in bf['ids']:
            if a>=12: o+=(a-11)*4; continue
            f=fmts[a]; t=f&3; sz=(f>>2)+1; bw=[1,1,2,4][t]
            sub=np.ascontiguousarray(raw[:,o:o+sz*bw])
            dt=[np.int8,np.uint8,'<i2','<f4'][t]
            out[a]=sub.view(dt).reshape(mx,sz); o+=sz*bw
    return idx,out
if __name__=='__main__':
    frame=[C+'list_%02d.bin'%i for i in range(1,8)]  # lista 1..7 = 1 quadro (ver docs/GPU-PICA.md)
    D=run(frame)
    print(len(D),'draws')
    TEXDUMP.mkdir(parents=True,exist_ok=True); seen={}
    rows=[]
    for n,(L,R) in enumerate(D):
        nv=R[0x228]; en=R[0x080]&7; dim=R[0x082]; W=(dim>>16)&0x7ff; H=dim&0x7ff
        pa=R[0x085]*8; fmt=R[0x08e]&15
        key=(pa,W,H,fmt); name=''
        if en&1 and W and H:
            if key not in seen:
                im=tex_decode(pa,W,H,fmt)
                nm=str(TEXDUMP/('tex_%08x_%dx%d_%s.png'%(pa,W,H,FMTN.get(fmt,fmt))))
                if im is not None: Image.fromarray(im,'RGBA').save(nm); seen[key]=nm
                else: seen[key]='?'
            name=seen[key]
        rows.append((n,L,nv,en,pa,W,H,FMTN.get(fmt,fmt),name))
        print('%3d L%d nv=%4d tex=%d %08x %4dx%-4d %-7s %s'%rows[-1])
    json.dump(rows,open(WORK/'draws.json','w'))
