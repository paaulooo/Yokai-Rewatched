# Captura UM quadro completo e consistente: heap linear + VRAM no inicio da janela,
# todas as listas PICA da janela, e a VRAM de novo no fim (com o quadro ja desenhado).
# Uso (MSYS2 ou PowerShell, na pasta de trabalho):  gdb-multiarch -x dump_frame2.gdb
# Pre-requisito: GDB Stub ligado no Azahar (porta 24689), jogo numa cena estavel (ex.: o quarto).
set pagination off
set confirm off
set architecture arm
set endian little
target remote :24689
python
import gdb, os
SKIP   = 20      # ignora as primeiras N listas (deixa o jogo estabilizar)
WINDOW = 14      # quantas listas gravar (2 quadros de 7 listas; sobra para escolher o melhor)
LINEAR_BASE, LINEAR_SIZE = 0x14000000, 0x04000000
VRAM_BASE,   VRAM_SIZE   = 0x1F000000, 0x00600000
CHUNK = 0x10000
OUT = "cap"
os.makedirs(OUT, exist_ok=True)
hits = 0
saved = 0
idx = open(OUT + "/index.txt", "w")
def dump(name, base, size):
    inf = gdb.selected_inferior()
    with open(name, "wb") as f:
        off = 0
        while off < size:
            n = min(CHUNK, size - off)
            try:
                f.write(inf.read_memory(base + off, n).tobytes())
            except Exception:
                f.write(b"\x00" * n)
            off += n
class Cap(gdb.Breakpoint):
    def stop(self):
        global hits, saved
        hits += 1
        if hits <= SKIP:
            return False
        r0 = int(gdb.parse_and_eval("$r0")) & 0xffffffff
        r1 = int(gdb.parse_and_eval("$r1")) & 0xffffffff
        inf = gdb.selected_inferior()
        if saved == 0:
            dump(OUT + "/linear.bin", LINEAR_BASE, LINEAR_SIZE)
            dump(OUT + "/vram_start.bin", VRAM_BASE, VRAM_SIZE)
            gdb.write("heap linear e VRAM (inicio) salvos\n")
        data = inf.read_memory(r0, r1).tobytes()
        name = "list_%02d.bin" % saved
        open(OUT + "/" + name, "wb").write(data)
        idx.write("%s addr=0x%08x size=0x%x\n" % (name, r0, r1)); idx.flush()
        saved += 1
        if saved >= WINDOW:
            dump(OUT + "/vram_end.bin", VRAM_BASE, VRAM_SIZE)
            gdb.write("FEITO: pasta %s/ (linear.bin, vram_start.bin, vram_end.bin, list_*.bin, index.txt)\n" % OUT)
            return True
        return False
Cap("*0x138f44")
end
continue
