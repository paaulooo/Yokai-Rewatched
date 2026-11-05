import frame_sim as F, numpy as np, sys
from PIL import Image, ImageDraw
D=F.run([F.C+'list_%02d.bin'%i for i in range(1,8)])
def tris(idx,prim):
    t=[]
    if prim==1:   # strip
        for i in range(len(idx)-2):
            a,b,c=idx[i],idx[i+1],idx[i+2]
            if a==b or b==c or a==c: continue
            t.append((a,b,c) if i%2==0 else (b,a,c))
    elif prim==0: t=[tuple(idx[i:i+3]) for i in range(0,len(idx)-2,3)]
    else: t=[tuple(idx[i:i+3]) for i in range(0,len(idx)-2,3)]
    return t
def export(n,name,uvattr,posattr=0):
    L,R=D[n]; nv=R[0x228]; idx,at=F.vertices(R,nv)
    prim=(R[0x25e]>>8)&3
    T=tris(list(idx),prim)
    P=at[posattr]; UV=at[uvattr]
    with open(name+'.obj','w') as f:
        for p in P: f.write('v %f %f %f\n'%tuple(p[:3]))
        for u in UV: f.write('vt %f %f\n'%(u[0],u[1]))
        for a,b,c in T: f.write('f %d/%d %d/%d %d/%d\n'%(a+1,a+1,b+1,b+1,c+1,c+1))
    return P,T,at
if __name__=='__main__':
    P,T,at=export(13,'mesh_sala_draw13',2)
    print('tris',len(T))
    C=at[1][:,:3]
    # previews: tres projecoes, painter's algorithm, cor de vertice
    out=[]
    for name,(ax,ay,az,sg) in {'xy':(0,1,2,1),'xz':(0,2,1,1),'yz':(1,2,0,1)}.items():
        S=6; W=int((P[:,ax].max()-P[:,ax].min())*S)+20; H=int((P[:,ay].max()-P[:,ay].min())*S)+20
        im=Image.new('RGB',(W,H),(30,30,30)); d=ImageDraw.Draw(im)
        order=sorted(T,key=lambda t:P[list(t),az].mean())
        for t in order:
            pts=[((P[i,ax]-P[:,ax].min())*S+10,H-((P[i,ay]-P[:,ay].min())*S+10)) for i in t]
            col=tuple(int(v*255) for v in C[list(t)].mean(0))
            d.polygon(pts,fill=col)
        im.save('mesh_prev_%s.png'%name); out.append(im)
