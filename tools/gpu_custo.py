"""Custo de GPU de uma rodada sem janela, das linhas `[hle] Ns:` do log (a cada 5 s do console).

    python gpu_custo.py ../work/runs/g3_c53.log ../work/runs/g3_c169.log [--de 100 --ate 150]

Mostra, na janela de tempo pedida, a média de: quadros do jogo/s, GPU executando nativo e sombra (ms/s), espera da GPU
e o sincronismo da thread "gpu" — e a razão sombra/sombra da primeira rodada (custo relativo, ex.: 5:3 × 16:9).
"""
import argparse
import re
from pathlib import Path

LINHA = re.compile(r"\[hle\] (\d+)s: ([\d.]+) quadros/s do jogo.*?sincronismo (\d+).*?espera (\d+) ms/s \(GPU executando: nativo (\d+) \+ sombra (\d+) ms/s")


def medir(p: Path, de: float, ate: float):
    v = []
    for l in p.read_text(encoding="utf8", errors="replace").splitlines():
        m = LINHA.search(l)
        if m and de <= int(m[1]) <= ate:
            v.append([float(x) for x in m.groups()[1:]])
    if not v:
        return None
    n = len(v)
    return [sum(c) / n for c in zip(*v)] + [n]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("logs", nargs="+", type=Path)
    ap.add_argument("--de", type=float, default=100)
    ap.add_argument("--ate", type=float, default=150)
    a = ap.parse_args()
    base = None
    for p in a.logs:
        r = medir(p, a.de, a.ate)
        if r is None:
            print(f"{p.name}: sem linhas [hle] entre {a.de} e {a.ate} s")
            continue
        fps, sinc, espera, nat, som, n = r
        base = base or som
        print(f"{p.name}: {fps:.1f} quadros/s · GPU nativo {nat:.0f} + sombra {som:.0f} ms/s (sombra ×{som / base:.3f}) · espera {espera:.0f} ms/s · sincronismo {sinc:.0f}% ({n} amostras)")


if __name__ == "__main__":
    main()
