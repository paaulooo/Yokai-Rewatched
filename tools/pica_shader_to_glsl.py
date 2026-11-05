"""Traduz o shader PICA200 ja desmontado (pica_shader_disasm.decode) para GLSL (#version 450, alvo Vulkan
via glslangValidator/shaderc -> SPIR-V). Uso: python pica_shader_to_glsl.py [shader_code.bin] [shader_opdescs.bin]
Escreve um .vert por "programa" encontrado em work/shaders/ (um shader_code.bin pode conter varios programas
concatenados, um por entry point/upload do GSP; cada um termina em END e e seguido de um NOP de padding).

So cobre os opcodes vistos de fato em data/gpu/shader_code.bin (ver docs/GPU-PICA.md): add, cmp, dp3, dp4, flr,
ifc, mad, madi, max, min, mov, mova, mul, rcp, rsq, end/nop. Opcodes fora dessa lista (loop, call, ifu, jmp,
dph*, ex2/lg2/lit, sge/slt...) geram um comentario TODO em vez de codigo adivinhado — regra 1 do CLAUDE.md:
nao advinhar formato sem dado real para validar.

Semantica de IFC confirmada batendo o ponto de uniao contra o proprio desmontado (nao e so suposicao):
olhando os 4 blocos IFC reais do shader, o bloco "verdadeiro" ocupa [ifc_addr+1 .. dest_off-1] e o bloco
"falso" ocupa [dest_off .. dest_off+num-1]; depois do if/else a execucao sempre continua em dest_off+num
(confirmado 3x: o codigo que comeca ali e sempre uma instrucao NOVA e distinta da que vem antes do ifc,
nos 3 blocos da parte de iluminacao). Condicao: combinador op (0=Or,1=And,2=JustX,3=JustY - ordem do enum
*assumida*, nao lida em codigo-fonte) com refx/refy negando cc.x/cc.y antes de combinar; nos 4 casos reais
op=2 (JustX) e cc.x==cc.y sempre (cmp usa os mesmos operandos em ambos os canais), entao o resultado da
formula nao depende da ordem exata do enum aqui - mas fica marcado como HIPOTESE para quando aparecer um
caso onde cc.x != cc.y.
"""
import os, sys
from pica_shader_disasm import decode, load_words, src_name, dest_name, DEFAULT_CODE, DEFAULT_OPDESC

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_DIR = os.path.join(HERE, "..", "work", "shaders")
CMPOPS_GLSL = ["==", "!=", "<", "<=", ">", ">="]


def split_reg(name):
    """'v3'/'r5'/'o2'/'c18' -> ('v',3) etc., mesma convencao do desmontador."""
    return name[0], int(name[1:])


def reg_glsl(kind, idx):
    return {"v": "v%d", "r": "r%d", "o": "o%d", "c": "c[%d]"}[kind] % idx


def sub_swizzle(sw, mask):
    """Letras do swizzle (saida x,y,z,w) que alimentam os componentes de 'mask', na ordem de 'mask'."""
    pos = {"x": 0, "y": 1, "z": 2, "w": 3}
    return "".join(sw[pos[c]] for c in mask)


# Enderecamento relativo (a0.x/a0.y/aL, campo 'idx'): a qual fonte ele se aplica depende do formato
# (comentario do nihstro: "src1" no formato comum; "src1/src2" no formato invertido 1i; "src2/src3" no
# MAD; a metade exata usada no MADI/1i e HIPOTESE, nao observada em data/gpu/shader_code.bin).
def idx_target(d):
    fmt, name = d["fmt"], d["name"]
    if fmt in ("arith1", "arith2", "cmp", "mova"): return 1
    if fmt == "arith2i": return 2
    if fmt == "mad": return 3 if name == "madi" else 2
    return None


IDX_VAR = {1: "a0x", 2: "a0y", 3: "aL"}


class Translator:
    def __init__(self, code, opdescs):
        self.code = code
        self.opdescs = opdescs
        self.used_v, self.used_r, self.used_o, self.used_c = set(), set(), set(), set()
        self.lines = []
        self.indent = 1

    def emit(self, s):
        self.lines.append("    " * self.indent + s)

    def use(self, raw_name, apply_idx=False, idx_sel=0):
        kind, idx = split_reg(raw_name)
        {"v": self.used_v, "r": self.used_r, "o": self.used_o, "c": self.used_c}[kind].add(idx)
        if apply_idx and kind == "c":
            return "c[%d + %s]" % (idx, IDX_VAR[idx_sel])
        return reg_glsl(kind, idx)

    def operand(self, d, which, mask):
        """Expressao GLSL para a fonte 'which' (1,2,3), reduzida aos componentes de 'mask'."""
        raw = src_name(d["src%s" % which])
        rel = d["idx"] and which == idx_target(d)
        reg = self.use(raw, rel, d["idx"])
        sw = sub_swizzle(d["sw%s" % which], mask)
        neg = "-" if d["neg%s" % which] else ""
        return "%s%s.%s" % (neg, reg, sw)

    def component(self, d, which, i):
        """Expressao GLSL do i-esimo componente (0=x..3=w) da fonte 'which', sem reduzir por mascara
        (usado por cmp/mova, que nao tem campo de mascara de destino)."""
        raw = src_name(d["src%s" % which])
        rel = d["idx"] and which == idx_target(d)
        reg = self.use(raw, rel, d["idx"])
        letter = d["sw%s" % which][i]
        neg = "-" if d["neg%s" % which] else ""
        return "%s%s.%s" % (neg, reg, letter)

    def dest(self, d):
        return self.use(dest_name(d["dest"]))

    def translate_range(self, start, end_excl):
        """Traduz instrucoes [start, end_excl) em linha reta, recursando em blocos IFC."""
        pc = start
        while pc < end_excl:
            d = decode(pc, self.code[pc], self.opdescs)
            name, fmt = d["name"], d["fmt"]

            if fmt == "trivial":  # nop/break/emit/end: sem efeito na traducao linear
                pc += 1
                continue

            if fmt == "cmp":
                x1, x2 = self.component(d, 1, 0), self.component(d, 2, 0)
                y1, y2 = self.component(d, 1, 1), self.component(d, 2, 1)
                self.emit("cc_x = (%s %s %s);" % (x1, CMPOPS_GLSL[d["opx"]], x2))
                self.emit("cc_y = (%s %s %s);" % (y1, CMPOPS_GLSL[d["opy"]], y2))
                pc += 1
                continue

            if fmt == "mova":
                self.emit("a0x = int(%s); a0y = int(%s);  // arredondamento exato ainda nao confirmado" % (
                    self.component(d, 1, 0), self.component(d, 1, 1)))
                pc += 1
                continue

            if fmt in ("arith1", "arith2", "arith2i", "mad"):
                dst = self.dest(d)
                mask = d["mask"]
                n = len(mask)
                s1 = self.operand(d, 1, mask)
                if name == "mov":
                    rhs = s1
                elif name == "flr":
                    rhs = "floor(%s)" % s1
                elif name in ("rcp", "rsq"):
                    scalar = self.component(d, 1, 0)
                    fn = ("1.0/%s" % scalar) if name == "rcp" else ("1.0/sqrt(%s)" % scalar)
                    rhs = fn if n == 1 else "vec%d(%s)" % (n, fn)
                elif name in ("dp3", "dp4"):
                    full = "xyzw" if name == "dp4" else "xyz"
                    s1f = self.operand(d, 1, full)
                    s2f = self.operand(d, 2, full)
                    fn = "dot(%s, %s)" % (s1f, s2f)
                    rhs = fn if n == 1 else "vec%d(%s)" % (n, fn)
                elif name in ("add", "mul", "max", "min"):
                    s2 = self.operand(d, 2, mask)
                    rhs = {"add": "%s + %s" % (s1, s2), "mul": "%s * %s" % (s1, s2),
                           "max": "max(%s, %s)" % (s1, s2), "min": "min(%s, %s)" % (s1, s2)}[name]
                elif name in ("mad", "madi"):
                    s2 = self.operand(d, 2, mask)
                    s3 = self.operand(d, 3, mask)
                    rhs = "%s * %s + %s" % (s1, s2, s3)
                else:
                    self.emit("// TODO opcode '%s' nao implementado (nao visto em shader_code.bin ainda)" % name)
                    pc += 1
                    continue
                self.emit("%s.%s = %s;" % (dst, mask, rhs))
                pc += 1
                continue

            if fmt == "flow2" and name == "ifc":
                true_start, true_end = pc + 1, d["dest_off"]            # bloco verdadeiro = [pc+1, dest_off)
                false_start, false_end = d["dest_off"], d["dest_off"] + d["num"]  # falso = [dest_off, dest_off+num)
                self.emit("if (%s) {" % self.ifc_cond(d))
                self.indent += 1
                self.translate_range(true_start, true_end)
                self.indent -= 1
                self.emit("} else {")
                self.indent += 1
                self.translate_range(false_start, false_end)
                self.indent -= 1
                self.emit("}")
                pc = false_end  # ponto de uniao, confirmado contra o desmontado real
                continue

            self.emit("// TODO fluxo de controle '%s' nao implementado (nao visto em shader_code.bin ainda)" % name)
            pc += 1

    def ifc_cond(self, d):
        # HIPOTESE na ordem exata do enum (ver docstring do modulo); op=2 (JustX) observado em todos os casos reais.
        x = "cc_x" if d["refx"] else "!cc_x"
        y = "cc_y" if d["refy"] else "!cc_y"
        return {0: "(%s || %s)" % (x, y), 1: "(%s && %s)" % (x, y), 2: x, 3: y}.get(d["op2"], x)


def find_programs(code):
    """Um shader_code.bin pode ter varios programas concatenados (um END por programa, seguido
    de 1 NOP de padding). Devolve lista de (start, end_inclusive_do_END) por programa."""
    progs = []
    start = 0
    i = 0
    n = len(code)
    while i < n:
        opc = (code[i] >> 26) & 0x3f
        if opc == 0x22:  # END
            progs.append((start, i))
            i += 1
            if i < n and ((code[i] >> 26) & 0x3f) == 0x21:  # NOP de padding
                i += 1
            start = i
        else:
            i += 1
    return progs


def gen_vert(code, opdescs, start, end_incl):
    tr = Translator(code, opdescs)
    tr.translate_range(start, end_incl)  # nao inclui o proprio END (sem efeito)
    out = [
        "#version 450",
        "// gerado por tools/pica_shader_to_glsl.py a partir de data/gpu/shader_code.bin[0x%04x:0x%04x]" % (start, end_incl),
        "// mapeamento o0..o6 -> semantica (posicao/cor/uv/normal) e HIPOTESE: falta ler VSH_OUTMAP_*.",
    ]
    max_c = (max(tr.used_c) + 1) if tr.used_c else 1
    out.append("layout(set=0, binding=0) uniform Uniforms { vec4 c[%d]; };" % max(max_c, 96))
    for i in sorted(tr.used_v):
        out.append("layout(location=%d) in vec4 v%d;" % (i, i))
    out.append("")
    for i in sorted(tr.used_o):
        out.append("layout(location=%d) out vec4 o%d_out;" % (i, i))
    out.append("")
    for i in sorted(tr.used_r):
        out.append("vec4 r%d = vec4(0.0);" % i)
    for i in sorted(tr.used_o):
        out.append("vec4 o%d = vec4(0.0);" % i)
    out.append("bool cc_x = false, cc_y = false;")
    out.append("int a0x = 0, a0y = 0, aL = 0;")
    out.append("")
    out.append("void main() {")
    out.extend(tr.lines)
    for i in sorted(tr.used_o):
        out.append("    o%d_out = o%d;" % (i, i))
    out.append("    gl_Position = o0;  // HIPOTESE: o0 = posicao de clip (bate com a cadeia de dp4 observada)")
    out.append("}")
    return "\n".join(out) + "\n"


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    code_path = args[0] if len(args) > 0 else DEFAULT_CODE
    opdesc_path = args[1] if len(args) > 1 else DEFAULT_OPDESC
    code = load_words(code_path)
    opdescs = load_words(opdesc_path)
    progs = find_programs(code)
    print("# %d programa(s) encontrados em %s" % (len(progs), code_path))
    os.makedirs(OUT_DIR, exist_ok=True)
    for n, (start, end) in enumerate(progs):
        src = gen_vert(code, opdescs, start, end)
        path = os.path.join(OUT_DIR, "prog%d_0x%04x_0x%04x.vert" % (n, start, end))
        open(path, "w").write(src)
        print("  prog%d: 0x%04x..0x%04x (%d instr) -> %s" % (n, start, end, end - start, path))


if __name__ == "__main__":
    main()
