# Foto de UMA função na entrada e na saída (Azahar GDB stub), para teste diferencial do código recompilado.
#
# Uso (MSYS2 UCRT64, Azahar com o jogo rodando e GDB Stub ligado na porta 24689):
#   cd .../yokai-port
#   gdb-multiarch -q -ex 'set $fn_addr = 0x329bc8' -ex 'set $fn_count = 3' -x gdb/fn_snapshot.gdb
# ou pelo lançador (acha o gdb, fixa as pastas):  python tools/fn_snapshot.py run 0x329bc8 --count 3
#
# Parâmetros (convenience variable do gdb OU variável de ambiente; a convenience variable ganha):
#   $fn_addr  / YW_FN_ADDR          endereço (VA) da função — obrigatório
#   $fn_count / YW_FN_COUNT         quantas chamadas fotografar (padrão 1)
#   $fn_skip  / YW_FN_SKIP          ignora as N primeiras chamadas (padrão 0)
#   $fn_stack / YW_FN_STACK_PAGES   páginas de pilha a partir da página de sp, para cima (padrão 2)
#   $fn_ptr   / YW_FN_PTR_PAGES     páginas a partir de cada r0..r3 que for um endereço legível (padrão 1)
#   YW_FN_EXTRA="0xADDR:0xTAM,..."  faixas extras gravadas na entrada e na saída
#   YW_FNSNAP_OUT                   pasta raiz (padrão: <yokai-port>/work/fnsnap, achada a partir da pasta atual)
#   YW_GDB_TARGET                   alvo (padrão ":24689")
#   YW_FNSNAP_DRYRUN=1              não conecta: só testa as funções auxiliares (validação de sintaxe/lógica)
#
# Saída: <OUT>/<va 8 hex>/<NNN>/{entry,exit}.json + {entry,exit}_<addr 8 hex>.bin   e   <OUT>/<va>/index.json
#   Formato descrito em docs/ENVIRONMENT.md ("Comparação com o Azahar (recompilação)").
#
# Como acha a saída: breakpoint em fn_addr; na parada grava a foto de entrada e põe um breakpoint em lr&~1.
# Quando esse breakpoint para com sp == sp da entrada (AAPCS: o chamador vê sp restaurado), é o retorno DESTA
# chamada (recursão/outras chamadas com sp diferente são ignoradas) -> foto de saída. No fim: detach (o jogo segue).
set pagination off
set confirm off
set architecture arm
set endian little
set remotetimeout 10
python
import gdb, os, json, time

PAGE = 0x1000
M32 = 0xffffffff

def param(conv, env, default=None):
    v = gdb.convenience_variable(conv)
    if v is not None:
        return int(v) & M32
    s = os.environ.get(env)
    if s:
        return int(s, 0)
    return default

def find_out():
    o = os.environ.get("YW_FNSNAP_OUT")
    if o:
        return o
    if os.environ.get("YW_WORK"):
        return os.path.join(os.environ["YW_WORK"], "fnsnap")
    d = os.path.abspath(os.getcwd())
    for _ in range(6):   # sobe até achar a raiz do projeto (tem gdb/fn_snapshot.gdb)
        if os.path.exists(os.path.join(d, "gdb", "fn_snapshot.gdb")):
            return os.path.join(d, "work", "fnsnap")
        d = os.path.dirname(d)
    return os.path.join(os.getcwd(), "fnsnap")

def parse_extra(s):
    out = []
    for item in (s or "").split(","):
        item = item.strip()
        if item:
            a, n = item.split(":")
            out.append((int(a, 0), int(n, 0)))
    return out

CORE = ["r%d" % i for i in range(13)] + ["sp", "lr", "pc", "cpsr"]
CORE_KEY = {"sp": "r13", "lr": "r14", "pc": "r15"}

def raw_bits(v):
    """Bits crus de um registrador (d* é float/união no gdb; int(v) converteria o VALOR, não os bits)."""
    try:
        b = v.bytes                      # gdb >= 14
        return int.from_bytes(bytes(b), "little")
    except Exception:
        pass
    t = v.type.strip_typedefs()
    if t.code == gdb.TYPE_CODE_UNION:
        for f in ("u64", "u32", "s64", "s32"):
            try:
                return int(v[f]) & ((1 << (8 * t.sizeof)) - 1)
            except Exception:
                pass
    if t.code == gdb.TYPE_CODE_INT:
        return int(v) & ((1 << (8 * t.sizeof)) - 1)
    raise ValueError("sem bits crus para tipo %s" % t)

def read_regs():
    fr = gdb.selected_frame()
    regs, missing = {}, []
    for n in CORE:
        try:
            regs[CORE_KEY.get(n, n)] = int(fr.read_register(n)) & M32
        except Exception:
            missing.append(n)
    vfp = {"s": None, "fpscr": None, "source": None}
    try:   # VFPv2 do ARM11: d0..d15 = s0..s31 (s[2i] = metade baixa de d[i])
        s = []
        for i in range(16):
            d = raw_bits(fr.read_register("d%d" % i))
            s += [d & M32, (d >> 32) & M32]
        vfp["s"], vfp["source"] = s, "d0-d15"
    except Exception:
        try:
            vfp["s"] = [raw_bits(fr.read_register("s%d" % i)) & M32 for i in range(32)]
            vfp["source"] = "s0-s31"
        except Exception:
            missing.append("vfp")
    try:
        vfp["fpscr"] = raw_bits(fr.read_register("fpscr")) & M32
    except Exception:
        missing.append("fpscr")
    return regs, vfp, missing

def wanted_pages(regs, stack_pages, ptr_pages, extra, base=None):
    """dict página -> lista de motivos."""
    want = {} if base is None else {p: list(w) for p, w in base.items()}
    def add(addr, npages, why):
        p0 = addr & ~(PAGE - 1)
        for k in range(npages):
            want.setdefault((p0 + k * PAGE) & M32, [])
            if why not in want[(p0 + k * PAGE) & M32]:
                want[(p0 + k * PAGE) & M32].append(why)
    add(regs["r13"], stack_pages, "sp")
    for r in ("r0", "r1", "r2", "r3"):
        if regs.get(r, 0) >= 0x1000:         # valores pequenos são números, não ponteiros
            add(regs[r], ptr_pages, r)
    for a, n in extra:
        add(a, (((a & (PAGE - 1)) + n + PAGE - 1) // PAGE), "extra")
    return want

def merge_runs(pages):
    """[(início, nº de páginas)] de páginas contíguas, em ordem."""
    runs = []
    for p in sorted(pages):
        if runs and runs[-1][0] + runs[-1][1] * PAGE == p:
            runs[-1][1] += 1
        else:
            runs.append([p, 1])
    return [(a, n) for a, n in runs]

def dump_pages(dirn, kind, want, reader):
    ok, bad = {}, []
    for p in sorted(want):
        try:
            ok[p] = reader(p, PAGE)
        except Exception:
            bad.append(p)     # página não mapeada (ex.: r0 era um número grande, não ponteiro)
    mem = []
    for a, n in merge_runs(ok):
        fn = "%s_%08x.bin" % (kind, a)
        with open(os.path.join(dirn, fn), "wb") as f:
            for k in range(n):
                f.write(ok[a + k * PAGE])
        why = sorted({w for k in range(n) for w in want[a + k * PAGE]})
        mem.append({"addr": "0x%08x" % a, "size": "0x%x" % (n * PAGE), "file": fn, "why": why})
    return mem, ["0x%08x" % p for p in bad]

def asc(e):
    # mensagens do gdb em ASCII: o console do gdb no Windows nao codifica acentos (UnicodeEncodeError)
    return str(e).encode("ascii", "replace").decode("ascii")

def hx(v):
    return None if v is None else "0x%08x" % v

def write_snap(dirn, kind, fn_addr, hit, call_no, regs, vfp, missing, mem, bad, extra_fields):
    j = {"format": "yw1-fnsnap-1", "kind": kind, "va": hx(fn_addr), "hit": hit, "call_no": call_no,
         "time": time.strftime("%Y-%m-%dT%H:%M:%S"),
         "regs": {k: hx(v) for k, v in regs.items()},
         "vfp": {"s": None if vfp["s"] is None else [hx(x) for x in vfp["s"]],
                 "fpscr": hx(vfp["fpscr"]), "source": vfp["source"]},
         "missing_regs": missing, "mem": mem, "unreadable_pages": bad}
    j.update(extra_fields)
    with open(os.path.join(dirn, kind + ".json"), "w") as f:
        json.dump(j, f, indent=1)

def gdb_reader(addr, size):
    return gdb.selected_inferior().read_memory(addr, size).tobytes()

def selftest(out):
    """Valida a lógica sem alvo (YW_FNSNAP_DRYRUN=1)."""
    regs = {"r%d" % i: 0 for i in range(16)}
    regs.update({"r0": 0x14001234, "r1": 5, "r2": 0x14001ff0, "r3": 0xdeadbeef, "r13": 0x0ffffe10, "r14": 0x100204, "r15": 0x329bc8})
    want = wanted_pages(regs, 2, 1, parse_extra("0x1f000000:0x1800"))
    def fake(a, n):
        if a == 0xdeadb000:
            raise gdb.MemoryError("nao mapeada")
        return bytes([(a >> 12) & 0xff]) * n
    d = os.path.join(out, "_dryrun", "00329bc8", "000")
    os.makedirs(d, exist_ok=True)
    mem, bad = dump_pages(d, "entry", want, fake)
    write_snap(d, "entry", 0x329bc8, 0, 1, regs, {"s": [0] * 32, "fpscr": 0, "source": "teste"}, [], mem, bad,
               {"return_to": hx(0x100204)})
    gdb.write("selftest: paginas=%s\n" % ", ".join("%08x" % p for p in sorted(want)))
    gdb.write("selftest: arquivos=%s  ilegiveis=%s\n" % ([m["file"] + "/" + m["size"] for m in mem], bad))
    gdb.write("selftest: gravado em %s\n" % d)

def main():
    out = find_out()
    if os.environ.get("YW_FNSNAP_DRYRUN"):
        selftest(out)
        return
    fn_addr = param("fn_addr", "YW_FN_ADDR")
    if fn_addr is None:
        raise gdb.GdbError("defina o endereco: -ex 'set $fn_addr = 0x...' (ou YW_FN_ADDR)")
    count = param("fn_count", "YW_FN_COUNT", 1)
    skip = param("fn_skip", "YW_FN_SKIP", 0)
    stack_pages = param("fn_stack", "YW_FN_STACK_PAGES", 2)
    ptr_pages = param("fn_ptr", "YW_FN_PTR_PAGES", 1)
    extra = parse_extra(os.environ.get("YW_FN_EXTRA"))
    root = os.path.join(out, "%08x" % fn_addr)
    target = os.environ.get("YW_GDB_TARGET", ":24689")
    try:
        gdb.execute("target remote " + target)
    except gdb.error as e:
        raise gdb.GdbError("nao conectou em %s (%s). O Azahar esta aberto, com o jogo rodando e o GDB Stub "
                           "ligado (Configurar > Depuracao, porta 24689)?" % (target, asc(e)))
    os.makedirs(root, exist_ok=True)
    entry_bp = gdb.Breakpoint("*0x%x" % fn_addr)
    ret_bps = {}          # endereço de retorno -> breakpoint
    pending = []          # chamadas abertas: dict(lr, sp, hit, dir, want)
    index = []
    calls = started = done = 0
    gdb.write("fn_snapshot: esperando 0x%08x (pular %d, gravar %d) -> %s\n" % (fn_addr, skip, count, root))
    while done < count:
        try:
            gdb.execute("continue")
        except KeyboardInterrupt:
            gdb.write("interrompido\n"); break
        except gdb.error as e:
            gdb.write("continue falhou: %s\n" % asc(e)); break
        fr = gdb.selected_frame()
        pc = int(fr.read_register("pc")) & M32
        sp = int(fr.read_register("sp")) & M32
        if pc == fn_addr and entry_bp.enabled:
            calls += 1
            if calls <= skip:
                continue
            hit = started; started += 1
            regs, vfp, missing = read_regs()
            d = os.path.join(root, "%03d" % hit); os.makedirs(d, exist_ok=True)
            want = wanted_pages(regs, stack_pages, ptr_pages, extra)
            mem, bad = dump_pages(d, "entry", want, gdb_reader)
            lr = regs["r14"] & ~1
            write_snap(d, "entry", fn_addr, hit, calls, regs, vfp, missing, mem, bad, {"return_to": hx(lr)})
            pending.append({"lr": lr, "sp": sp, "hit": hit, "dir": d, "want": want, "call": calls, "r0": regs["r0"]})
            if lr not in ret_bps:
                ret_bps[lr] = gdb.Breakpoint("*0x%x" % lr, internal=True)
            gdb.write("entrada #%d (chamada %d): r0=%08x r1=%08x sp=%08x volta p/ %08x\n" %
                      (hit, calls, regs["r0"], regs["r1"], sp, lr))
            if started >= count:
                entry_bp.enabled = False
        elif pc in ret_bps:
            m = [p for p in pending if p["lr"] == pc and p["sp"] == sp]
            if not m:
                continue          # outra chamada (recursão / outro chamador) passando pelo mesmo endereço
            p = m[-1]; pending.remove(p)
            regs, vfp, missing = read_regs()
            want = wanted_pages(regs, stack_pages, ptr_pages, extra, base=p["want"])   # inclui as páginas da entrada
            mem, bad = dump_pages(p["dir"], "exit", want, gdb_reader)
            write_snap(p["dir"], "exit", fn_addr, p["hit"], p["call"], regs, vfp, missing, mem, bad, {})
            index.append({"hit": p["hit"], "call_no": p["call"], "dir": "%03d" % p["hit"],
                          "r0_in": hx(p["r0"]), "r0_out": hx(regs["r0"])})
            done += 1
            gdb.write("saida   #%d: r0=%08x\n" % (p["hit"], regs["r0"]))
            if not any(q["lr"] == pc for q in pending):
                ret_bps.pop(pc).delete()
        else:
            gdb.write("parada inesperada em pc=%08x; encerrando\n" % pc); break
    with open(os.path.join(root, "index.json"), "w") as f:
        json.dump({"format": "yw1-fnsnap-1", "va": hx(fn_addr), "complete": done,
                   "open_calls": [p["hit"] for p in pending],
                   "hits": sorted(index, key=lambda h: h["hit"])}, f, indent=1)
    entry_bp.delete()
    for b in ret_bps.values():
        b.delete()
    gdb.write("fn_snapshot: %d chamada(s) completas em %s\n" % (done, root))
    try:
        gdb.execute("detach")    # o jogo continua rodando no Azahar
    except gdb.error:
        pass

main()
end
quit
