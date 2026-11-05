import struct, numpy as np
import l5, etc1
def parse(buf):
    if buf[:4]!=b'IMGC': raise ValueError('not IMGC')
    magic,eo,fmt,c1,cnt,bd,bpt,w,h,pio,pic,iio,iic,dofs,plat=struct.unpack_from('<8shBBBBhhhHHHHii',buf,0)
    imgs=[struct.unpack_from('<4i',buf,iio+24*i) for i in range(iic)]
    return dict(fmt=fmt,cnt=cnt,bd=bd,w=w,h=h,dofs=dofs,imgs=imgs,pal=(pio,pic),plat=plat)
def tiles(buf,H):
    t0,ts,d0,ds=H['imgs'][0]; base=H['dofs']
    tt=l5.l5dec(buf[base+t0:base+t0+ts]); dd=l5.l5dec(buf[base+d0:base+d0+ds])
    if struct.unpack_from('<H',tt,0)[0]==0x453: raise NotImplementedError('legacy tile table')
    ents=np.frombuffer(tt,dtype='<i2'); tb=64*H['bd']//8
    dd=np.frombuffer(dd,dtype=np.uint8)
    out=np.zeros((len(ents),tb),np.uint8)
    for i,e in enumerate(ents.tolist()):
        if e>=0 and (e+1)*tb<=len(dd): out[i]=dd[e*tb:(e+1)*tb]
    return out
def morton_xy(yfast):
    xs=np.zeros(64,int); ys=np.zeros(64,int)
    for p in range(64):
        a=(p&1)|((p>>1)&2)|((p>>2)&4)   # bits 0,2,4
        b=((p>>1)&1)|((p>>2)&2)|((p>>3)&4)  # bits 1,3,5
        xs[p],ys[p]=(b,a) if yfast else (a,b)
    return xs,ys
def decode(buf,yfast=True,tb=True):
    H=parse(buf); fmt=H['fmt']; bd=H['bd']; W=(H['w']+7)&~7; Hh=(H['h']+7)&~7
    T=tiles(buf,H); nx=W//8; ny=Hh//8
    T=T[:nx*ny]
    xs,ys=morton_xy(yfast)
    img=np.zeros((Hh,W,4),np.uint8); img[:,:,3]=255
    if fmt in (0x1B,0x1C,0x1D) and bd in (4,8):
        bs=8 if bd==4 else 16
        for t in range(len(T)):
            ty,tx=divmod(t,nx)
            for sb in range(4):
                bx,by=((sb>>1),(sb&1)) if yfast else ((sb&1),(sb>>1))
                o=sb*bs; raw=T[t][o:o+bs]
                if bd==8: al=raw[:8]; v=int.from_bytes(bytes(raw[8:16]),'little')
                else: v=int.from_bytes(bytes(raw[:8]),'little')
                blk=etc1.etc1_block(v)
                if tb: blk=blk.transpose(1,0,2)
                y0=ty*8+by*4; x0=tx*8+bx*4
                img[y0:y0+4,x0:x0+4,:3]=blk
                if bd==8:
                    for i in range(16):
                        yy,xx=(i%4,i//4) if not tb else (i//4,i%4)
                        img[y0+yy,x0+xx,3]=((int(al[i//2])>>(4*(i&1)))&15)*17
        return img[:H['h'],:H['w']],H
    # pixel formats
    P=np.zeros((len(T),64,4),np.uint8); P[:,:,3]=255
    if bd==32:
        a=T.reshape(-1,64,4); P=np.stack([a[:,:,3],a[:,:,2],a[:,:,1],a[:,:,0]],-1)  # ABGR bytes
    elif bd==24:
        a=T.reshape(-1,64,3); P[:,:,:3]=a[:,:,::-1]
    elif bd==16:
        a=T.reshape(-1,64,2).astype(np.uint32); v=a[:,:,0]|(a[:,:,1]<<8)
        if fmt==1: P=(np.stack([(v>>12)&15,(v>>8)&15,(v>>4)&15,v&15],-1)*17).astype(np.uint8)
        elif fmt==2: P=np.stack([((v>>11)&31)*255//31,((v>>6)&31)*255//31,((v>>1)&31)*255//31,(v&1)*255],-1).astype(np.uint8)
        elif fmt==4: P=np.stack([((v>>11)&31)*255//31,((v>>5)&63)*255//63,(v&31)*255//31,v*0+255],-1).astype(np.uint8)
        else: l=a[:,:,1]; al=a[:,:,0]; P=np.stack([l,l,l,al],-1).astype(np.uint8)  # LA88
    elif bd==8:
        a=T.reshape(-1,64).astype(np.uint32)
        if fmt in (0x0B,0x0C) and fmt!=0x0D and False: pass
        l=a; P=np.stack([l,l,l,l*0+255],-1).astype(np.uint8)
        if fmt in (0x0E,0x0F): P=np.stack([l*0+255]*3+[l],-1).astype(np.uint8)
        if fmt in (0x0B,0x0C) : lo=(a&15)*17; hi=(a>>4)*17; P=np.stack([hi,hi,hi,lo],-1).astype(np.uint8)
    elif bd==4:
        a=T.reshape(-1,32); n=np.stack([a&15,a>>4],-1).reshape(-1,64).astype(np.uint32)*17
        P=np.stack([n,n,n,n*0+255],-1).astype(np.uint8)
        if fmt in (0x0F,0x10): P=np.stack([n*0+255]*3+[n],-1).astype(np.uint8)
    for t in range(len(T)):
        ty,tx=divmod(t,nx)
        img[ty*8+ys,tx*8+xs]=P[t]
    return img[:H['h'],:H['w']],H
