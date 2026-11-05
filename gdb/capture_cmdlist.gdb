# Captura as listas de comandos PICA que o jogo envia (FUN_00138f44 = ProcessCommandList).
# Uso: gdb-multiarch -x capture_cmdlist.gdb   (GDB Stub ligado no Azahar, porta 24689)
set pagination off
set confirm off
set architecture arm
set endian little
target remote :24689
python
import gdb
MAX_LISTS = 300
count = 0
log = open("cmdlists_index.txt", "w")
class Cap(gdb.Breakpoint):
    def stop(self):
        global count
        r0 = int(gdb.parse_and_eval("$r0")) & 0xffffffff   # endereco da lista
        r1 = int(gdb.parse_and_eval("$r1")) & 0xffffffff   # tamanho em bytes
        r2 = int(gdb.parse_and_eval("$r2")) & 0xffffffff
        r3 = int(gdb.parse_and_eval("$r3")) & 0xffffffff
        if count < MAX_LISTS and 8 <= r1 <= 0x200000:
            try:
                data = gdb.selected_inferior().read_memory(r0, r1).tobytes()
                name = "cmdlist_%04d.bin" % count
                open(name, "wb").write(data)
                log.write("%s addr=0x%08x size=0x%x r2=%d flags=0x%x\n" % (name, r0, r1, r2, r3))
                log.flush()
                count += 1
            except Exception as e:
                log.write("erro addr=0x%08x size=0x%x: %s\n" % (r0, r1, e)); log.flush()
        return False   # nao para: segue o jogo
Cap("*0x138f44")
end
continue
