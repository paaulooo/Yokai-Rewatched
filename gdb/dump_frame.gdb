# Para o jogo na N-esima ProcessCommandList e salva: lista PICA + VRAM + heap linear.
# Uso: gdb-multiarch -x dump_frame.gdb      (GDB Stub ligado, jogo ja numa cena estavel)
set pagination off
set confirm off
set architecture arm
set endian little
target remote :24689
python
import gdb
TARGET_HIT = 3          # para no 3o envio de lista (ajuste se quiser outro quadro)
LINEAR_BASE, LINEAR_SIZE = 0x14000000, 0x04000000   # heap linear (64 MB, modo 64MB do jogo)
VRAM_BASE,  VRAM_SIZE   = 0x1F000000, 0x00600000   # VRAM (6 MB)
CHUNK = 0x10000
hits = 0
def dump(name, base, size):
    inf = gdb.selected_inferior()
    done = 0
    with open(name, "wb") as f:
        off = 0
        while off < size:
            n = min(CHUNK, size - off)
            try:
                f.write(inf.read_memory(base + off, n).tobytes())
            except Exception:
                f.write(b"\x00" * n)      # regiao nao mapeada: preenche com zeros
            off += n
class Cap(gdb.Breakpoint):
    def stop(self):
        global hits
        hits += 1
        if hits < TARGET_HIT:
            return False
        r0 = int(gdb.parse_and_eval("$r0")) & 0xffffffff
        r1 = int(gdb.parse_and_eval("$r1")) & 0xffffffff
        inf = gdb.selected_inferior()
        open("frame_cmdlist.bin", "wb").write(inf.read_memory(r0, r1).tobytes())
        open("frame_info.txt", "w").write("cmdlist addr=0x%08x size=0x%x\n" % (r0, r1))
        dump("frame_vram.bin", VRAM_BASE, VRAM_SIZE)
        dump("frame_linear.bin", LINEAR_BASE, LINEAR_SIZE)
        gdb.write("FEITO: frame_cmdlist.bin, frame_vram.bin, frame_linear.bin\n")
        return True      # para aqui
Cap("*0x138f44")
end
continue
