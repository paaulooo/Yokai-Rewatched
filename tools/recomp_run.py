"""Roda o jogo recompilado sem janela com um roteiro de entrada e monta a folha de contato das telas.

    python recomp_run.py NOME --secs 100 [--input "20:start; +8:a"] [--every 1] [--step 3] [--speed 3]

--secs e os tempos do roteiro são do relógio do console; --speed acelera o console inteiro (só testes; padrão 1).

Saídas em work/runs/NOME/ (capturas .rgba), work/runs/NOME.log (log do jogo) e work/img_png/sheet_*.png.
O roteiro (`YW_INPUT`) está descrito em engine/src/hle/script.rs. Nunca captura a área de trabalho.
"""
import argparse
import os
import subprocess
import sys

from paths import ROOT, WORK

EXE = WORK / "recomp" / "target" / "release" / "yw1r.exe"
NOISE = ("[ipc] srv:GetServiceHandle", "[fs] RomFS", "[dsp] On")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("name")
    ap.add_argument("--secs", type=float, default=60)
    ap.add_argument("--input", default="")
    ap.add_argument("--every", type=float, default=1.0)
    ap.add_argument("--step", type=float, default=3.0)
    ap.add_argument("--cols", type=int, default=8)
    ap.add_argument("--renderer", default="vulkan")
    ap.add_argument("--tail", type=int, default=30)
    ap.add_argument("--speed", type=float, default=1.0)
    a = ap.parse_args()
    out = WORK / "runs" / a.name
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.update(
        YW_AUDIO="none",
        YW_DSP_STATS="0",
        YW_HEADLESS_SECS=str(a.secs / a.speed),
        YW_SPEED=str(a.speed),
        YW_SHOT_EVERY=str(a.every),
        YW_FRAMES_DIR=str(out),
        YW_RENDERER=a.renderer,
    )
    if a.input:
        env["YW_INPUT"] = a.input
    log = WORK / "runs" / f"{a.name}.log"
    with open(log, "w", encoding="utf8", errors="replace") as f:
        r = subprocess.run([str(EXE)], cwd=EXE.parent.parent.parent, env=env, stdout=f, stderr=subprocess.STDOUT,
                           timeout=a.secs / a.speed + 90)
    lines = log.read_text(encoding="utf8", errors="replace").splitlines()
    keep = [l for l in lines if not l.startswith(NOISE) and not l.startswith("  t")]
    print(f"saída {r.returncode} · log {log}")
    print("\n".join(keep[-a.tail:]))
    subprocess.run([sys.executable, str(ROOT / "tools" / "frames_sheet.py"), "--dir", str(out), "--step", str(a.step),
                    "--cols", str(a.cols), "--scale", "0.35", "-o", str(WORK / "img_png" / f"sheet_{a.name}.png")])


if __name__ == "__main__":
    main()
