"""Mede onde um elemento de HUD da tela de cima foi parar no widescreen (fase G5).

Compara a mesma captura (mesmo instante do console, mesmo roteiro) de uma rodada em 5:3 e de uma larga: recorta o
elemento na 5:3 (caixa em pixels da tela de cima 400x240), procura o recorte na larga deslizando na horizontal (menor
diferença média; a cena atrás muda com o frustum largo, o elemento opaco não) e diz a distância às bordas.

    python hud_medir.py --ref ../work/runs/A_53 --wide ../work/runs/A_219 --t 205.0 --box 4,6,150,30 [--box ...]

Saída por caixa: x na 5:3, x achado na larga, distância à esquerda (5:3 → larga), à direita, ao centro, e o erro
contra a âncora esperada (esq/centro/dir, a mais próxima). Só lê as capturas .rgba do jogo (nunca a tela).
"""
import argparse
import re
from pathlib import Path

import numpy as np

NAME = re.compile(r"t(\d+)_s0_(\d+)x(\d+)\.rgba$")


def top(d: Path, t: float):
    want = round(t * 10)
    for p in d.iterdir():
        m = NAME.match(p.name)
        if m and int(m[1]) == want:
            w, h = int(m[2]), int(m[3])
            a = np.fromfile(p, np.uint8).reshape(h, w, 4)[..., :3].astype(np.float32)
            if h != 240:  # resolução interna N×: reduz por média de blocos
                n = h // 240
                a = a.reshape(240, n, w // n, n, 3).mean(axis=(1, 3))
            return a
    raise SystemExit(f"sem captura da tela de cima em {t}s em {d}")


def medir(ref, wide, box):
    x0, y0, x1, y1 = box
    tpl = ref[y0:y1, x0:x1]
    w = x1 - x0
    W = wide.shape[1]
    best = None
    for dx in range(0, W - w + 1):
        d = np.abs(wide[y0:y1, dx:dx + w] - tpl).mean()
        if best is None or d < best[0]:
            best = (d, dx)
    return best


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ref", type=Path, required=True)
    ap.add_argument("--wide", type=Path, required=True)
    ap.add_argument("--t", type=float, required=True)
    ap.add_argument("--box", action="append", required=True, help="x0,y0,x1,y1 na tela de cima 400x240 (5:3)")
    a = ap.parse_args()
    ref, wide = top(a.ref, a.t), top(a.wide, a.t)
    W = wide.shape[1]
    print(f"t={a.t}s · 5:3 {ref.shape[1]}x240 · larga {W}x240")
    for b in a.box:
        x0, y0, x1, y1 = map(int, b.split(","))
        x1, y1 = min(x1, ref.shape[1]), min(y1, 240)
        d, dx = medir(ref, wide, (x0, y0, x1, y1))
        esq = (x0, dx)
        dir_ = (400 - x1, W - (dx + x1 - x0))
        cen = (x0 - 200, dx - W / 2)
        erros = {"esq": abs(esq[1] - esq[0]), "dir": abs(dir_[1] - dir_[0]), "centro": abs(cen[1] - cen[0])}
        lado = min(erros, key=erros.get)
        print(
            f"caixa {x0},{y0},{x1},{y1}: x 5:3 {x0} → larga {dx} (dif. média {d:.1f}) · borda esq {esq[0]}→{esq[1]} · "
            f"borda dir {dir_[0]}→{dir_[1]} · centro {cen[0]:+.0f}→{cen[1]:+.0f} · âncora {lado}, erro {erros[lado]:.1f} px"
        )


if __name__ == "__main__":
    main()
