"""Mede o widescreen (G3) comparando capturas sem janela de duas rodadas da mesma rota: uma em 5:3 e outra larga.

    python wide_medir.py --a ../work/runs/g3_c53 --b ../work/runs/g3_c169 [--de 20 --ate 152]

Para cada instante com tela de cima nas duas rodadas, reduz as duas para 240 linhas e procura a escala horizontal `s` e
o deslocamento que fazem o MEIO da imagem larga bater com a de 5:3 (correlação dos gradientes horizontais, linha a
linha). Cena sem esticar ⇒ s ≈ 1,000 (esticada ⇒ s ≈ largura/400). Também mede, nas faixas laterais que só existem
na larga, a fração de pixels "com conteúdo" (diferente da cor de fundo) — objetos nas bordas aparecem, não somem.

As rodadas não são idênticas quadro a quadro (o relógio varia um pouco), então o resultado por instante tem ruído: o
script mostra a mediana e só conta instantes com correlação alta (câmera quase parada nas duas).
"""
import argparse
import re
from pathlib import Path

import numpy as np
from PIL import Image

NAME = re.compile(r"t(\d+)_s0_(\d+)x(\d+)\.rgba$")


def tops(d: Path):
    out = {}
    for p in d.iterdir():
        m = NAME.match(p.name)
        if m:
            out[int(m[1])] = (p, int(m[2]), int(m[3]))
    return out


def gray240(p, w, h):
    im = Image.frombytes("RGBA", (w, h), p.read_bytes()).convert("L")
    if h != 240:
        im = im.resize((round(w * 240 / h), 240), Image.LANCZOS)
    return np.asarray(im, dtype=np.float32)


def melhor_escala(a, b):
    """Escala s (b reamostrada por 1/s em torno do centro) e correlação máxima entre os gradientes x de a e do meio de b."""
    ga = np.diff(a, axis=1)
    ga = (ga - ga.mean()) / (ga.std() + 1e-6)
    hb, wb = b.shape
    cx_b = (wb - 1) / 2
    cx_a = (a.shape[1] - 1) / 2
    xs_a = np.arange(a.shape[1]) - cx_a
    best = (0.0, 1.0, 0.0)
    for s in np.arange(0.85, 1.40, 0.0025):
        for dx in np.arange(-3, 3.01, 0.5):
            xb = cx_b + xs_a * s + dx
            if xb[0] < 0 or xb[-1] > wb - 1:
                continue
            i0 = np.floor(xb).astype(int)
            f = xb - i0
            i1 = np.minimum(i0 + 1, wb - 1)
            rb = b[:, i0] * (1 - f) + b[:, i1] * f
            gb = np.diff(rb, axis=1)
            gb = (gb - gb.mean()) / (gb.std() + 1e-6)
            c = float((ga * gb).mean())
            if c > best[0]:
                best = (c, float(s), float(dx))
    return best


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--a", type=Path, required=True, help="rodada 5:3")
    ap.add_argument("--b", type=Path, required=True, help="rodada larga")
    ap.add_argument("--de", type=float, default=0)
    ap.add_argument("--ate", type=float, default=1e9)
    ap.add_argument("--min-corr", type=float, default=0.6)
    a = ap.parse_args()
    ta, tb = tops(a.a), tops(a.b)
    comuns = sorted(t for t in ta if t in tb and a.de * 10 <= t <= a.ate * 10)
    res = []
    for t in comuns:
        A = gray240(*ta[t])
        B = gray240(*tb[t])
        if B.shape[1] == A.shape[1]:
            continue
        c, s, dx = melhor_escala(A, B)
        # faixas laterais (só na larga): desvio padrão local > 2 = há imagem (não é cor chapada)
        lado = (B.shape[1] - A.shape[1]) // 2
        faixas = np.concatenate([B[:, :lado], B[:, -lado:]], axis=1) if lado > 0 else np.zeros((1, 1))
        conteudo = float((np.abs(np.diff(faixas, axis=1)) > 2).mean()) if lado > 1 else 0.0
        res.append((t / 10, c, s, dx, conteudo, B.shape[1]))
        print(f"t={t / 10:6.1f}s  larga {B.shape[1]}×240  corr {c:.3f}  escala {s:.4f}  desloc {dx:+.1f}  bordas com conteúdo {conteudo * 100:.0f}%")
    bons = [r for r in res if r[1] >= a.min_corr]
    if bons:
        ss = np.array([r[2] for r in bons])
        print(f"\n{len(bons)} de {len(res)} instantes com correlação ≥ {a.min_corr}: escala mediana {np.median(ss):.4f} "
              f"(p10 {np.percentile(ss, 10):.4f}, p90 {np.percentile(ss, 90):.4f}); esticada seria {bons[0][5] / 400:.4f}")


if __name__ == "__main__":
    main()
