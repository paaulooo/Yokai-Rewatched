"""Converte todos os .xi (IMGC) extraidos do .fa para PNG em work/xi_png/."""
import sys,os,glob,collections
sys.path.insert(0,os.path.dirname(os.path.abspath(__file__)))
from paths import *
import imgc
from PIL import Image
ok=0; err=collections.Counter()
for p in sorted(glob.glob(str(FA_OUT/'**/*.xi'),recursive=True)):
    out=XI_PNG/os.path.relpath(p,FA_OUT); out=out.with_suffix('.png')
    try:
        img,H=imgc.decode(open(p,'rb').read(),True,True)
        out.parent.mkdir(parents=True,exist_ok=True); Image.fromarray(img,'RGBA').save(out); ok+=1
    except Exception as e: err[str(e)[:50]]+=1
print(ok,'texturas;',dict(err))
