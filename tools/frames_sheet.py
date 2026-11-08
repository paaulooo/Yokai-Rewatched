"""Folha de contato das telas gravadas pelo jogo recompilado sem janela (YW_HEADLESS_SECS).

Lê work/recomp_frames/tNNNNN_s{0,1}_WxH.rgba (NNNNN = instante do console em décimos de segundo), empilha a tela de
cima e a de baixo de cada instante e monta uma grade reduzida com o tempo escrito, para ver o caminho do jogo de uma vez.

    python frames_sheet.py [--dir work/recomp_frames] [--from 20] [--to 60] [--step 2] [--cols 6] [--scale 0.5] [-o saida.png]
    python frames_sheet.py --at 41.5            # uma captura só, em tamanho real (cima + baixo)
"""
import argparse
import re
from pathlib import Path

from PIL import Image, ImageDraw

from paths import WORK

NAME = re.compile(r"t(\d+)_s([01])_(\d+)x(\d+)\.rgba$")


def load(dir_: Path):
    shots: dict[int, dict[int, Path]] = {}
    for p in dir_.iterdir():
        m = NAME.match(p.name)
        if m:
            shots.setdefault(int(m[1]), {})[int(m[2])] = p
    return shots


def screen(p: Path | None, w: int, h: int) -> Image.Image:
    if p is None:
        return Image.new("RGB", (w, h), (40, 0, 40))
    return Image.frombytes("RGBA", (w, h), p.read_bytes()).convert("RGB")


def stacked(s: dict[int, Path]) -> Image.Image:
    im = Image.new("RGB", (400, 480), (0, 0, 0))
    im.paste(screen(s.get(0), 400, 240), (0, 0))
    im.paste(screen(s.get(1), 320, 240), (40, 240))
    return im


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", type=Path, default=WORK / "recomp_frames")
    ap.add_argument("--from", dest="t0", type=float, default=0)
    ap.add_argument("--to", dest="t1", type=float, default=1e9)
    ap.add_argument("--step", type=float, default=1.0)
    ap.add_argument("--cols", type=int, default=6)
    ap.add_argument("--scale", type=float, default=0.5)
    ap.add_argument("--at", type=float)
    ap.add_argument("-o", type=Path)
    a = ap.parse_args()
    shots = load(a.dir)
    if not shots:
        raise SystemExit(f"nenhuma captura em {a.dir}")
    times = sorted(shots)
    if a.at is not None:
        t = min(times, key=lambda k: abs(k - a.at * 10))
        out = a.o or WORK / "img_png" / f"shot_{t:05d}.png"
        out.parent.mkdir(parents=True, exist_ok=True)
        stacked(shots[t]).save(out)
        print(out, f"(t={t / 10:.1f}s)")
        return
    picked, last = [], -1e9
    for t in times:
        if a.t0 * 10 <= t <= a.t1 * 10 and t - last >= a.step * 10 - 0.5:
            picked.append(t)
            last = t
    if not picked:
        raise SystemExit("nenhuma captura no intervalo")
    tw, th = int(400 * a.scale), int(480 * a.scale)
    rows = (len(picked) + a.cols - 1) // a.cols
    sheet = Image.new("RGB", (a.cols * tw, rows * (th + 14)), (20, 20, 20))
    d = ImageDraw.Draw(sheet)
    for i, t in enumerate(picked):
        x, y = (i % a.cols) * tw, (i // a.cols) * (th + 14)
        sheet.paste(stacked(shots[t]).resize((tw, th), Image.BILINEAR), (x, y + 14))
        d.text((x + 3, y + 1), f"{t / 10:.1f}s", fill=(255, 255, 0))
    out = a.o or WORK / "img_png" / f"sheet_{picked[0]:05d}_{picked[-1]:05d}.png"
    out.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(out)
    print(out, f"{len(picked)} instantes de {picked[0] / 10:.1f}s a {picked[-1] / 10:.1f}s")


if __name__ == "__main__":
    main()
