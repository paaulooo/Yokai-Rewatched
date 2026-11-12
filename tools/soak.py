"""Teste de estresse: roda o trecho título → parque → quarto várias vezes (em paralelo) SEM janela e diz em quais
execuções o jogo travou (o vigia grava stall.log).

    python soak.py [--runs 12] [--parallel 3] [--secs 190] [--name soak] [--env YW_X=1 ...]

Cada execução tem a sua pasta work/logs/NAME-NN (ver logs.py). Não grava quadros.
"""
import argparse
import os
import subprocess
import sys
import time

from paths import WORK

EXE = WORK / "recomp" / "target" / "release" / "yw1r.exe"


def launch(name, secs, prefix, extra):
    logdir = WORK / "logs" / name
    logdir.mkdir(parents=True, exist_ok=True)
    for f in logdir.glob("*"):
        f.unlink()
    frames = WORK / "runs" / name
    frames.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update(YW_AUDIO="none", YW_DSP_STATS="0", YW_HEADLESS_SECS=str(secs), YW_SHOT_EVERY="100000",
               YW_FRAMES_DIR=str(frames), YW_RENDERER="vulkan", YW_INPUT=prefix, YW_LOG_DIR=str(logdir),
               YW_VK_PIPELINE_CACHE="0")
    env.update(extra)
    return subprocess.Popen([str(EXE)], cwd=EXE.parent.parent.parent, env=env, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL), logdir


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=12)
    ap.add_argument("--parallel", type=int, default=3)
    ap.add_argument("--secs", type=float, default=190)
    ap.add_argument("--name", default="soak")
    ap.add_argument("--env", nargs="*", default=[])
    a = ap.parse_args()
    extra = dict(e.split("=", 1) for e in a.env)
    prefix = (WORK / "runs" / "route_prefix.txt").read_text().split(" 205:stick")[0].strip()
    todo = list(range(1, a.runs + 1))
    live, done = [], []
    while todo or live:
        while todo and len(live) < a.parallel:
            n = todo.pop(0)
            p, d = launch(f"{a.name}-{n:02d}", a.secs, prefix, extra)
            live.append((n, p, d))
        time.sleep(2)
        for item in list(live):
            n, p, d = item
            if p.poll() is not None:
                live.remove(item)
                stall = d / "stall.log"
                hung = stall.exists() and "TRAVAMENTO" in stall.read_text(encoding="utf8", errors="replace")
                done.append((n, hung))
                print(f"execução {n:02d}: {'TRAVOU' if hung else 'ok'}  ({d})", flush=True)
    bad = [n for n, h in done if h]
    print(f"\n{len(bad)} travamento(s) em {len(done)} execuções: {sorted(bad)}")


if __name__ == "__main__":
    main()
