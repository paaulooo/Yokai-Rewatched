"""Caminhos centrais do projeto. Tudo pode ser sobrescrito por variaveis de ambiente.

Layout esperado (BASE = pasta que contem yokai-port/, romfs/, 'Dump Texturas'/ ...):
    BASE/
      romfs/yw1_a.fa            <- RomFS extraido do CIA (ctrtool ou Azahar)
      Dump Texturas/            <- dump de texturas do Azahar (opcional, so para validar)
      yokai-port/               <- ESTE projeto (ROOT)
"""
import os
from pathlib import Path
ROOT = Path(os.environ.get("YW_ROOT", Path(__file__).resolve().parent.parent))
BASE = Path(os.environ.get("YW_BASE", ROOT.parent))
FA = Path(os.environ.get("YW_FA", BASE / "romfs" / "yw1_a.fa"))
DUMP_TEX = Path(os.environ.get("YW_DUMPTEX", BASE / "Dump Texturas"))
CAP = Path(os.environ.get("YW_CAP", ROOT / "data" / "capture" / "cap"))          # saida de gdb/dump_frame2.gdb
WORK = Path(os.environ.get("YW_WORK", ROOT / "work"))                              # tudo que e gerado (gitignore)
FA_OUT = WORK / "fa_out"        # arquivos extraidos do yw1_a.fa
XI_PNG = WORK / "xi_png"        # .xi convertidos para PNG
MODEL_OUT = WORK / "model_out"  # .prm -> .obj (+ texturas)
TEXDUMP = WORK / "texdump"      # texturas do quadro capturado
for p in (WORK,): p.mkdir(parents=True, exist_ok=True)
