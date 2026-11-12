"""Análise do perfil de GPU por draw (YW_VK_DRAWPROF=arquivo.csv no yw1r).

Custo incremental de um draw = BOTTOM_OF_PIPE dele − BOTTOM_OF_PIPE do draw anterior no mesmo buffer de comandos e lote
(os draws se sobrepõem na GPU; o 1º do buffer usa o próprio TOP_OF_PIPE). Ajusta custo ≈ a + b·vs + c·fs por buffer
(0 = nativo, 1 = sombra N×) e quebra por características (aniso, filtro, profundidade...).

    python drawprof.py ../work/prof.csv [--from 0.8]   # usa só o último 20% dos lotes
"""
import argparse
import numpy as np

ap = argparse.ArgumentParser()
ap.add_argument("csv")
ap.add_argument("--from", dest="frm", type=float, default=0.8, help="fração inicial dos lotes a descartar")
a = ap.parse_args()

d = np.genfromtxt(a.csv, delimiter=",", names=True, dtype=None, encoding="utf-8")
b0 = d["batch"].min() + (d["batch"].max() - d["batch"].min()) * a.frm
d = d[d["batch"] >= b0]
nb = len(np.unique(d["batch"]))

def inc(rows):
    out = np.zeros(len(rows))
    order = np.lexsort((rows["idx"], rows["cb"], rows["batch"]))
    r = rows[order]
    prev_key = None
    prev_bot = 0.0
    for i, x in enumerate(r):
        key = (x["batch"], x["cb"])
        if key != prev_key:
            out[order[i]] = max(x["t_bot_us"] - x["t_top_us"], 0)
        else:
            out[order[i]] = max(x["t_bot_us"] - prev_bot, 0)
        prev_key, prev_bot = key, x["t_bot_us"]
    return out

c = inc(d)
print(f"{len(d)} draws em {nb} lotes")
for cb in (0, 1):
    m = d["cb"] == cb
    if not m.any():
        continue
    x = d[m]
    y = c[m]
    tot = y.sum()
    print(f"\n=== buffer {'nativo' if cb == 0 else 'sombra'} (n={x['ns'][0]}): {m.sum()} draws, {tot/1000:.1f} ms total, {tot/nb:.0f} µs/lote, "
          f"{y.mean():.1f} µs/draw (mediana {np.median(y):.1f})")
    print(f"   VS invocações/draw {x['vs_inv'].mean():.0f} (prims {x['prims'].mean():.0f}) · FS invocações/draw {x['fs_inv'].mean():.0f}")
    A = np.column_stack([np.ones(len(x)), x["vs_inv"], x["fs_inv"]])
    coef, *_ = np.linalg.lstsq(A, y, rcond=None)
    pred = A @ coef
    r2 = 1 - ((y - pred) ** 2).sum() / ((y - y.mean()) ** 2).sum()
    print(f"   ajuste: {coef[0]:.2f} µs fixo + {coef[1]*1000:.2f} ns/vértice + {coef[2]*1000:.3f} ns/fragmento (R²={r2:.2f})")
    print(f"   parcelas: fixo {coef[0]*len(x)/tot*100:.0f}% · vértice {coef[1]*x['vs_inv'].sum()/tot*100:.0f}% · fragmento {coef[2]*x['fs_inv'].sum()/tot*100:.0f}%")
    for name, sel in [
        ("aniso>0", x["aniso"] > 0),
        ("linear>0", x["linear"] > 0),
        ("ntex=0", x["ntex"] == 0),
        ("dtest", x["dtest"] == 1),
        ("dwrite", x["dwrite"] == 1),
        ("blend", x["blend"] == 1),
        ("atest", x["atest"] == 1),
        ("wbuf", x["wbuf"] == 1),
        ("stencil", x["stencil"] == 1),
        ("pass_break", x["pass_break"] == 1),
        ("aot", x["aot"] == 1),
    ]:
        if sel.any():
            ys = y[sel]
            fs = x["fs_inv"][sel]
            print(f"   {name:10s}: {sel.mean()*100:5.1f}% dos draws · {ys.sum()/tot*100:5.1f}% do tempo · {ys.mean():7.1f} µs/draw · "
                  f"{(ys.sum()/max(fs.sum(),1))*1000:6.2f} ns/frag")
    # alvos
    print("   por alvo de cor:")
    for col in np.unique(x["color"]):
        s = x["color"] == col
        print(f"     {col}: {s.sum()/nb:.1f} draws/lote · {y[s].sum()/nb:.0f} µs/lote · fs {x['fs_inv'][s].sum()/nb:.0f}/lote · {x['w'][s][0]}x{x['h'][s][0]}")
    # top draws
    top = np.argsort(-y)[:12]
    print("   mais caros:")
    for i in top:
        r = x[i]
        print(f"     {y[i]:8.1f} µs · vs {r['vs_inv']:6d} · fs {r['fs_inv']:8d} · prims {r['prims']:5d} · aniso {r['aniso']} lin {r['linear']} ntex {r['ntex']} dtest {r['dtest']} blend {r['blend']} atest {r['atest']} wbuf {r['wbuf']} cor {r['color']}")
