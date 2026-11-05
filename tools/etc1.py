import numpy as np
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
