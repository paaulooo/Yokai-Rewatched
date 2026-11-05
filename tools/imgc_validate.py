import glob,os,re,collections,random,numpy as np
import sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from paths import *
import imgc
from PIL import Image
D=str(DUMP_TEX)
byd=collections.defaultdict(list)
for p in glob.glob(D+'/*.png'):
    m=re.match(r'tex1_(\d+)x(\d+)_',os.path.basename(p))
    if m: byd[(int(m[1]),int(m[2]))].append(p)
cache={}
def small(img):  # 32x32 RGB thumbnail em float
    return np.asarray(Image.fromarray(img[:,:,:3]).resize((32,32),Image.BOX),dtype=np.float32)
def dumpthumb(p):
    if p not in cache: cache[p]=small(np.asarray(Image.open(p).convert('RGB')))
    return cache[p]
random.seed(1)
fs=sorted(glob.glob(str(FA_OUT/'**/*.xi'),recursive=True)); random.shuffle(fs)
res=collections.Counter(); shown=0
for p in fs[:60]:
    try: img,H=imgc.decode(open(p,'rb').read(),False)
    except Exception as e: print('ERRO',p,e); continue
    cands=byd.get((H['w'],H['h']),[])
    if not cands: continue
    best=None
    for yf in (False,True):
        img,H=imgc.decode(open(p,'rb').read(),yf)
        for fl in (0,1):
            im=img[::-1] if fl else img
            t=small(im)
            for c in cands:
                d=np.abs(t-dumpthumb(c)).mean()
                if best is None or d<best[0]: best=(d,yf,fl,c)
    d,yf,fl,c=best
    res[(yf,fl,d<6)]+=1
    if shown<14: print('%-60s %dx%d fmt%x bd%d  diff=%.1f yfast=%s vflip=%s %s'%(p[7:67],H['w'],H['h'],H['fmt'],H['bd'],d,yf,fl,os.path.basename(c)[:34])); shown+=1
print(res)
