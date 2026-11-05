import glob,os,collections,traceback
import sys,os
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from paths import *
import prm,xpck,imgc
import numpy as np
from PIL import Image
ok=0; err=collections.Counter(); texn=0; models=0
for p in sorted(glob.glob(str(FA_OUT/'data/character/**/*.xc'),recursive=True)):
    try: L=dict(xpck.read(open(p,'rb').read()))
    except Exception as e: err['xpck '+str(e)[:30]]+=1; continue
    prms=sorted(n for n in L if n.endswith('.prm'))
    if not prms: continue
    base=str(MODEL_OUT/os.path.relpath(p,FA_OUT/'data/character'))[:-3]
    os.makedirs(base,exist_ok=True); models+=1
    xis=sorted(n for n in L if n.endswith('.xi')); pngs={}
    for n in xis:
        try:
            img,H=imgc.decode(L[n],True,True); Image.fromarray(img,'RGBA').save(base+'/'+n[:-3]+'.png'); texn+=1
        except Exception as e: err['xi '+str(e)[:30]]+=1
    for n in prms:
        try:
            m=prm.parse(L[n]); P=m['vertices'][0]
            UV=next((m['vertices'][k] for k in (4,3,5) if k in m['vertices'] and m['vertices'][k].shape[1]==2),None)
            T=prm.strip_to_tris(m['indices'])
            prm.write_obj(base+'/'+n[:-4]+'.obj',P,UV,T); ok+=1
        except Exception as e: err[type(e).__name__+' '+str(e)[:40]]+=1
print('modelos',models,'prm ok',ok,'texturas',texn); print(err.most_common(8))
