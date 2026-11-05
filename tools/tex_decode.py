import numpy as np, sys
import sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from paths import *
from PIL import Image
VR=np.fromfile(os.environ.get('YW_VRAM',str(CAP/'vram_start.bin')),dtype=np.uint8)
def morton(n=8):
    xs=np.zeros((8,8),int); 
    idx=np.zeros(64,int)
    for i in range(64):
        x=(i&1)|((i>>1)&2)|((i>>2)&4); y=((i>>1)&1)|((i>>2)&2)|((i>>3)&4)
        idx[i]=y*8+x
    return idx
M=morton()
def untile(buf,W,H,bpp):
    a=buf[:W*H*bpp].reshape(H//8,W//8,64,bpp)
    out=np.zeros((H//8,W//8,64,bpp),np.uint8)
    out[:,:,M,:]=a
    out=out.reshape(H//8,W//8,8,8,bpp).transpose(0,2,1,3,4).reshape(H,W,bpp)
    return out[::-1]  # textures are bottom-up
def rgba8(off,W,H):
    a=untile(VR[off:],W,H,4)
    return a[:,:,[3,2,1,0]]  # bytes A,B,G,R -> R,G,B,A
def rgba4(off,W,H):
    a=untile(VR[off:],W,H,2).astype(np.uint16)
    v=a[:,:,0]|(a[:,:,1]<<8)
    r=(v>>12)&15;g=(v>>8)&15;b=(v>>4)&15;al=v&15
    return (np.stack([r,g,b,al],-1)*17).astype(np.uint8)
def save(name,arr):
    Image.fromarray(arr,'RGBA').save(name)
if __name__=='__main__':
    # uso: python tex_decode.py   (decodifica 3 texturas de exemplo + folha da VRAM inteira em work/)
    for off,W,H in [(0x38ca00,512,512),(0x0fd200,512,256),(0x17d200,512,256)]:
        a=rgba8(off,W,H); a[:,:,3]=255
        Image.fromarray(a[:,:,:3]).save(str(WORK/('t_%x_%dx%d.png'%(off,W,H))))
    # whole-VRAM sheet, 512 wide
    n=len(VR)//(512*4*8)*8
    a=rgba8(0,512,n); Image.fromarray(a[:,:,:3]).save(str(WORK/'vram_sheet512.png')); print(a.shape)
