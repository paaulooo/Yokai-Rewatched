"""Lançador e leitor das fotos de função (gdb/fn_snapshot.gdb) — base do teste diferencial da recompilação.

  python fn_snapshot.py run 0x329bc8 [--count 3] [--skip 0] [--stack-pages 2] [--ptr-pages 1] [--extra 0xADDR:0xTAM,...]
         [--target :24689]             # roda o gdb-multiarch contra o GDB stub do Azahar (jogo rodando, stub ligado)
  python fn_snapshot.py list            # funções/fotos já gravadas em work/fnsnap/
  python fn_snapshot.py show 0x329bc8 [hit]   # registradores entrada x saída + quais páginas mudaram
  python fn_snapshot.py selftest        # testa o .gdb sem Azahar (YW_FNSNAP_DRYRUN=1)

O gdb-multiarch vem do MSYS2 UCRT64; este script procura no PATH e em C:/msys64/ucrt64/bin (YW_GDB sobrescreve).
Pode rodar do PowerShell (chama o .exe direto) ou do MSYS2.
Formato dos arquivos: docs/ENVIRONMENT.md, seção "Comparação com o Azahar (recompilação)".
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import ROOT, WORK  # noqa: E402

GDB_SCRIPT = ROOT / "gdb" / "fn_snapshot.gdb"
OUT = Path(os.environ.get("YW_FNSNAP_OUT", WORK / "fnsnap"))


def find_gdb():
    for c in (os.environ.get("YW_GDB"), shutil.which("gdb-multiarch"), r"C:/msys64/ucrt64/bin/gdb-multiarch.exe"):
        if c and Path(c).exists():
            return c
    sys.exit("ERRO: gdb-multiarch não encontrado (instale no MSYS2 UCRT64: pacman -S mingw-w64-ucrt-x86_64-gdb-multiarch, "
             "ou defina YW_GDB)")


def run_gdb(env_extra, ex=(), batch=True):
    env = dict(os.environ, YW_FNSNAP_OUT=str(OUT), **env_extra)
    cmd = [find_gdb(), "-q"] + (["-batch"] if batch else [])
    for e in ex:
        cmd += ["-ex", e]
    cmd += ["-x", str(GDB_SCRIPT)]
    print("$", " ".join(cmd), flush=True)
    return subprocess.call(cmd, env=env)


def cmd_run(a):
    env = {"YW_FN_ADDR": hex(a.va), "YW_FN_COUNT": str(a.count), "YW_FN_SKIP": str(a.skip),
           "YW_FN_STACK_PAGES": str(a.stack_pages), "YW_FN_PTR_PAGES": str(a.ptr_pages),
           "YW_GDB_TARGET": a.target}
    if a.extra:
        env["YW_FN_EXTRA"] = a.extra
    sys.exit(run_gdb(env))


def cmd_selftest(a):
    sys.exit(run_gdb({"YW_FNSNAP_DRYRUN": "1"}))


def load(p):
    with open(p, encoding="utf-8") as f:
        return json.load(f)


def cmd_list(a):
    if not OUT.exists():
        print(f"(nada em {OUT})"); return
    for d in sorted(OUT.iterdir()):
        idx = d / "index.json"
        if d.name.startswith("_") or not idx.exists():
            continue
        j = load(idx)
        print(f"{j['va']}: {j['complete']} chamada(s) completa(s), abertas={j['open_calls']}  ({d})")
        for h in j["hits"]:
            print(f"   #{h['hit']:03d} chamada {h['call_no']}: r0 {h['r0_in']} -> {h['r0_out']}")


def cmd_show(a):
    d = OUT / f"{a.va:08x}" / f"{a.hit:03d}"
    if not (d / "entry.json").exists():
        sys.exit(f"não há {d / 'entry.json'}")
    en = load(d / "entry.json")
    ex = load(d / "exit.json") if (d / "exit.json").exists() else None
    print(f"{en['va']} foto #{a.hit} (chamada {en['call_no']}), volta para {en.get('return_to')}")
    for k, v in en["regs"].items():
        w = ex["regs"].get(k) if ex else None
        print(f"  {k:5s} {v}" + (f" -> {w}" if w and w != v else ""))
    if ex and en["vfp"]["s"] and ex["vfp"]["s"]:
        ch = [f"s{i}: {x}->{y}" for i, (x, y) in enumerate(zip(en["vfp"]["s"], ex["vfp"]["s"])) if x != y]
        print("  vfp: " + (", ".join(ch) if ch else "sem mudança") + f"; fpscr {en['vfp']['fpscr']} -> {ex['vfp']['fpscr']}")
    if ex:   # páginas que mudaram entre entrada e saída (mesmo endereço)
        def pages(j, kind):
            out = {}
            for m in j["mem"]:
                b = (d / m["file"]).read_bytes(); base = int(m["addr"], 16)
                for o in range(0, len(b), 0x1000):
                    out[base + o] = b[o:o + 0x1000]
            return out
        pe, px = pages(en, "entry"), pages(ex, "exit")
        for p in sorted(set(pe) & set(px)):
            n = sum(1 for x, y in zip(pe[p], px[p]) if x != y)
            if n:
                first = next(i for i, (x, y) in enumerate(zip(pe[p], px[p])) if x != y)
                print(f"  página {p:08x}: {n} bytes mudaram (primeiro em {p + first:08x})")
    else:
        print("  (sem exit.json: a chamada não retornou durante a captura)")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("run"); p.add_argument("va", type=lambda s: int(s, 0))
    p.add_argument("--count", type=int, default=1); p.add_argument("--skip", type=int, default=0)
    p.add_argument("--stack-pages", type=int, default=2); p.add_argument("--ptr-pages", type=int, default=1)
    p.add_argument("--extra", help="faixas extras 0xADDR:0xTAM,..."); p.add_argument("--target", default=":24689")
    p.set_defaults(fn=cmd_run)
    sub.add_parser("selftest").set_defaults(fn=cmd_selftest)
    sub.add_parser("list").set_defaults(fn=cmd_list)
    p = sub.add_parser("show"); p.add_argument("va", type=lambda s: int(s, 0)); p.add_argument("hit", type=int, nargs="?", default=0)
    p.set_defaults(fn=cmd_show)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
