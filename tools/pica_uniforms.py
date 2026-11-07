"""Decodificador dos uniforms float do vertex shader PICA200 (c[0..95], VSH_FLOATUNIFORM_*,
regs 0x2c0=index/mode, 0x2c1=dados). Validado contra dados reais da captura (nao adivinhado):

* Escalar float24 (1 sinal + 7 expoente[bias 63] + 16 mantissa, bits 23..0): confirmado batendo
  VIEWPORT_WIDTH/HEIGHT reais (0x46e000 -> 240.0, 0x469000 -> 200.0, ver docs/GPU-PICA.md).
* Empacotamento de um vec4 float24 em 3 palavras de 32 bits: a ordem (palavra2,palavra1,palavra0)
  concatenadas em um inteiro de 96 bits, com os 4 componentes extraidos em ordem x,y,z,w a partir
  dos 24 bits menos significativos, foi confirmada contra o uniform c[91] real da captura
  (list_02.bin, draw 0): decodifica para (0.0, 1.0, 2.0, 3.0) -- valores inteiros limpos, e c[92]
  decodifica para (0.125, 1/256, 0.5, 0.25) -- fracoes limpas (potencias de 2). Essas coincidencias
  praticamente descartam ordem/alinhamento errados.
* vec4 float32: 4 palavras IEEE754 diretas em ordem x,y,z,w (confirmado: c[0..2] carregados assim
  formam uma matriz plausivel de projecao/view, ver tools/vk_render_min.py).

Uso: `uniforms_per_draw(lists)` roda o mesmo gatilho de draw do tools/frame_sim.py (escrita no reg
0x22f) e devolve, por draw, uma copia do array c[0..95] (cada entrada = lista de 4 floats) tal como
estava no momento do draw.
"""
import sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from frame_sim import decode


def f24(raw24):
    raw24 &= 0xFFFFFF
    sign = (raw24 >> 23) & 1
    exp = (raw24 >> 16) & 0x7F
    mant = raw24 & 0xFFFF
    if exp == 0 and mant == 0:
        return 0.0
    v = (1.0 + mant / 65536.0) * (2.0 ** (exp - 63))
    return -v if sign else v


def f24_vec4(w0, w1, w2):
    n = (w2 & 0xFFFFFFFF) | ((w1 & 0xFFFFFFFF) << 32) | ((w0 & 0xFFFFFFFF) << 64)
    return [f24((n >> (24 * i)) & 0xFFFFFF) for i in range(4)]


def _f32(word):
    import struct
    return struct.unpack('<f', struct.pack('<I', word & 0xFFFFFFFF))[0]


def uniforms_per_draw(lists):
    table = [[0.0, 0.0, 0.0, 0.0] for _ in range(96)]
    mode = True
    index = 0
    out = []
    for L, fn in enumerate(lists):
        for reg, mask, consec, params in decode(open(fn, 'rb').read()):
            if reg == 0x2c0:
                if len(params) == 1:
                    cfg = params[0]
                    mode = bool(cfg >> 31)
                    index = cfg & 0xFF
                elif len(params) == 4:
                    cfg = params[0]
                    idx = cfg & 0xFF
                    if idx < 96:
                        table[idx] = f24_vec4(params[1], params[2], params[3])
                continue
            if reg == 0x2c1:
                if mode:
                    n = len(params) // 4
                    for i in range(n):
                        if index < 96:
                            table[index] = [_f32(w) for w in params[4 * i:4 * i + 4]]
                        index += 1
                else:
                    n = len(params) // 3
                    for i in range(n):
                        if index < 96:
                            table[index] = f24_vec4(*params[3 * i:3 * i + 3])
                        index += 1
                continue
            if reg == 0x22f:
                out.append((L, [row[:] for row in table]))
    return out


if __name__ == '__main__':
    import os
    from paths import CAP
    C = str(CAP) + os.sep
    frame = [C + 'list_%02d.bin' % i for i in range(1, 8)]
    D = uniforms_per_draw(frame)
    L, c = D[0]
    print('draw 0 (lista %d): c[91]=%s c[92]=%s' % (L, c[91], c[92]))
    print('c[0..3] (projecao?):')
    for i in range(4):
        print(' ', c[i])
    print('c[4..6] (view?):')
    for i in range(4, 7):
        print(' ', c[i])
    print('c[8..10] (world?):')
    for i in range(8, 11):
        print(' ', c[i])
    print('c[11]=', c[11], 'c[18]=', c[18])
