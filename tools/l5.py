import struct
def lz10(d,size):
    o=bytearray(); i=0
    while len(o)<size:
        fl=d[i]; i+=1
        for b in range(8):
            if len(o)>=size: break
            if fl&(0x80>>b):
                x=d[i]; y=d[i+1]; i+=2
                ln=(x>>4)+3; disp=(((x&15)<<8)|y)+1
                for _ in range(ln): o.append(o[-disp])
            else: o.append(d[i]); i+=1
    return bytes(o[:size])
def rle(d,size):
    o=bytearray(); i=0
    while len(o)<size:
        f=d[i]; i+=1
        if f&0x80: o+=bytes([d[i]])*((f&0x7f)+3); i+=1
        else: n=(f&0x7f)+1; o+=d[i:i+n]; i+=n
    return bytes(o[:size])
def huff(d,size,bits):
    tsz=(d[0]+1)*2; tree=d[0:tsz]; pos=tsz
    stream=d[pos:]
    out=bytearray(); nib=[]; wi=0; bitbuf=0
    def bitgen():
        for w in range(0,len(stream)-3,4):
            v=struct.unpack_from('<I',stream,w)[0]
            for k in range(31,-1,-1): yield (v>>k)&1
    g=bitgen()
    node=1
    n=0
    while len(out)<size:
        cur=node
        while True:
            nd=tree[cur]; off=nd&0x3f
            nxt=(cur&~1)+off*2+2
            b=next(g)
            if b==0: leaf=nd&0x80; child=nxt
            else: leaf=nd&0x40; child=nxt+1
            if leaf: val=tree[child]; break
            cur=child
        if bits==8: out.append(val)
        else:
            nib.append(val)
            if len(nib)==2: out.append(nib[0]|(nib[1]<<4)); nib=[]
    return bytes(out[:size])
def l5dec(buf):
    h=struct.unpack_from('<I',buf,0)[0]; m=h&7; size=h>>3; body=buf[4:]
    if m==0: return body[:size]
    if m==1: return lz10(body,size)
    if m==2: return huff(body,size,4)
    if m==3: return huff(body,size,8)
    if m==4: return rle(body,size)
    raise ValueError('method %d'%m)
