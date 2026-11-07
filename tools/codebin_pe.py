"""Empacota o code.bin (ARM11 do 3DS, sem cabeçalho) num PE32 com máquina ARM, mantendo os endereços virtuais
reais (.text 0x100000, .rodata 0x4C0000, .data 0x5BE000 — ver docs/ENVIRONMENT.md). Motivo: o REA no Windows só
aceita PE nativo no adaptador Ghidra, e para ARM exige a máquina ARMNT (0x01C4). Validado: com este invólucro o REA
(Ghidra 12.1.4) descompila FUN_00329bc8 (leitor da fonte) igual ao export data/ghidra/code.bin.c — o código ARM não
é lido como Thumb.

REA: GHIDRA_INSTALL_DIR=<projeto>/ghidra_12.1.4_PUBLIC, JAVA_HOME=JDK 21; ex.:
  npx -y rea-agents@6.8.0 decompile work/code_arm.exe 0x329bc8 --json   (≈2 min a frio por chamada no CLI)

Uso: python codebin_pe.py [saída]   (padrão: work/code_arm.exe)
O PE não é executável de verdade (sem imports/relocações); serve só para análise estática.
"""
import struct, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from paths import ROOT, WORK

IMAGE_BASE = 0xF0000            # alinhado a 64 KiB; .text fica em RVA 0x10000 → VA 0x100000
SECTIONS = [  # nome, offset no code.bin, tamanho, VA, flags
    (b'.text',   0x000000, 0x3C0000, 0x100000, 0x60000020),  # código | executável | leitura
    (b'.rodata', 0x3C0000, 0x0FE000, 0x4C0000, 0x40000040),  # dados inicializados | leitura
    (b'.data',   0x4BE000, 0x03F000, 0x5BE000, 0xC0000040),  # dados | leitura | escrita
]
FILE_ALIGN, SECT_ALIGN = 0x200, 0x1000


def align(x, a):
    return (x + a - 1) // a * a


def build(code):
    assert len(code) == 0x4FD000, 'code.bin inesperado (tamanho %#x)' % len(code)
    nsec = len(SECTIONS)
    headers = align(0x40 + 4 + 20 + 0xE0 + 40 * nsec, FILE_ALIGN)
    raw, ptr = [], headers
    for name, off, size, va, fl in SECTIONS:
        raw.append((name, code[off:off + size], ptr, va - IMAGE_BASE, size, fl))
        ptr += align(size, FILE_ALIGN)
    image_size = align(max(r[3] + r[4] for r in raw), SECT_ALIGN)
    dos = bytearray(0x40); dos[0:2] = b'MZ'; struct.pack_into('<I', dos, 0x3C, 0x40)
    coff = struct.pack('<HHIIIHH', 0x01C4, nsec, 0, 0, 0, 0xE0, 0x0102)  # ARMNT (exigido pelo REA), executável, 32 bits
    opt = struct.pack('<HBBIIIIIIIIIHHHHHHIIIIHHIIIIII',
                      0x10B, 1, 0, 0x3C0000, 0x13D000, 0, 0x10000, 0x10000, 0x3D0000,
                      IMAGE_BASE, SECT_ALIGN, FILE_ALIGN, 4, 0, 0, 0, 4, 0, 0,
                      image_size, headers, 0, 1, 0, 0x100000, 0x1000, 0x100000, 0x1000, 0, 16)
    opt += b'\0' * (16 * 8)
    sec = b''
    for name, data, p, rva, size, fl in raw:
        sec += struct.pack('<8sIIIIIIHHI', name, size, rva, align(size, FILE_ALIGN), p, 0, 0, 0, 0, fl)
    out = bytearray(dos + b'PE\0\0' + coff + opt + sec)
    out += b'\0' * (headers - len(out))
    for name, data, p, rva, size, fl in raw:
        out += data + b'\0' * (align(size, FILE_ALIGN) - size)
    return bytes(out)


if __name__ == '__main__':
    dst = sys.argv[1] if len(sys.argv) > 1 else str(WORK / 'code_arm.exe')
    pe = build(open(ROOT / 'data' / 'ghidra' / 'code.bin', 'rb').read())
    open(dst, 'wb').write(pe)
    print('PE ARM gravado em', dst, len(pe), 'bytes')
