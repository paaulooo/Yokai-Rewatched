"""Cliente do servidor RPC do Azahar para comparar o jogo emulado com a recompilação.

Usa o módulo oficial `AzaharRPC.py` que vem com o Azahar (importado de
"C:/Program Files/Azahar/scripting" via sys.path; YW_AZAHAR_SCRIPTING sobrescreve). Nada é copiado para o repo.

Subcomandos (saídas em work/azahar/):
  shot  [nome]                     tela de cima e de baixo do PRÓXIMO quadro -> <nome>_top.png, <nome>_bottom.png (+ _full.png)
  watch N [--every S] [--name X]   N screenshots espaçados de S segundos -> X_000_top.png ...
  mem   FAIXA [FAIXA...]           lê memória guest; FAIXA = 0xADDR:0xTAM | linear | vram -> mem/<nome>_<addr>_<tam>.bin
  procs                            lista processos (pid, title id, nome) e o processo selecionado
  perf                             estatísticas de desempenho (fps, velocidade)
  ping                             só testa se o RPC responde

Ligar o RPC (está DESLIGADO na config do Paulo; este script não mexe nela):
  Azahar -> Emulação -> Configurar -> Depuração -> marcar a opção do servidor RPC [H: nome exato do rótulo na UI]
  (equivale a [Debugging] enable_rpc_server=true em %APPDATA%/Azahar/config/qt-config.ini, com o Azahar fechado),
  e reiniciar o Azahar. Porta UDP fixa 45987 em 127.0.0.1.

API de screenshot (lida do AzaharRPC.py): screenshot(res_scale, secondary_window) devolve UM PNG do quadro seguinte com o
LAYOUT da janela (as duas telas juntas no layout padrão). res_scale=0 usa a resolução interna atual (resolution_factor).
secondary_window=True captura a janela secundária (só existe no layout "Janelas separadas", onde ela mostra a tela de baixo).
Por isso este script recorta top/bottom da imagem conforme o layout (ver split_layout).
"""
import argparse
import configparser
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import WORK  # noqa: E402

AZAHAR_SCRIPTING = Path(os.environ.get("YW_AZAHAR_SCRIPTING", r"C:/Program Files/Azahar/scripting"))
QT_CONFIG = Path(os.environ.get("YW_AZAHAR_CONFIG",
                                Path(os.environ.get("APPDATA", "")) / "Azahar" / "config" / "qt-config.ini"))
RPC_HOST = os.environ.get("YW_RPC_HOST", "127.0.0.1")
OUT = WORK / "azahar"

TOP_W, TOP_H, BOT_W, BOT_H = 400, 240, 320, 240
PRESETS = {  # faixas de memória úteis (VA do processo do jogo)
    "linear": (0x14000000, 0x04000000),   # heap linear (FCRAM) — o mesmo que data/capture/cap/linear.bin
    "vram":   (0x1F000000, 0x00600000),   # VRAM — o mesmo que vram_start.bin / vram_end.bin
    "code":   (0x00100000, 0x004FD000),   # code.bin carregado
}

MSG_SEM_RPC = """
ERRO: o servidor RPC do Azahar não respondeu em {host}:{port} ({err}).
  - O Azahar está aberto e com um jogo rodando? (sem jogo o RPC não atende)
  - O servidor RPC está ligado? Na config atual ele está DESLIGADO
    ({cfg}: [Debugging] enable_rpc_server={val}).
    Para ligar: Azahar -> Emulação -> Configurar -> Depuração -> habilitar o servidor RPC
    (ou, com o Azahar FECHADO, editar enable_rpc_server=true) e reiniciar o Azahar. Porta UDP {port}.
  - Firewall: é UDP local (127.0.0.1), normalmente não bloqueia.
""".strip()


def load_rpc_module():
    if not (AZAHAR_SCRIPTING / "AzaharRPC.py").exists():
        sys.exit(f"ERRO: não achei {AZAHAR_SCRIPTING / 'AzaharRPC.py'} (defina YW_AZAHAR_SCRIPTING com a pasta 'scripting' do Azahar)")
    sys.path.insert(0, str(AZAHAR_SCRIPTING))
    import AzaharRPC  # noqa: E402  (módulo do Azahar, GPLv2, não versionado aqui)
    return AzaharRPC


def read_qt_config():
    """Lê (sem alterar) as opções de layout/RPC do qt-config.ini. Devolve dict de strings (pode vir vazio)."""
    out = {}
    if not QT_CONFIG.exists():
        return out
    cp = configparser.RawConfigParser(strict=False, interpolation=None)
    try:
        cp.read(QT_CONFIG, encoding="utf-8")
    except Exception:
        return out
    for sec, keys in (("Debugging", ("enable_rpc_server", "use_gdbstub", "gdbstub_port")),
                      ("Layout", ("layout_option", "swap_screen", "upright_screen", "screen_gap")),
                      ("Renderer", ("resolution_factor",))):
        for k in keys:
            if cp.has_option(sec, k):
                out[k] = cp.get(sec, k)
    return out


def connect(port=None, timeout=2.0, retries=1):
    """Cria o cliente e confirma que o RPC responde (get_process é seguro de reenviar). Sai com mensagem clara se não."""
    mod = load_rpc_module()
    port = port or mod.CITRA_PORT
    rpc = mod.AzaharRPC(address=RPC_HOST, port=port, timeout=timeout, retries=retries)
    try:
        pid = rpc.get_process()
    except (TimeoutError, OSError) as e:   # no Windows, UDP p/ porta fechada costuma dar ConnectionResetError (10054)
        cfg = read_qt_config()
        sys.exit(MSG_SEM_RPC.format(host=RPC_HOST, port=port, err=type(e).__name__, cfg=QT_CONFIG,
                                    val=cfg.get("enable_rpc_server", "?")))
    return mod, rpc, pid


# ---------------------------------------------------------------- layout -> telas
def split_layout(w, h, cfg):
    """Devolve {'top': (x,y,w,h), 'bottom': (x,y,w,h)} recortando a imagem do layout, ou {} se não reconhecer.

    Regras (Citra/Azahar framebuffer_layout, sem rotação) [H: conferir com um screenshot real]:
      padrão (layout_option=0): largura 400s, altura (480+gap)s; cima em (0,0), baixo centralizada em (40s, 240s+gap*s)
                                swap_screen=true inverte a ordem (baixo em cima, centralizada).
      lado a lado (3):          largura 720s, altura 240s; cima em (0,0), baixo em (400s, 0)
      tela única (1/4):         400s x 240s = só a de cima; 320s x 240s = só a de baixo
    """
    layout = cfg.get("layout_option", "0")
    swap = cfg.get("swap_screen", "false").lower() == "true"
    if cfg.get("upright_screen", "false").lower() == "true":
        return {}
    gap = int(cfg.get("screen_gap", "0") or 0)
    s = w / TOP_W
    if layout in ("0", "") and s == int(s) and abs(h - (TOP_H * 2 + gap) * s) < 1:
        s = int(s)
        g = int(round(gap * s))
        if not swap:
            return {"top": (0, 0, TOP_W * s, TOP_H * s), "bottom": (40 * s, TOP_H * s + g, BOT_W * s, BOT_H * s)}
        return {"bottom": (40 * s, 0, BOT_W * s, BOT_H * s), "top": (0, BOT_H * s + g, TOP_W * s, TOP_H * s)}
    s = h / TOP_H
    if s == int(s) and s > 0:
        s = int(s)
        if w == (TOP_W + BOT_W) * s:
            return {"top": (0, 0, TOP_W * s, TOP_H * s), "bottom": (TOP_W * s, 0, BOT_W * s, BOT_H * s)}
        if w == TOP_W * s:
            return {"top": (0, 0, w, h)}
        if w == BOT_W * s:
            return {"bottom": (0, 0, w, h)}
    return {}


def parse_rect(t):
    v = [int(x, 0) for x in t.split(",")]
    if len(v) != 4:
        raise argparse.ArgumentTypeError("retângulo = x,y,l,a")
    return tuple(v)


def take_shot(rpc, name, args, cfg):
    """Pega um screenshot e grava _full/_top/_bottom. Devolve lista de arquivos gravados."""
    from PIL import Image
    import io
    OUT.mkdir(parents=True, exist_ok=True)
    t0 = time.perf_counter()
    png = rpc.screenshot(res_scale=args.scale, secondary_window=False)
    dt = time.perf_counter() - t0
    img = Image.open(io.BytesIO(png)).convert("RGB")
    saved = []
    if args.full:
        p = OUT / f"{name}_full.png"; img.save(p); saved.append(p)
    rects = {}
    if args.top_rect or args.bottom_rect:
        if args.top_rect: rects["top"] = args.top_rect
        if args.bottom_rect: rects["bottom"] = args.bottom_rect
    else:
        rects = split_layout(img.width, img.height, cfg)
    if not rects:
        p = OUT / f"{name}_full.png"
        if p not in saved:
            img.save(p); saved.append(p)
        print(f"AVISO: layout {img.width}x{img.height} não reconhecido (config: {cfg}); salvei só {p.name}. "
              f"Use --top-rect/--bottom-rect x,y,l,a.")
    for scr, (x, y, w, h) in rects.items():
        p = OUT / f"{name}_{scr}.png"
        img.crop((x, y, x + w, y + h)).save(p); saved.append(p)
    if "bottom" not in rects and args.secondary:
        # layout "Janelas separadas": a de baixo fica na janela secundária -> é OUTRO quadro (o seguinte) [H]
        png2 = rpc.screenshot(res_scale=args.scale, secondary_window=True)
        p = OUT / f"{name}_bottom.png"
        Image.open(io.BytesIO(png2)).convert("RGB").save(p); saved.append(p)
        print("AVISO: tela de baixo veio da janela secundária = quadro diferente do de cima")
    print(f"{name}: imagem {img.width}x{img.height} em {dt*1000:.0f} ms -> " + ", ".join(str(p) for p in saved))
    return saved


def cmd_shot(args):
    _, rpc, pid = connect(args.port, args.timeout)
    cfg = read_qt_config()
    try:
        take_shot(rpc, args.name, args, cfg)
    except RuntimeError as e:
        sys.exit(f"ERRO no screenshot: {e} (Busy = outro pedido em andamento; Timeout = o jogo não apresentou quadro — pausado?)")


def cmd_watch(args):
    _, rpc, pid = connect(args.port, args.timeout)
    cfg = read_qt_config()
    t_next = time.perf_counter()
    for i in range(args.count):
        try:
            take_shot(rpc, f"{args.name}_{i:03d}", args, cfg)
        except RuntimeError as e:
            print(f"#{i}: falhou ({e})")
        t_next += args.every
        time.sleep(max(0.0, t_next - time.perf_counter()))


def parse_range(t):
    if t in PRESETS:
        return t, PRESETS[t][0], PRESETS[t][1]
    if ":" not in t:
        raise argparse.ArgumentTypeError(f"faixa '{t}': use 0xADDR:0xTAM ou {'/'.join(PRESETS)}")
    a, n = t.split(":", 1)
    return "mem", int(a, 0), int(n, 0)


def cmd_mem(args):
    mod, rpc, pid = connect(args.port, args.timeout)
    if args.pid is not None:
        rpc.set_process(args.pid)
    (OUT / "mem").mkdir(parents=True, exist_ok=True)
    chunk = mod.MAX_REQUEST_DATA_SIZE
    for label, addr, size in args.ranges:
        name = args.name or label
        p = OUT / "mem" / f"{name}_{addr:08x}_{size:x}.bin"
        bad = 0
        t0 = time.perf_counter()
        with open(p, "wb") as f:
            off = 0
            while off < size:
                n = min(chunk, size - off)
                try:
                    d = rpc.read_memory(addr + off, n)
                except (TimeoutError, OSError):
                    d = None
                if not d or len(d) != n:
                    d = b"\x00" * n; bad += 1   # página não mapeada / pacote perdido -> zeros (contado)
                f.write(d)
                off += n
        nchunks = (size + chunk - 1) // chunk
        print(f"{p}  ({size:#x} B em {time.perf_counter()-t0:.1f} s; blocos com falha preenchidos com 0: {bad}/{nchunks})")
        if args.hexdump and size <= 0x400:
            data = p.read_bytes()
            for o in range(0, len(data), 16):
                print(f"  {addr+o:08x}: " + " ".join(f"{b:02x}" for b in data[o:o+16]))


def cmd_procs(args):
    _, rpc, pid = connect(args.port, args.timeout)
    for p, (tid, nm) in sorted(rpc.process_list().items()):
        print(f"{'*' if p == pid else ' '} pid={p:3d}  title={tid:016x}  {nm}")
    print("(* = processo selecionado para 'mem'; o jogo é o title 0004000000167700)")


def cmd_perf(args):
    _, rpc, _ = connect(args.port, args.timeout)
    st = rpc.perf_stats(reset=False)
    if not st:
        sys.exit("perf_stats indisponível")
    for k, v in st.items():
        print(f"{k:22s} {v}")


def cmd_ping(args):
    _, rpc, pid = connect(args.port, args.timeout)
    print(f"RPC OK em {RPC_HOST}:{args.port or 45987}; processo selecionado pid={pid}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=None, help="porta UDP do RPC (padrão 45987)")
    ap.add_argument("--timeout", type=float, default=2.0, help="segundos por pedido (screenshot espera >= 30 s)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    def shot_opts(p):
        p.add_argument("--scale", type=int, default=1, help="escala do screenshot (0 = resolução interna atual; padrão 1 = nativo)")
        p.add_argument("--full", action="store_true", help="grava também a imagem do layout inteiro (_full.png)")
        p.add_argument("--secondary", action="store_true", help="se a de baixo não estiver no layout, pega a janela secundária")
        p.add_argument("--top-rect", type=parse_rect, help="recorte manual da tela de cima x,y,l,a")
        p.add_argument("--bottom-rect", type=parse_rect, help="recorte manual da tela de baixo x,y,l,a")

    p = sub.add_parser("shot", help="tela de cima/baixo do próximo quadro"); p.add_argument("name", nargs="?", default="shot")
    shot_opts(p); p.set_defaults(fn=cmd_shot)
    p = sub.add_parser("watch", help="N screenshots com intervalo"); p.add_argument("count", type=int)
    p.add_argument("--every", type=float, default=1.0, help="intervalo em segundos"); p.add_argument("--name", default="watch")
    shot_opts(p); p.set_defaults(fn=cmd_watch)
    p = sub.add_parser("mem", help="lê memória guest para .bin"); p.add_argument("ranges", nargs="+", type=parse_range)
    p.add_argument("--name", help="prefixo do arquivo"); p.add_argument("--pid", type=int, help="seleciona outro processo antes")
    p.add_argument("--hexdump", action="store_true", help="imprime o conteúdo (faixas <= 1 KB)"); p.set_defaults(fn=cmd_mem)
    sub.add_parser("procs").set_defaults(fn=cmd_procs)
    sub.add_parser("perf").set_defaults(fn=cmd_perf)
    sub.add_parser("ping").set_defaults(fn=cmd_ping)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
