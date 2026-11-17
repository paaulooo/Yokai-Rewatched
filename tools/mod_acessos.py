"""Tabela dos arquivos que o jogo abriu, a partir dos ganchos de log (YW_HOOK_LOG) de uma rodada.

    YW_HOOK_LOG_MAX=100000 YW_HOOK_LOG="$(python mod_acessos.py --spec)" python recomp_run.py NOME --secs 205 --input "…"
    python mod_acessos.py NOME            # -> work/mods/acessos.csv (+ resumo por função e extensão)

Colunas: caminho, funcao (0x47359c abrir · 0x47365c abrir+descomprimir · 0x473728 existe? · 0x47397c/0x473a38 arquivo
solto · 0x1db8c0/0x1dbaa8 busca nos .fa montados), tamanho (*tam na volta), buffer, retorno (r0), vblank.
"""
import argparse
import collections
import csv
import re
import sys

from paths import WORK

FUNCS = {
    "0x47359c": "abrir",
    "0x47365c": "abrir+descomprimir",
    "0x473728": "existe?",
    "0x47397c": "solto (abrir)",
    "0x473a38": "solto (descomprimir)",
    "0x1db8c0": "fa: busca",
    "0x1dbaa8": "fa: busca+descomprimir",
}
SPEC = ("0x47359c:depois:s1:x2+0/4:x3+0/4:x13+0/8; 0x47365c:depois:s1:x2+0/4:x3+0/4:x13+0/8; 0x473728:depois:s1; "
        "0x1db8c0:depois:s1:x2+0/4:x3+0/4; 0x1dbaa8:depois:s1:x2+0/4:x3+0/4; 0x47397c:depois:s1:x2+0/4:x3+0/4; "
        "0x473a38:depois:s1:x2+0/4:x3+0/4")
LINHA = re.compile(r'\[gancho-log\] (0x[0-9a-f]+) depois q=(\d+) r0=(0x[0-9a-f]+).*?s1="([^"]*)"(.*)')
PALAVRA = re.compile(r'x(\d)\+0/4@0x[0-9a-f]+: ([0-9a-f]{8})')


def le32(h):
    return int.from_bytes(bytes.fromhex(h), "little")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("nome", nargs="?")
    ap.add_argument("--spec", action="store_true", help="imprime o YW_HOOK_LOG a usar")
    ap.add_argument("-o", default=str(WORK / "mods" / "acessos.csv"))
    a = ap.parse_args()
    if a.spec or not a.nome:
        print(SPEC)
        return
    log = WORK / "runs" / f"{a.nome}.log"
    linhas = []
    for l in log.read_text(encoding="utf8", errors="replace").splitlines():
        m = LINHA.search(l)
        if not m:
            continue
        va, q, r0, caminho, resto = m.groups()
        w = {int(i): le32(h) for i, h in PALAVRA.findall(resto)}
        linhas.append(dict(caminho=caminho, funcao=f"{va} {FUNCS.get(va, '?')}", tamanho=w.get(3, ""),
                           buffer=f"{w[2]:#x}" if 2 in w else "", retorno=int(r0, 16), vblank=int(q)))
    out = WORK / "mods"
    out.mkdir(parents=True, exist_ok=True)
    with open(a.o, "w", newline="", encoding="utf8") as f:
        wr = csv.DictWriter(f, fieldnames=["caminho", "funcao", "tamanho", "buffer", "retorno", "vblank"])
        wr.writeheader()
        wr.writerows(linhas)
    print(f"{len(linhas)} linhas -> {a.o}")
    por_f = collections.Counter(l["funcao"] for l in linhas)
    for k, v in sorted(por_f.items()):
        print(f"  {k:32} {v}")
    ext = collections.defaultdict(collections.Counter)
    for l in linhas:
        c = l["caminho"]
        e = c.rsplit(".", 1)[-1] if "." in c.rsplit("/", 1)[-1] else "(sem)"
        ext[l["funcao"]][e] += 1
    for k in sorted(ext):
        print(f"  {k}: " + ", ".join(f"{e} {n}" for e, n in ext[k].most_common()))


if __name__ == "__main__":
    sys.exit(main())
