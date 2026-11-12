"""Piloto em malha fechada: roda o jogo recompilado SEM janela e o guia olhando os quadros gravados.

    python pilot.py NOME [--secs 600] [--goal mom]

O jogo lê a entrada de `work/runs/NOME.in` (YW_INPUT_FILE, uma linha como `stick(0,1) a`), depois do roteiro fixo
até o quarto (`work/runs/route_prefix.txt`, sem a parte de andar). O piloto olha o minimapa da tela de baixo (seta do
jogador, pino do objetivo) e decide o próximo movimento. Nunca captura a área de trabalho: só lê os .rgba do jogo.
Trilha em work/runs/NOME.pilot.txt; quadros antigos são apagados (guarda 1 a cada 5 s).
"""
import argparse
import os
import subprocess
import time

import numpy as np
from scipy import ndimage

from paths import WORK, ROOT

EXE = WORK / "recomp" / "target" / "release" / "yw1r.exe"


def read_frame(d, t, scr):
    f = d / f"t{t:05d}_s{scr}_{'400' if scr == 0 else '320'}x240.rgba"
    try:
        a = np.fromfile(f, np.uint8)
        return a.reshape(240, 400 if scr == 0 else 320, 4)[..., :3].astype(int)
    except Exception:
        return None


def markers(bot):
    """(seta do jogador (x, y) ou None, pino do objetivo ou None) no minimapa (tela de baixo)."""
    m = (bot[..., 0] > 170) & (bot[..., 1] < 90) & (bot[..., 2] < 90)
    m[:30] = False
    lab, n = ndimage.label(m)
    arrow = pin = None
    for i in range(1, n + 1):
        ys, xs = np.nonzero(lab == i)
        if len(xs) < 20:
            continue
        r = bot[ys, xs, 0].mean()
        c = (float(xs.mean()), float(ys.mean()))
        if len(xs) >= 50 and r < 240:
            arrow = c
        elif r >= 245:
            pin = c
    return arrow, pin


def dialog_open(top):
    """Caixa de diálogo na parte de baixo da tela de cima (faixa clara translúcida)? Heurística: muita área clara."""
    if top is None:
        return False
    strip = top[170:225, 20:380]
    return float((strip.min(axis=2) > 200).mean()) > 0.35


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("name")
    ap.add_argument("--secs", type=float, default=600)
    ap.add_argument("--speed", type=float, default=1.0)
    a = ap.parse_args()
    out = WORK / "runs" / a.name
    out.mkdir(parents=True, exist_ok=True)
    for f in out.glob("*.rgba"):
        f.unlink()
    inp = WORK / "runs" / f"{a.name}.in"
    inp.write_text("")
    prefix = (WORK / "runs" / "route_prefix.txt").read_text().split(" 205:stick")[0].strip()
    logdir = WORK / "logs" / a.name
    logdir.mkdir(parents=True, exist_ok=True)
    for f in logdir.glob("*"):
        f.unlink()
    env = dict(os.environ)
    env.update(YW_LOG_DIR=str(logdir), YW_AUDIO="none", YW_DSP_STATS="0", YW_HEADLESS_SECS=str(a.secs / a.speed), YW_SPEED=str(a.speed),
               YW_SHOT_EVERY="0.1", YW_FRAMES_DIR=str(out), YW_RENDERER="vulkan", YW_INPUT=prefix,
               YW_INPUT_FILE=str(inp))
    log = open(WORK / "runs" / f"{a.name}.log", "w", encoding="utf8", errors="replace")
    proc = subprocess.Popen([str(EXE)], cwd=EXE.parent.parent.parent, env=env, stdout=log, stderr=subprocess.STDOUT)
    trace = open(WORK / "runs" / f"{a.name}.pilot.txt", "w", encoding="utf8")

    def say(s):
        trace.write(s + "\n")
        trace.flush()

    last_t, hist, mode, t_mode = -1, [], "espera", 0
    pin_seen = None
    tpl2f = None
    wp = [(175, 118), (177, 76), (192, 77), (192, 118)]  # 2F: vão da parede → norte → coluna da escada → descer
    wpi = 0
    stuck_at, stuck_since = None, 0
    off_since = None
    try:
        while proc.poll() is None:
            names = sorted(p.name for p in out.glob("t*_s1_320x240.rgba"))
            if not names:
                time.sleep(0.2)
                continue
            t = int(names[-1][1:6])
            # apaga o que ficou para trás (guarda 1 quadro a cada 5 s)
            for n in names[:-3]:
                tt = int(n[1:6])
                if tt % 50:
                    for s in (0, 1):
                        try:
                            (out / n.replace("_s1_320", f"_s{s}_{'400' if s == 0 else '320'}")).unlink()
                        except OSError:
                            pass
            if t == last_t:
                time.sleep(0.03)
                continue
            last_t = t
            bot = read_frame(out, t, 1)
            if bot is None:
                continue
            tsec = t / 10
            arrow, pin = markers(bot)
            if pin:
                pin_seen = pin
            if tsec < 203:
                continue
            # andar de cima × de baixo: compara a planta com a do quarto (primeiro quadro depois de 203 s)
            if arrow is None and tpl2f is None:
                continue  # ainda sem minimapa (cutscene/carregando)
            if tpl2f is None:
                tpl2f = bot[40:200, 60:260].copy()
                say(f"{tsec:.1f}s minimapa do 2F capturado")
            same2f = np.abs(bot[40:200, 60:260] - tpl2f).mean() < 14
            cmd = ""
            if arrow is None:
                cmd = ""
            elif mode in ("espera", "2F"):
                if same2f:
                    mode, off_since = "2F", None
                else:
                    off_since = off_since or tsec
                    mode = "1F" if tsec - off_since > 1.0 else mode
                if mode == "1F":
                    say(f"{tsec:.1f}s desceu para 1F")
                    wpi = 0
                    stuck_at = None
                    pin_seen = None
                    pin = None
                else:
                    tx, ty = wp[min(wpi, len(wp) - 1)]
                    dx, dy = tx - arrow[0], ty - arrow[1]
                    if abs(dx) < 6 and abs(dy) < 6 and wpi < len(wp) - 1:
                        wpi += 1
                        say(f"{tsec:.1f}s waypoint {wpi} alcançado em {arrow}")
                    sx = 0 if abs(dx) < 5 else (1 if dx > 0 else -1)
                    sy = 0 if abs(dy) < 5 else (-1 if dy > 0 else 1)  # tela: y cresce para baixo; stick y+ = cima
                    if wpi == len(wp) - 1 and abs(dy) < 6:
                        sy = -1  # último trecho: empurra para baixo, pela escada
                    cmd = f"stick({sx},{sy})" if (sx or sy) else ""
            if mode == "1F":
                tgt = pin_seen
                if tgt is None or arrow is None:
                    cmd = ""
                else:
                    dx, dy = tgt[0] - arrow[0], tgt[1] - arrow[1]
                    if abs(dx) < 10 and abs(dy) < 12:
                        cmd = "a" if int(tsec * 4) % 2 == 0 else ""
                        mode = "mãe"
                        t_mode = tsec
                    else:
                        # eixo x primeiro: o ponto de chegada fica embaixo da escada (subir = voltar ao 2º andar)
                        if abs(dx) >= 8:
                            sx, sy = (1 if dx > 0 else -1), 0
                        else:
                            sx = 0
                            sy = 0 if abs(dy) < 5 else (-1 if dy > 0 else 1)
                        cmd = f"stick({sx},{sy})"
            elif mode == "mãe":
                ph = tsec % 4
                if ph < 0.4 and arrow and pin_seen and not dialog_open(read_frame(out, t, 0)):
                    # vira para a mãe (e anda um passo) antes de apertar A
                    dx, dy = pin_seen[0] - arrow[0], pin_seen[1] - arrow[1]
                    cmd = f"stick({1 if dx > 3 else (-1 if dx < -3 else 0)},{-1 if dy > 3 else (1 if dy < -3 else 0)})"
                else:
                    cmd = "a" if int(tsec * 4) % 2 == 0 else ""
            # preso? (mesma posição por 1,5 s com movimento pedido) → tenta o outro eixo
            if arrow and cmd.startswith("stick"):
                if stuck_at and abs(arrow[0] - stuck_at[0]) < 1.5 and abs(arrow[1] - stuck_at[1]) < 1.5:
                    if tsec - stuck_since > 1.5:
                        say(f"{tsec:.1f}s preso em {arrow} com {cmd}")
                        stuck_since = tsec
                        sx, sy = [int(v) for v in cmd[6:-1].split(",")]
                        if sx and sy:
                            cmd = f"stick({sx},0)"
                        elif sy:
                            cmd = f"stick({-1 if int(tsec) % 2 else 1},{sy})"
                        else:
                            cmd = f"stick({sx},{1 if int(tsec) % 2 else -1})"
                else:
                    stuck_at, stuck_since = arrow, tsec
            inp.write_text(cmd)
            if int(tsec * 10) % 5 == 0:
                say(f"{tsec:.1f}s modo {mode} seta {arrow and tuple(round(v) for v in arrow)} pino "
                    f"{pin_seen and tuple(round(v) for v in pin_seen)} cmd {cmd!r}")
    finally:
        if proc.poll() is None:
            proc.terminate()
        trace.close()
        log.close()


if __name__ == "__main__":
    main()
