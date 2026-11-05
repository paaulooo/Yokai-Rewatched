import struct, numpy as np, l5
def _blob(d,off): return l5.l5dec(d[off:])
def parse(d):
    """Le um XMPR (.prm): devolve dict(vertices={attr:array}, stride, count, indices, attrs, strings)."""
    assert d[:4]==b'XMPR'
    i=d.find(b'XPVB'); o_attr,o_fix,o_vert,stride,count=struct.unpack_from('<4HI',d,i+4)
    A=_blob(d,i+o_attr); fixed=np.frombuffer(_blob(d,i+o_fix),dtype='<f4'); vb=_blob(d,i+o_vert)
    vb=np.frombuffer(vb,dtype=np.uint8)[:stride*count].reshape(count,stride)
    attrs=[]; verts={}
    for k in range(len(A)//4):
        t,off,size,flag=A[4*k:4*k+4]
        if size==0: continue
        attrs.append((k,t,off,size,flag))
        if flag&1: continue            # atributo constante (vem da tabela fixa)
        raw=np.ascontiguousarray(vb[:,off:off+size])
        verts[k]=raw.view('<f4').reshape(count,size//4) if t in (2,3,4) else raw
    j=d.find(b'XPVI'); ifmt,ioff,icnt=struct.unpack_from('<HHI',d,j+4)
    idx=np.frombuffer(_blob(d,j+ioff),dtype='<u2' if ifmt==2 else 'u1')[:icnt].astype(np.int32)
    return dict(vertices=verts,attrs=attrs,fixed=fixed,stride=stride,count=count,indices=idx)
def strip_to_tris(idx):
    t=[]
    for k in range(len(idx)-2):
        a,b,c=int(idx[k]),int(idx[k+1]),int(idx[k+2])
        if a==b or b==c or a==c: continue
        t.append((a,b,c) if k%2==0 else (b,a,c))
    return t
def write_obj(name,P,UV,tris):
    with open(name,'w') as f:
        for p in P: f.write('v %f %f %f\n'%tuple(p[:3]))
        if UV is not None:
            for u in UV: f.write('vt %f %f\n'%(u[0],1-u[1]))
        for a,b,c in tris:
            f.write(('f %d/%d %d/%d %d/%d\n'%(a+1,a+1,b+1,b+1,c+1,c+1)) if UV is not None else 'f %d %d %d\n'%(a+1,b+1,c+1))
