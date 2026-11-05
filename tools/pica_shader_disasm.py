"""Desmontador do vertex/geometry shader PICA200 (instrucoes de 32 bits + tabela de operand descriptors).
Uso: python pica_shader_disasm.py [shader_code.bin] [shader_opdescs.bin]
Sem argumentos, le data/gpu/shader_code.bin e data/gpu/shader_opdescs.bin.

Encoding verificado contra o header do nihstro (github.com/neobrain/nihstro, shader_bytecode.h, BSD-3-Clause),
que por sua vez documenta o "Shader Instruction Set" do 3dbrew. Pontos marcados como HIPOTESE abaixo nao
estao 100% confirmados pelo codigo-fonte consultado (so por comentarios/TODOs dele) — ver docs/GPU-PICA.md
regra 1: nao advinhar formato, validar contra execucao real quando possivel.

Registradores: v0-v15 input, r0-r15 temporario, c0-c?? uniforme float (c = valor-0x20), o0-o15 saida.
Swizzle: 4 campos de 2 bits (selector_0..3, LSB->MSB dentro do byte) mapeiam, por posicao, os componentes
de saida x,y,z,w para um dos 4 componentes da fonte (enum x=0,y=1,z=2,w=3). dest_mask: bit3=x .. bit0=w.

A funcao decode() devolve um dict estruturado (reaproveitado por pica_shader_to_glsl.py); disasm_one()
so formata esse dict como texto.
"""
import struct, sys, os

HERE = os.path.dirname(os.path.abspath(__file__))
DEFAULT_CODE = os.path.join(HERE, "..", "data", "gpu", "shader_code.bin")
DEFAULT_OPDESC = os.path.join(HERE, "..", "data", "gpu", "shader_opdescs.bin")

SEL = "xyzw"
CMPOPS = ["==", "!=", "<", "<=", ">", ">=", "unk6", "unk7"]
IDXNAMES = ["", "a0.x", "a0.y", "aL"]

# (mnemonico, formato) por opcode.
OPS = {
    0x00: ("add", "arith2"), 0x01: ("dp3", "arith2"), 0x02: ("dp4", "arith2"),
    0x03: ("dph", "arith2"), 0x04: ("dst", "arith2"),
    0x05: ("ex2", "arith1"), 0x06: ("lg2", "arith1"), 0x07: ("lit", "arith1"),
    0x08: ("mul", "arith2"), 0x09: ("sge", "arith2"), 0x0a: ("slt", "arith2"),
    0x0b: ("flr", "arith1"),
    0x0c: ("max", "arith2"), 0x0d: ("min", "arith2"),
    0x0e: ("rcp", "arith1"), 0x0f: ("rsq", "arith1"),
    0x12: ("mova", "mova"), 0x13: ("mov", "arith1"),
    0x18: ("dphi", "arith2i"), 0x19: ("dsti", "arith2i"),
    0x1a: ("sgei", "arith2i"), 0x1b: ("slti", "arith2i"),
    0x20: ("break", "trivial"), 0x21: ("nop", "trivial"), 0x22: ("end", "trivial"),
    0x23: ("breakc", "flow2"), 0x24: ("call", "flow2call"), 0x25: ("callc", "flow2"),
    0x26: ("callu", "flow3"), 0x27: ("ifu", "flow3"), 0x28: ("ifc", "flow2"),
    0x29: ("loop", "flow3"), 0x2a: ("emit", "trivial"), 0x2b: ("setemit", "setemit"),
    0x2c: ("jmpc", "flow2"), 0x2d: ("jmpu", "flow3"),
}


def src_name(v):
    if v < 0x10: return "v%d" % v
    if v < 0x20: return "r%d" % (v - 0x10)
    return "c%d" % (v - 0x20)


def dest_name(v):
    if v < 0x10: return "o%d" % v
    return "r%d" % (v - 0x10)


def swizzle4(desc, sel0_bit):
    """4 selectors de 2 bits; 'sel0_bit' e a posicao de selector_0 (mapeia a saida .x).
    Os demais (selector_1=.y, selector_2=.z, selector_3=.w) ficam nos 2 bits seguintes
    *abaixo* de sel0_bit (ver layout do SwizzlePattern no cabecalho do nihstro)."""
    sx = (desc >> sel0_bit) & 3
    sy = (desc >> (sel0_bit - 2)) & 3
    sz = (desc >> (sel0_bit - 4)) & 3
    sw = (desc >> (sel0_bit - 6)) & 3
    return SEL[sx] + SEL[sy] + SEL[sz] + SEL[sw]


def mask4(desc):
    """dest_mask (bits0-3): bit3=x bit2=y bit1=z bit0=w. So os componentes escritos aparecem."""
    return "".join(c for c, bit in zip("xyzw", (3, 2, 1, 0)) if (desc >> bit) & 1) or "----"


def operand(idx_, opdescs):
    return opdescs[idx_] if idx_ < len(opdescs) else 0


def decode(addr, w, opdescs):
    """Decodifica uma instrucao em um dict estruturado. Campos sempre presentes: addr,raw,opc,name,fmt."""
    opc = (w >> 26) & 0x3f
    name, fmt = OPS.get(opc, (None, None))
    if 0x2e <= opc <= 0x2f:
        name, fmt = "cmp", "cmp"
    elif 0x30 <= opc <= 0x37:
        name, fmt = "madi", "mad"
    elif 0x38 <= opc <= 0x3f:
        name, fmt = "mad", "mad"

    d = {"addr": addr, "raw": w, "opc": opc, "name": name, "fmt": fmt}
    if fmt is None:
        return d

    if fmt in ("arith1", "arith2", "mova", "cmp"):
        desc_id = w & 0x7f
        src2 = (w >> 7) & 0x1f
        src1 = (w >> 12) & 0x7f
        idx = (w >> 19) & 3
        dest = (w >> 21) & 0x1f
        desc = operand(desc_id, opdescs)
        d.update(src1=src1, src2=src2, idx=idx, dest=dest, desc=desc,
                 neg1=(desc >> 4) & 1, neg2=(desc >> 13) & 1,
                 sw1=swizzle4(desc, 11), sw2=swizzle4(desc, 20), mask=mask4(desc))
        if fmt == "cmp":
            d.update(opy=(w >> 21) & 7, opx=(w >> 24) & 7)
        return d

    if fmt == "arith2i":  # fontes trocadas / invertidas (DPHI/DSTI/SGEI/SLTI)
        desc_id = w & 0x7f
        src2 = (w >> 7) & 0x7f
        src1 = (w >> 14) & 0x1f
        idx = (w >> 19) & 3
        dest = (w >> 21) & 0x1f
        desc = operand(desc_id, opdescs)
        d.update(src1=src1, src2=src2, idx=idx, dest=dest, desc=desc,
                 neg1=(desc >> 4) & 1, neg2=(desc >> 13) & 1,
                 sw1=swizzle4(desc, 11), sw2=swizzle4(desc, 20), mask=mask4(desc))
        return d

    if fmt == "mad":
        desc_id = w & 0x1f
        inv = name == "madi"
        if inv:
            src3 = (w >> 5) & 0x7f
            src2 = (w >> 12) & 0x1f
        else:
            src3 = (w >> 5) & 0x1f
            src2 = (w >> 10) & 0x7f
        src1 = (w >> 17) & 0x1f
        idx = (w >> 22) & 3
        dest = (w >> 24) & 0x1f
        desc = operand(desc_id, opdescs)
        d.update(src1=src1, src2=src2, src3=src3, idx=idx, dest=dest, desc=desc,
                 neg1=(desc >> 4) & 1, neg2=(desc >> 13) & 1, neg3=(desc >> 22) & 1,
                 sw1=swizzle4(desc, 11), sw2=swizzle4(desc, 20), sw3=swizzle4(desc, 29),
                 mask=mask4(desc))
        return d

    if fmt in ("flow2", "flow2call", "flow3"):
        num = w & 0xff
        dest_off = (w >> 10) & 0xfff
        refy = (w >> 24) & 1
        refx = (w >> 25) & 1
        op2 = (w >> 22) & 3          # flow2: combinador de condicao (ver nota em pica_shader_to_glsl.py)
        bool_id = (w >> 22) & 0xf    # flow3 (ifu/callu)
        int_id = (w >> 22) & 3       # flow3 (loop)
        d.update(num=num, dest_off=dest_off, refy=refy, refx=refx, op2=op2,
                 bool_id=bool_id, int_id=int_id)
        return d

    if fmt == "setemit":
        d.update(winding=(w >> 22) & 1, prim=(w >> 23) & 1, vid=(w >> 24) & 3)
        return d

    return d  # trivial: so addr/raw/opc/name/fmt bastam


def disasm_one(addr, w, opdescs):
    d = decode(addr, w, opdescs)
    name, fmt = d["name"], d["fmt"]
    label = "0x%04x" % addr
    if fmt is None:
        return label, "?? op=0x%02x raw=%08x" % (d["opc"], w)

    if fmt in ("arith1", "arith2", "mova", "cmp", "arith2i"):
        neg1 = "-" if d["neg1"] else ""
        neg2 = "-" if d["neg2"] else ""
        idx_s = ("[%s]" % IDXNAMES[d["idx"]]) if d["idx"] else ""
        s1 = "%s%s.%s" % (neg1, src_name(d["src1"]), d["sw1"])
        s2 = "%s%s.%s" % (neg2, src_name(d["src2"]), d["sw2"])
        if fmt == "mova":
            txt = "mova a0, %s%s" % (s1, idx_s)
        elif fmt == "cmp":
            txt = "cmp cc.xy, %s%s, %s, %s %s" % (s1, idx_s, s2, CMPOPS[d["opx"]], CMPOPS[d["opy"]])
        elif fmt == "arith1":
            txt = "%s %s.%s, %s%s" % (name, dest_name(d["dest"]), d["mask"], s1, idx_s)
        elif fmt == "arith2i":
            txt = "%s %s.%s, %s, %s%s" % (name, dest_name(d["dest"]), d["mask"], s1, s2, idx_s)
        else:  # arith2
            txt = "%s %s.%s, %s%s, %s" % (name, dest_name(d["dest"]), d["mask"], s1, idx_s, s2)
        return label, txt

    if fmt == "mad":
        neg1 = "-" if d["neg1"] else ""
        neg2 = "-" if d["neg2"] else ""
        neg3 = "-" if d["neg3"] else ""
        idx_s = ("[%s]" % IDXNAMES[d["idx"]]) if d["idx"] else ""
        txt = "%s %s.%s, %s%s.%s%s, %s%s.%s, %s%s.%s" % (
            name, dest_name(d["dest"]), d["mask"], neg1, src_name(d["src1"]), d["sw1"], idx_s,
            neg2, src_name(d["src2"]), d["sw2"], neg3, src_name(d["src3"]), d["sw3"])
        return label, txt

    if fmt in ("flow2", "flow2call", "flow3"):
        if fmt == "flow2call":
            txt = "call 0x%04x (%d instr)" % (d["dest_off"], d["num"])
        elif name == "loop":
            txt = "loop i%d, dest=0x%04x" % (d["int_id"], d["dest_off"])
        elif name in ("ifu", "callu"):
            txt = "%s b%d%s dest=0x%04x (%d instr)" % (
                name, d["bool_id"], " else" if name == "ifu" else "", d["dest_off"], d["num"])
        else:  # breakc/callc/ifc/jmpc
            txt = "%s cond(op=%d,refx=%d,refy=%d) dest=0x%04x (%d instr)" % (
                name, d["op2"], d["refx"], d["refy"], d["dest_off"], d["num"])
        return label, txt

    if fmt == "trivial":
        return label, name

    if fmt == "setemit":
        return label, "setemit vtx=%d prim_emit=%d winding=%d" % (d["vid"], d["prim"], d["winding"])

    return label, "?? fmt=%s" % fmt


def load_words(path):
    data = open(path, "rb").read()
    n = len(data) // 4
    return list(struct.unpack("<%dI" % n, data[:n * 4]))


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    code_path = args[0] if len(args) > 0 else DEFAULT_CODE
    opdesc_path = args[1] if len(args) > 1 else DEFAULT_OPDESC
    code = load_words(code_path)
    opdescs = load_words(opdesc_path)
    print("# %d instrucoes, %d operand descriptors" % (len(code), len(opdescs)))
    for addr, w in enumerate(code):
        label, txt = disasm_one(addr, w, opdescs)
        print("%s: %08x  %s" % (label, w, txt))


if __name__ == "__main__":
    main()
