import struct, l5
def read(buf):
    if buf[:4]!=b'XPCK': raise ValueError('not XPCK')
    m,cnt_t,io,no,do,isz,nsz,dsz=struct.unpack_from('<4sHHHHHHI',buf,0)
    n=cnt_t&0xFFF
    names=l5.l5dec(buf[no<<2:(no<<2)+(nsz<<2)])
    out=[]
    for i in range(n):
        h,nof,ol,sl,ou,su=struct.unpack_from('<IHHHBB',buf,(io<<2)+12*i)
        e=names.index(0,nof); name=names[nof:e].decode('shift-jis',errors='replace')
        off=((ou<<16)|ol)<<2; size=(su<<16)|sl
        data=buf[(do<<2)+off:(do<<2)+off+size]
        out.append((name,data))
    return out
