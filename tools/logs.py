"""Lê os logs das execuções do jogo (work/logs/).

    python logs.py                 lista as execuções (mais recentes primeiro) com um resumo
    python logs.py latest          resumo da última execução (meta, fps, travamento, avisos)
    python logs.py NOME|NNNN [arquivo] [--tail 40]   mostra um arquivo da execução (padrão: game.log)

Arquivos por execução: game.log (tudo) · perf.log · gfx.log · dsp.log · input.log · stall.log · meta.txt.
Execuções do jogo pela janela ficam em work/logs/NNNN; as de tools/recomp_run.py e tools/pilot.py, em work/logs/NOME.
"""
import argparse
import re
import sys

from paths import WORK

LOGS = WORK / "logs"


def runs():
    ds = [d for d in LOGS.iterdir() if d.is_dir()] if LOGS.exists() else []
    return sorted(ds, key=lambda d: (d / "meta.txt").stat().st_mtime if (d / "meta.txt").exists() else 0, reverse=True)


def summary(d):
    out = [f"== {d.name} =="]
    meta = d / "meta.txt"
    if meta.exists():
        first = meta.read_text(encoding="utf8", errors="replace").splitlines()[0]
        out.append("  " + first)
    perf = d / "perf.log"
    if perf.exists():
        fps = [float(m.group(1)) for l in perf.read_text(encoding="utf8", errors="replace").splitlines()
               for m in [re.search(r"([\d.]+) quadros/s", l)] if m]
        if fps:
            out.append(f"  quadros/s: mín {min(fps):.1f} · mediana {sorted(fps)[len(fps) // 2]:.1f} · máx {max(fps):.1f}"
                       f" ({len(fps)} amostras de 5 s)")
    stall = d / "stall.log"
    out.append("  TRAVAMENTO: sim — veja stall.log" if stall.exists() and "TRAVAMENTO" in
               stall.read_text(encoding="utf8", errors="replace") else "  travamento: não")
    game = d / "game.log"
    if game.exists():
        bad = [l for l in game.read_text(encoding="utf8", errors="replace").splitlines()
               if re.search(r"panic|parou|erro|falha|ignorado|não mapeado|sem AOT", l, re.I) and not l.startswith("[dsp]")]
        out.append(f"  avisos/erros: {len(bad)}")
        out += ["    " + l[:160] for l in bad[:6]]
    return "\n".join(out)


def find(name):
    for d in runs():
        if d.name == name or d.name.lstrip("0") == name.lstrip("0"):
            return d
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run", nargs="?")
    ap.add_argument("file", nargs="?", default=None)
    ap.add_argument("--tail", type=int, default=0)
    a = ap.parse_args()
    if not a.run:
        for d in runs()[:15]:
            print(summary(d))
        return
    d = runs()[0] if a.run == "latest" else find(a.run)
    if d is None:
        sys.exit(f"execução {a.run!r} não encontrada em {LOGS}")
    if a.file is None and a.run == "latest":
        print(summary(d))
        return
    f = d / (a.file or "game.log")
    if not f.exists():
        sys.exit(f"{f} não existe (arquivos: {', '.join(p.name for p in d.iterdir())})")
    lines = f.read_text(encoding="utf8", errors="replace").splitlines()
    print("\n".join(lines[-a.tail:] if a.tail else lines))


if __name__ == "__main__":
    main()
