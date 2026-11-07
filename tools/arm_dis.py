"""Mini desassemblador ARM/Thumb sobre data/ghidra/code.bin (VA = offset + 0x100000). Uso: python arm_dis.py VA [n_instr] [thumb]"""
import sys, struct
from capstone import *
from paths import ROOT
D = open(ROOT / 'data' / 'ghidra' / 'code.bin', 'rb').read()
def dis(va, n=60, thumb=False):
    md = Cs(CS_ARCH_ARM, CS_MODE_THUMB if thumb else CS_MODE_ARM)
    off = va - 0x100000
    out = []
    for i in md.disasm(D[off:off + n * 4], va):
        out.append('%08x  %-8s %s' % (i.address, i.mnemonic, i.op_str))
        if len(out) >= n: break
    return '\n'.join(out)
if __name__ == '__main__':
    print(dis(int(sys.argv[1], 16), int(sys.argv[2]) if len(sys.argv) > 2 else 60, len(sys.argv) > 3))
