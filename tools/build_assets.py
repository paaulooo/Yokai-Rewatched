"""Pipeline completo de assets: .fa -> arquivos -> texturas PNG -> modelos OBJ.
Uso: python build_assets.py   (tudo vai para work/)
"""
import subprocess,sys,os
here=os.path.dirname(os.path.abspath(__file__))
def run(*a): print('>>',*a); subprocess.check_call([sys.executable,*a],cwd=here)
run('fa_list.py','--extract')
run('export_xi.py')
run('export_all.py')
