"""Compara duas imagens de tela (Azahar x recompilação) e converte .rgba cru em PNG.

Entradas aceitas: qualquer imagem que o pillow abra (PNG...) ou .rgba cru com o tamanho no nome
(`t001_s0_400x240.rgba`, formato gravado por engine/src/hle/mod.rs::run_headless em work/recomp_frames/:
RGBA 8 bits por canal, linhas de cima para baixo, sem cabeçalho).

  python img_diff.py diff A B [--tol 8] [--align 2] [--out work/img_diff/x.png] [--crop-a x,y,l,a] [--crop-b ...]
  python img_diff.py png ARQ.rgba [...] [--out-dir DIR]      # .rgba -> PNG (padrão work/img_png/)

Métricas (só RGB; alfa ignorado a menos que --alpha):
  iguais%  = pixels cujo |a-b| <= tol em TODOS os canais
  PSNR     = 10*log10(255^2 / MSE) em dB (inf = idênticas)
Tamanhos diferentes: a maior é reduzida para o tamanho da menor (média por bloco quando o fator é inteiro,
ex. Azahar em escala 2/3; senão bilinear) — ou --resize a|b|none. --align N procura o deslocamento inteiro
(-N..N em x e y) com mais pixels iguais (útil se um lado tiver borda/offset).
Imagem de diferença: [A | B | mapa] lado a lado; no mapa, pixel igual = A em cinza escurecido,
pixel diferente = vermelho com intensidade proporcional à diferença.
"""
import argparse
import os
import re
import sys
from pathlib import Path

import numpy as np
from PIL import Image

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import WORK  # noqa: E402

RGBA_NAME = re.compile(r"(\d+)x(\d+)\.rgba$", re.I)


def load(path, size=None):
    """Devolve array HxWx4 uint8."""
    path = Path(path)
    if path.suffix.lower() == ".rgba":
        if size is None:
            m = RGBA_NAME.search(path.name)
            if not m:
                sys.exit(f"ERRO: {path.name}: .rgba sem 'LxA' no nome; passe --size LxA")
            size = (int(m.group(1)), int(m.group(2)))
        w, h = size
        raw = np.fromfile(path, dtype=np.uint8)
        if raw.size != w * h * 4:
            sys.exit(f"ERRO: {path.name}: {raw.size} bytes, esperado {w}x{h}x4 = {w*h*4}")
        return raw.reshape(h, w, 4)
    return np.asarray(Image.open(path).convert("RGBA"))


def parse_rect(t):
    v = [int(x, 0) for x in t.split(",")]
    if len(v) != 4:
        raise argparse.ArgumentTypeError("retângulo = x,y,l,a")
    return v


def parse_size(t):
    w, h = t.lower().split("x")
    return int(w), int(h)


def crop(a, r):
    if r is None:
        return a
    x, y, w, h = r
    return a[y:y + h, x:x + w]


def resize_to(a, w, h):
    ah, aw = a.shape[:2]
    if (aw, ah) == (w, h):
        return a
    if aw % w == 0 and ah % h == 0 and aw // w == ah // h:   # fator inteiro: média por bloco (exata p/ escala do Azahar)
        f = aw // w
        return a.reshape(h, f, w, f, 4).astype(np.float32).mean(axis=(1, 3)).round().astype(np.uint8)
    return np.asarray(Image.fromarray(a, "RGBA").resize((w, h), Image.BILINEAR))


def metrics(a, b, tol, use_alpha):
    ch = 4 if use_alpha else 3
    d = np.abs(a[..., :ch].astype(np.int16) - b[..., :ch].astype(np.int16))
    same = (d.max(axis=2) <= tol)
    mse = float((d.astype(np.float64) ** 2).mean())
    psnr = float("inf") if mse == 0 else 10 * np.log10(255.0 ** 2 / mse)
    return same, d, mse, psnr


def best_offset(a, b, n, tol, use_alpha):
    """Procura (dx,dy) em [-n,n] que maximiza pixels iguais na área comum. Devolve (dx, dy, a_recortada, b_recortada)."""
    h, w = a.shape[:2]
    best = None
    for dy in range(-n, n + 1):
        for dx in range(-n, n + 1):
            aa = a[max(0, dy):h + min(0, dy), max(0, dx):w + min(0, dx)]
            bb = b[max(0, -dy):h + min(0, -dy), max(0, -dx):w + min(0, -dx)]
            s = metrics(aa, bb, tol, use_alpha)[0].mean()
            if best is None or s > best[0]:
                best = (s, dx, dy, aa, bb)
    return best[1:]


def diff_image(a, b, same, d):
    gray = (a[..., :3].astype(np.float32) @ np.array([0.299, 0.587, 0.114], np.float32)) * 0.35
    m = np.repeat(gray[..., None], 3, axis=2)
    mag = d[..., :3].max(axis=2).astype(np.float32)
    red = np.zeros_like(m); red[..., 0] = 96 + mag * (159 / 255.0)
    m = np.where(same[..., None], m, red).clip(0, 255).astype(np.uint8)
    h, w = a.shape[:2]
    sep = np.full((h, 2, 3), 255, np.uint8)
    return np.concatenate([a[..., :3], sep, b[..., :3], sep, m], axis=1)


def cmd_diff(args):
    a = crop(load(args.a, args.size_a), args.crop_a)
    b = crop(load(args.b, args.size_b), args.crop_b)
    sa, sb = a.shape[1::-1], b.shape[1::-1]
    if sa != sb:
        mode = args.resize
        if mode == "none":
            sys.exit(f"ERRO: tamanhos diferentes {sa} x {sb} e --resize none")
        if mode == "smaller":
            mode = "b" if sb[0] * sb[1] > sa[0] * sa[1] else "a"
        if mode == "b":
            b = resize_to(b, *sa)
        else:
            a = resize_to(a, *sb)
        print(f"tamanhos diferentes: A {sa[0]}x{sa[1]}, B {sb[0]}x{sb[1]} -> redimensionei {mode.upper()} para "
              f"{a.shape[1]}x{a.shape[0]}")
    dx = dy = 0
    if args.align:
        dx, dy, a, b = best_offset(a, b, args.align, args.tol, args.alpha)
        # com (dx,dy) achado, B[y-dy, x-dx] casa com A[y, x] => o conteúdo de B está deslocado (-dx, -dy) em relação a A
        print(f"alinhamento: conteúdo de B deslocado ({-dx:+d}, {-dy:+d}) px em relação a A; área comum {a.shape[1]}x{a.shape[0]} "
              f"(coordenadas abaixo são nessa área comum)")
    same, d, mse, psnr = metrics(a, b, args.tol, args.alpha)
    n = same.size
    eq = int(same.sum())
    exact = int((d.max(axis=2) == 0).sum())
    print(f"A: {args.a}\nB: {args.b}")
    print(f"pixels: {n}  iguais(tol={args.tol}): {eq} = {100.0*eq/n:.2f}%  exatos: {100.0*exact/n:.2f}%")
    print(f"MSE={mse:.2f}  PSNR={'inf' if psnr == float('inf') else f'{psnr:.2f}'} dB  "
          f"dif. máx={int(d.max())}  dif. média={float(d.mean()):.2f}")
    if eq != n:
        ys, xs = np.nonzero(~same)
        print(f"caixa das diferenças: x {xs.min()}..{xs.max()}, y {ys.min()}..{ys.max()}")
    out = Path(args.out) if args.out else WORK / "img_diff" / f"{Path(args.a).stem}__vs__{Path(args.b).stem}.png"
    out.parent.mkdir(parents=True, exist_ok=True)
    Image.fromarray(diff_image(a, b, same, d), "RGB").save(out)
    print(f"imagem de diferença [A | B | mapa]: {out}")
    if args.min_equal is not None and 100.0 * eq / n < args.min_equal:
        sys.exit(2)   # útil em script: falha se ficou abaixo do limiar


def cmd_png(args):
    out_dir = Path(args.out_dir) if args.out_dir else WORK / "img_png"
    out_dir.mkdir(parents=True, exist_ok=True)
    for f in args.files:
        a = load(f, args.size)
        mode = "RGBA" if args.keep_alpha else "RGB"
        img = Image.fromarray(a if args.keep_alpha else a[..., :3], mode)
        p = out_dir / (Path(f).stem + ".png")
        img.save(p)
        al = a[..., 3]
        print(f"{p}  {a.shape[1]}x{a.shape[0]}  alfa min/max={al.min()}/{al.max()}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("diff", help="compara A e B")
    p.add_argument("a"); p.add_argument("b")
    p.add_argument("--tol", type=int, default=0, help="diferença máxima por canal para contar como igual (padrão 0)")
    p.add_argument("--alpha", action="store_true", help="compara também o canal alfa")
    p.add_argument("--resize", choices=("smaller", "a", "b", "none"), default="smaller", help="quem redimensionar se diferirem")
    p.add_argument("--align", type=int, default=0, metavar="N", help="procura deslocamento em -N..N px")
    p.add_argument("--crop-a", type=parse_rect); p.add_argument("--crop-b", type=parse_rect)
    p.add_argument("--size-a", type=parse_size, help="LxA de um .rgba sem tamanho no nome")
    p.add_argument("--size-b", type=parse_size)
    p.add_argument("--out", help="PNG de saída (padrão work/img_diff/A__vs__B.png)")
    p.add_argument("--min-equal", type=float, help="sai com código 2 se iguais%% < este valor")
    p.set_defaults(fn=cmd_diff)
    p = sub.add_parser("png", help=".rgba (ou outra imagem) -> PNG")
    p.add_argument("files", nargs="+"); p.add_argument("--out-dir"); p.add_argument("--size", type=parse_size)
    p.add_argument("--keep-alpha", action="store_true", help="mantém o alfa (padrão: descarta, o framebuffer do 3DS não usa)")
    p.set_defaults(fn=cmd_png)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
