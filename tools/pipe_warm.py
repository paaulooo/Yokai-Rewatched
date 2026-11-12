"""Enche a lista de pipelines do Vulkan (pré-compilação no boot) rodando o jogo recompilado sem janela pela rota
título → quarto, uma vez em cada combinação de opções que muda as chaves dos pipelines.

    python pipe_warm.py [--secs 200] [--saves PASTA] [--only base,fxaa3,interp3,aniso16] [--dry]

Rodadas (em sequência; a GPU é uma só):
  base     1× (YW_SCALE=1, sem FXAA, sem interpolação, sem anisotrópico)
  fxaa3    YW_SCALE=3 + YW_FXAA=1                  (sombra 3×: chaves de profundidade rápida, FXAA)
  interp3  YW_SCALE=3 + YW_INTERP=120              (replay da interpolação)
  aniso16  YW_ANISO=16                              (bits do filtro anisotrópico no fragmento especializado)

A lista fica em %APPDATA%/yw1/vk_pipekeys_<hash>.bin (o motor grava; YW_VK_PIPEKEYS sobrescreve) e, como o cache do
driver, é DERIVADA dos shaders do jogo: nunca vai para o repositório. Depois de cada rodada o script mostra quantas
chaves a lista tem e as linhas de resumo do Vulkan no log.

Entrada: o roteiro work/runs/route_probe4.txt cortado antes de " 205:stick" (só a parte com A/toque, até o quarto).
--saves PASTA usa YW_SAVES=PASTA (não toca no save do usuário). As configurações do usuário não são lidas: cada
rodada usa um settings.toml vazio em work/runs/pipe_warm/ (YW_SETTINGS_TOML) e as variáveis acima mandam.
Logs em work/runs/pipe_warm_<rodada>.log.
"""
import argparse
import glob
import os
import struct
import subprocess
import sys

from paths import WORK

EXE = WORK / "recomp" / "target" / "release" / "yw1r.exe"
ROUTE = WORK / "runs" / "route_probe4.txt"

# opções de render que valem em todas as rodadas (o ambiente manda sobre o settings.toml)
OFF = {"YW_SCALE": "1", "YW_FXAA": "0", "YW_INTERP": "0", "YW_ANISO": "0"}
RUNS = [
    ("base", {}),
    ("fxaa3", {"YW_SCALE": "3", "YW_FXAA": "1"}),
    ("interp3", {"YW_SCALE": "3", "YW_INTERP": "120"}),
    ("aniso16", {"YW_ANISO": "16"}),
]


def route():
    s = ROUTE.read_text(encoding="utf8").strip()
    i = s.find(" 205:stick")
    if i >= 0:
        s = s[:i]
    return s.rstrip().rstrip(";").strip()


def keys_file():
    """A lista em uso: YW_VK_PIPEKEYS, senão o vk_pipekeys_*.bin mais novo em %APPDATA%/yw1."""
    p = os.environ.get("YW_VK_PIPEKEYS")
    if p and p not in ("0", "off"):
        return p
    files = glob.glob(os.path.join(os.environ.get("APPDATA", "."), "yw1", "vk_pipekeys_*.bin"))
    return max(files, key=os.path.getmtime) if files else None


def count_keys(path):
    """Nº de chaves pelo cabeçalho (magic YWPK, versão, hash SPIR-V, hash AOT, nº AOT, nº de chaves)."""
    if not path or not os.path.exists(path):
        return None
    with open(path, "rb") as f:
        h = f.read(32)
    if len(h) < 32 or h[:4] != b"YWPK":
        return None
    ver, _spv, _aot, naot, n = struct.unpack("<IQQII", h[4:32])
    return n, ver, naot


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--secs", type=float, default=200, help="segundos de console por rodada (padrão 200)")
    ap.add_argument("--saves", default="", help="pasta de saves (YW_SAVES) para não tocar no save do usuário")
    ap.add_argument("--only", default="", help="rodadas separadas por vírgula (padrão: todas)")
    ap.add_argument("--dry", action="store_true", help="só mostra o que rodaria")
    a = ap.parse_args()
    if not EXE.exists():
        sys.exit(f"falta {EXE} (cd work/recomp && cargo build --release -p yw1-recomp-app)")
    want = [r for r in a.only.split(",") if r]
    runs = [r for r in RUNS if not want or r[0] in want]
    out = WORK / "runs" / "pipe_warm"
    out.mkdir(parents=True, exist_ok=True)
    settings = out / "settings.toml"
    if not settings.exists():
        settings.write_text("", encoding="utf8")
    inp = route()
    print(f"lista antes: {count_keys(keys_file())} ({keys_file()})")
    for name, opts in runs:
        env = dict(os.environ)
        env.update(OFF)
        env.update(opts)
        env.update(
            YW_AUDIO="none",
            YW_DSP_STATS="0",
            YW_RENDERER="vulkan",
            YW_HEADLESS_SECS=str(a.secs),
            YW_INPUT=inp,
            YW_SETTINGS_TOML=str(settings),
            YW_LOG_DIR=str(WORK / "logs" / f"pipe_warm_{name}"),
        )
        env.pop("YW_VK_PREWARM", None)
        if a.saves:
            os.makedirs(a.saves, exist_ok=True)
            env["YW_SAVES"] = os.path.abspath(a.saves)
        desc = " ".join(f"{k}={v}" for k, v in {**OFF, **opts}.items())
        log = WORK / "runs" / f"pipe_warm_{name}.log"
        print(f"\n== {name}: {desc} · {a.secs:.0f} s · log {log}")
        if a.dry:
            continue
        with open(log, "w", encoding="utf8", errors="replace") as f:
            r = subprocess.run([str(EXE)], cwd=EXE.parent.parent.parent, env=env, stdout=f, stderr=subprocess.STDOUT,
                               timeout=a.secs + 120)
        lines = log.read_text(encoding="utf8", errors="replace").splitlines()
        for l in lines:
            if ("lista de pipelines" in l or "pré-compila" in l or "travada de" in l
                    or l.startswith("[hle] vulkan: compilações") or l.startswith("[hle] vulkan: draws")):
                print("  " + l)
        c = count_keys(keys_file())
        print(f"  saída {r.returncode} · chaves na lista: {c[0] if c else '—'} ({keys_file()})")


if __name__ == "__main__":
    main()
