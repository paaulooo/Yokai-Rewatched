"""Confere a HUD dos quadros intermediários da interpolação contra o quadro real (bug do texto embaralhado).

Usa as capturas de uma rodada sem janela com `YW_SHOT_MIDS=1` (engine/src/hle/mod.rs::run_headless):
  DIR/tNNNNN_s0_WxH.rgba                 quadro real da tela de cima
  DIR/mids/tNNNNN_s0_mKdeS_WxH.rgba      intermediários K = 1..S-1 desse mesmo quadro
Para cada intermediário compara a faixa da HUD (padrão: a do "Objective") com a do real e conta os pixels diferentes.
Depois do F1 (texturas capturadas + HUD sem lerp) o esperado é 0 em todos.

  python hud_diff.py NOME                    # work/runs/NOME (ou um caminho de pasta)
  python hud_diff.py --dir ../work/recomp_frames --rect 8,4,200,20 --tol 0 --png
  --rect x,y,l,a   faixa em coordenadas NATIVAS da tela de cima (400x240); escalada para o tamanho da imagem
  --tol N          diferença máxima por canal ainda considerada igual (padrão 0)
  --png            grava [real | intermediário | mapa] da faixa em work/hud_diff/ para os que diferem

[H] A faixa padrão do "Objective" (canto superior esquerdo, 0,0,200,24) é estimada, não medida: confira numa
captura (python img_diff.py png ...) e ajuste --rect se o texto estiver em outro lugar.
"""
import argparse
import os
import re
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import WORK  # noqa: E402

REAL = re.compile(r"^t(\d+)_s0_(\d+)x(\d+)\.rgba$")
MID = re.compile(r"^t(\d+)_s0_m(\d+)de(\d+)_(\d+)x(\d+)\.rgba$")


def load(path, w, h):
    a = np.fromfile(path, dtype=np.uint8)
    if a.size != w * h * 4:
        raise ValueError(f"{path}: {a.size} bytes, esperado {w * h * 4}")
    return a.reshape(h, w, 4)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("run", nargs="?", help="nome da rodada (work/runs/NOME) ou pasta")
    ap.add_argument("--dir", help="pasta das capturas (padrão work/recomp_frames)")
    ap.add_argument("--rect", default="0,0,200,24", help="x,y,l,a em coordenadas nativas 400x240")
    ap.add_argument("--tol", type=int, default=0)
    ap.add_argument("--png", action="store_true")
    a = ap.parse_args()

    if a.dir:
        d = Path(a.dir)
    elif a.run:
        d = Path(a.run) if Path(a.run).is_dir() else WORK / "runs" / a.run
    else:
        d = WORK / "recomp_frames"
    mids_dir = d / "mids"
    if not mids_dir.is_dir():
        sys.exit(f"sem {mids_dir}: rode com YW_SHOT_MIDS=1 (e interpolação ligada)")
    rx, ry, rw, rh = (int(v) for v in a.rect.split(","))

    reals = {}
    for f in d.iterdir():
        m = REAL.match(f.name)
        if m:
            reals[int(m.group(1))] = (f, int(m.group(2)), int(m.group(3)))

    total = ruins = 0
    out_dir = WORK / "hud_diff"
    for f in sorted(mids_dir.iterdir()):
        m = MID.match(f.name)
        if not m:
            continue
        shot, k, steps, w, h = (int(v) for v in m.groups())
        if shot not in reals:
            print(f"{f.name}: sem o quadro real t{shot:05}")
            continue
        rf, w2, h2 = reals[shot]
        if (w2, h2) != (w, h):
            print(f"{f.name}: tamanho diferente do real ({w}x{h} vs {w2}x{h2})")
            continue
        s = w / 400.0
        x0, y0 = int(rx * s), int(ry * s)
        x1, y1 = min(w, int((rx + rw) * s)), min(h, int((ry + rh) * s))
        real = load(rf, w, h)[y0:y1, x0:x1, :3].astype(np.int16)
        mid = load(f, w, h)[y0:y1, x0:x1, :3].astype(np.int16)
        dif = (np.abs(real - mid) > a.tol).any(axis=2)
        n = int(dif.sum())
        total += 1
        ruins += n > 0
        print(f"t{shot:05} intermediário {k}/{steps - 1}: {n} pixels diferentes na faixa ({dif.size} pixels)")
        if a.png and n > 0:
            from PIL import Image

            out_dir.mkdir(parents=True, exist_ok=True)
            mapa = np.zeros_like(real, dtype=np.uint8)
            mapa[dif] = (255, 0, 0)
            mapa[~dif] = (real[~dif] // 3).astype(np.uint8)
            lado = np.concatenate([real.astype(np.uint8), mid.astype(np.uint8), mapa], axis=1)
            Image.fromarray(lado).save(out_dir / f"t{shot:05}_m{k}de{steps}.png")
    if total == 0:
        sys.exit("nenhum intermediário encontrado")
    print(f"{total} intermediários conferidos · {ruins} com a faixa diferente do real")
    sys.exit(1 if ruins else 0)


if __name__ == "__main__":
    main()
