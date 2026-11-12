//! Tradução AOT de um programa de vértice do PICA200 (código + descritores + ponto de entrada) para GLSL, com
//! exatamente a semântica do interpretador do motor (`engine/src/pica/shader.rs` e o ubershader
//! `engine/src/render/shaders/pica_vs.vert`).
//!
//! O shader gerado é o próprio `pica_vs.vert` com o trecho do interpretador (entre `@@AOT-BEGIN` e `@@AOT-END`)
//! trocado pelo programa traduzido: `run()`, `loadInputs()` e `outComp()`. Interface, leitura dos atributos,
//! auxiliares aritméticos (`pmul`, `dot3p`, `cond`, `cmpOp`…), pilha de CALL/IF/LOOP (`push`, `brk`), saídas e
//! encaixe 12.4 continuam os do ubershader, então a única coisa traduzida é a decodificação de cada instrução — que
//! aqui vira código com os campos constantes (registradores, swizzles, máscaras e descritores embutidos).
//!
//! Três formas (ideia do Zakuro, `crates/zakuro-gpu/src/shader/glsl.rs`, adaptada à nossa semântica):
//! - **straight**: sem instrução de fluxo alcançável → código reto até o END;
//! - **struct**: só IFU/IFC e CALL/CALLC/CALLU bem aninhados (nenhum bloco interno passa do fim do externo, pilha
//!   ≤ 16) → if/else e blocos aninhados. Equivale à pilha do interpretador nesses casos: o quadro do topo termina
//!   exatamente no fim do bloco, e sem laços o limite de 100 000 passos nunca é atingido;
//! - **dispatch**: qualquer outro fluxo (LOOP, BREAK/BREAKC, JMPC/JMPU, aninhamento torto) → laço com `switch` por
//!   "trechos" (sequências sem entrada no meio) e a MESMA pilha do interpretador entre eles. Diferenças deliberadas
//!   para o Zakuro, para bater com o nosso: pilha cheia descarta o quadro mais antigo, BREAK sem laço não mexe na
//!   pilha, o fim do bloco "então" de IF é o destino (aritmética sem sinal com volta), e o limite de passos é contado
//!   instrução a instrução como no interpretador (só as escritas em registrador de saída precisam ser protegidas:
//!   o resto não é observável depois do `return`).
//!
//! Também devolve as palavras de código e os descritores de que a tradução depende (tudo o que a execução pode
//! alcançar): o motor só usa o AOT se elas forem iguais às carregadas no PICA.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;

pub const CODE_WORDS: u32 = 4096;
const STACK: usize = 16;
/// limite de passos do interpretador (`shader.rs`)
const MAX_STEPS: u32 = 100_000;
/// teto do código gerado (instruções emitidas; o modo `struct` copia o corpo de cada CALL)
const EMIT_LIMIT: usize = 20_000;
// modo struct: sem laços, cada instrução (aritmética, fluxo ou fim de quadro) roda no máximo uma vez → nunca chega
// ao limite de passos, que então nem precisa ser contado
const _: () = assert!(3 * EMIT_LIMIT < MAX_STEPS as usize);
/// teto de estados (pc, pilha) explorados no modo `dispatch`
const STATES: usize = 200_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Straight,
    Struct,
    Dispatch,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Straight => "straight",
            Mode::Struct => "struct",
            Mode::Dispatch => "dispatch",
        }
    }
}

pub struct Translation {
    pub mode: Mode,
    /// trecho que substitui o interpretador no `pica_vs.vert`
    pub region: String,
    /// endereço → palavra de todo o código de que a tradução depende
    pub code_deps: BTreeMap<u32, u32>,
    /// índice → valor dos descritores usados
    pub desc_deps: BTreeMap<u32, u32>,
    /// instruções aritméticas traduzidas
    pub instructions: usize,
}

const OPNAMES: [&str; 64] = [
    "add", "dp3", "dp4", "dph", "dst", "ex2", "lg2", "litp", "mul", "sge", "slt", "flr", "max", "min", "rcp", "rsq",
    "?10", "?11", "mova", "mov", "?14", "?15", "?16", "?17", "dphi", "dsti", "sgei", "slti", "?1c", "?1d", "?1e",
    "?1f", "break", "nop", "end", "breakc", "call", "callc", "callu", "ifu", "ifc", "loop", "emit", "setemit", "jmpc",
    "jmpu", "cmp", "cmp", "madi", "madi", "madi", "madi", "madi", "madi", "madi", "madi", "mad", "mad", "mad", "mad",
    "mad", "mad", "mad", "mad",
];

fn is_arith(op: u32) -> bool {
    op <= 0x1b || op == 0x2e || op == 0x2f || op >= 0x30
}

fn is_flow(op: u32) -> bool {
    matches!(op, 0x20 | 0x23..=0x29 | 0x2c | 0x2d)
}

/// destino (12 bits) e contagem (8 bits) de uma instrução de fluxo
fn target(w: u32) -> (u32, u32) {
    ((w >> 10) & 0xfff, w & 0xff)
}

struct Gen<'a> {
    code: &'a [u32],
    desc: &'a [u32],
    code_deps: BTreeMap<u32, u32>,
    desc_deps: BTreeMap<u32, u32>,
    /// registradores de entrada lidos / de saída escritos
    read_in: u16,
    written_out: u16,
    emitted: usize,
}

impl<'a> Gen<'a> {
    fn word(&mut self, pc: u32) -> u32 {
        let w = self.code[pc as usize];
        self.code_deps.insert(pc, w);
        w
    }

    fn descr(&mut self, i: u32) -> u32 {
        let d = self.desc[i as usize];
        self.desc_deps.insert(i, d);
        d
    }

    /// registrador-fonte de 5/7 bits (`src` do ubershader); `rel` = componente de endereçamento relativo
    fn src(&mut self, r: u32, rel: Option<&str>) -> String {
        if r < 0x10 {
            self.read_in |= 1 << r;
            format!("vin[{r}]")
        } else if r < 0x20 {
            format!("tmp[{}]", r - 0x10)
        } else {
            let k = r - 0x20; // 0..95: com rel = 0 sempre dentro das 96 constantes
            match rel {
                None => format!("s.cf[{k}]"),
                Some(a) => format!("unif({k} + {a})"),
            }
        }
    }

    /// destino de 5 bits com máscara (bit 3 = x)
    fn write(&mut self, dest: u32, mask: u32, out: &mut String, ind: &str) {
        let comps: String = (0..4).filter(|k| mask >> (3 - k) & 1 != 0).map(|k| b"xyzw"[k] as char).collect();
        if comps.is_empty() {
            return;
        }
        let t = if dest < 0x10 {
            self.written_out |= 1 << dest;
            format!("vout[{dest}]")
        } else {
            format!("tmp[{}]", dest - 0x10)
        };
        if comps.len() == 4 {
            let _ = writeln!(out, "{ind}    {t} = r;");
        } else {
            let _ = writeln!(out, "{ind}    {t}.{comps} = r.{comps};");
        }
    }

    /// A instrução aritmética em `pc` (formato 1 / 1i / CMP / MAD / MADI). Devolve se escreve num registrador de
    /// saída (o modo `dispatch` protege essas escritas com o limite de passos).
    fn arith(&mut self, pc: u32, out: &mut String, ind: &str) -> bool {
        let w = self.word(pc);
        let op = w >> 26;
        self.emitted += 1;
        let rel_of = |i: u32| match i {
            1 => Some("a0.x"),
            2 => Some("a0.y"),
            3 => Some("al"),
            _ => None,
        };
        let swz = |e: String, sel: u32, neg: bool| {
            let c: String = [6, 4, 2, 0].iter().map(|s| b"xyzw"[((sel >> s) & 3) as usize] as char).collect();
            let e = if c == "xyzw" { e } else { format!("{e}.{c}") };
            if neg {
                format!("-{e}")
            } else {
                e
            }
        };
        let mut body = String::new();
        let mut writes_out = false;
        if op >= 0x30 {
            let d = self.descr(w & 0x1f);
            let rel = rel_of((w >> 22) & 3);
            let (r1, r2, r3) = if op >= 0x38 {
                (self.src((w >> 17) & 0x1f, None), self.src((w >> 10) & 0x7f, rel), self.src((w >> 5) & 0x1f, None))
            } else {
                (self.src((w >> 17) & 0x1f, None), self.src((w >> 12) & 0x1f, None), self.src((w >> 5) & 0x7f, rel))
            };
            let dest = (w >> 24) & 0x1f;
            let mask = d & 0xf;
            if mask == 0 {
                return false;
            }
            let _ = writeln!(body, "{ind}    vec4 s1 = {};", swz(r1, (d >> 5) & 0xff, d >> 4 & 1 != 0));
            let _ = writeln!(body, "{ind}    vec4 s2 = {};", swz(r2, (d >> 14) & 0xff, d >> 13 & 1 != 0));
            let _ = writeln!(body, "{ind}    vec4 s3 = {};", swz(r3, (d >> 23) & 0xff, d >> 22 & 1 != 0));
            let _ = writeln!(body, "{ind}    precise vec4 r = pmul4(s1, s2) + s3;");
            writes_out = dest < 0x10;
            self.write(dest, mask, &mut body, ind);
        } else {
            let d = self.descr(w & 0x7f);
            let inv = (0x18..=0x1b).contains(&op);
            let rel = rel_of((w >> 19) & 3);
            let (r1, r2) = if inv {
                (self.src((w >> 14) & 0x1f, None), self.src((w >> 7) & 0x7f, rel))
            } else {
                (self.src((w >> 12) & 0x7f, rel), self.src((w >> 7) & 0x1f, None))
            };
            let s1 = swz(r1, (d >> 5) & 0xff, d >> 4 & 1 != 0);
            let s2 = swz(r2, (d >> 14) & 0xff, d >> 13 & 1 != 0);
            let mask = d & 0xf;
            let dest = (w >> 21) & 0x1f;
            let expr: Option<String> = match op {
                0x00 => Some("s1 + s2".into()),
                0x01 => Some("vec4(dot3p(s1, s2))".into()),
                0x02 => Some("vec4(dot4p(s1, s2))".into()),
                0x03 | 0x18 => Some("vec4(t)".into()),
                0x04 | 0x19 => Some("vec4(1.0, pmul(s1.y, s2.y), s1.z, s2.w)".into()),
                0x05 => Some("vec4(exp2(s1.x))".into()),
                0x06 => Some("vec4(log2(s1.x))".into()),
                0x07 => Some("vec4(max(s1.x, 0.0), clamp(s1.y, -127.9961, 127.9961), s1.z, max(s1.w, 0.0))".into()),
                0x08 => Some("pmul4(s1, s2)".into()),
                0x09 | 0x1a => Some("vec4(greaterThanEqual(s1, s2))".into()),
                0x0a | 0x1b => Some("vec4(lessThan(s1, s2))".into()),
                0x0b => Some("floor(s1)".into()),
                0x0c => Some(
                    "vec4(s1.x > s2.x ? s1.x : s2.x, s1.y > s2.y ? s1.y : s2.y, s1.z > s2.z ? s1.z : s2.z, s1.w > s2.w ? s1.w : s2.w)"
                        .into(),
                ),
                0x0d => Some(
                    "vec4(s1.x < s2.x ? s1.x : s2.x, s1.y < s2.y ? s1.y : s2.y, s1.z < s2.z ? s1.z : s2.z, s1.w < s2.w ? s1.w : s2.w)"
                        .into(),
                ),
                0x0e => Some("vec4(1.0 / s1.x)".into()),
                0x0f => Some("vec4(1.0 / sqrt(s1.x))".into()),
                0x13 => Some("s1".into()),
                _ => None,
            };
            match op {
                // MOVA: a0.x/a0.y ← int(s1.x/s1.y)
                0x12 => {
                    if mask & 0xc == 0 {
                        return false;
                    }
                    let _ = writeln!(body, "{ind}    vec4 s1 = {s1};");
                    if mask & 8 != 0 {
                        let _ = writeln!(body, "{ind}    a0.x = toInt(s1.x);");
                    }
                    if mask & 4 != 0 {
                        let _ = writeln!(body, "{ind}    a0.y = toInt(s1.y);");
                    }
                }
                // CMP: cc ← (s1.x op_x s2.x, s1.y op_y s2.y)
                0x2e | 0x2f => {
                    let _ = writeln!(body, "{ind}    vec4 s1 = {s1};");
                    let _ = writeln!(body, "{ind}    vec4 s2 = {s2};");
                    let _ = writeln!(
                        body,
                        "{ind}    cc = bvec2(cmpOp(s1.x, s2.x, {}u), cmpOp(s1.y, s2.y, {}u));",
                        (w >> 24) & 7,
                        (w >> 21) & 7
                    );
                }
                _ => {
                    let Some(expr) = expr else { return false }; // opcodes sem efeito
                    // LITP altera cc mesmo sem máscara
                    if mask == 0 && op != 0x07 {
                        return false;
                    }
                    let _ = writeln!(body, "{ind}    vec4 s1 = {s1};");
                    if !matches!(op, 0x05..=0x07 | 0x0b | 0x0e | 0x0f | 0x13) {
                        let _ = writeln!(body, "{ind}    vec4 s2 = {s2};");
                    }
                    if matches!(op, 0x03 | 0x18) {
                        let _ = writeln!(body, "{ind}    precise float t = dot3p(s1, s2) + s2.w;");
                    }
                    let _ = writeln!(body, "{ind}    precise vec4 r = {expr};");
                    if op == 0x07 {
                        let _ = writeln!(body, "{ind}    cc = bvec2(s1.x > 0.0, s1.w > 0.0);");
                    }
                    writes_out = dest < 0x10 && mask != 0;
                    self.write(dest, mask, &mut body, ind);
                }
            }
        }
        let _ = writeln!(out, "{ind}{{ // {pc:03x}: {} ({w:08x})", OPNAMES[op as usize]);
        out.push_str(&body);
        let _ = writeln!(out, "{ind}}}");
        writes_out
    }

    /// condição de IFC/CALLC/BREAKC/JMPC ou o uniform bool de IFU/CALLU (auxiliares do ubershader)
    fn taken(w: u32) -> String {
        match w >> 26 {
            0x26 | 0x27 => format!("boolU({w:#010x}u)"),
            0x2d => format!("(boolU({w:#010x}u) == {})", if w & 1 == 0 { "true" } else { "false" }),
            _ => format!("cond({w:#010x}u)"),
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // modo struct

    /// Bloco de `pc` até `end` (exclusivo; None = até o END). Ok(true) = o bloco termina em `return` (END ou pc
    /// ≥ 4096); Err = fluxo que não cabe em blocos aninhados (vai para o modo dispatch).
    fn block(&mut self, out: &mut String, start: u32, end: Option<u32>, depth: usize, ind: &str) -> Result<bool, String> {
        let mut pc = start;
        loop {
            if Some(pc) == end {
                return Ok(false);
            }
            if pc >= CODE_WORDS {
                let _ = writeln!(out, "{ind}return;");
                return Ok(true);
            }
            if self.emitted > EMIT_LIMIT {
                return Err("código grande demais para blocos".into());
            }
            let w = self.code[pc as usize];
            let op = w >> 26;
            if is_arith(op) {
                self.arith(pc, out, ind);
                pc += 1;
                continue;
            }
            self.word(pc);
            match op {
                0x22 => {
                    let _ = writeln!(out, "{ind}return; // {pc:03x}: end");
                    return Ok(true);
                }
                0x27 | 0x28 => {
                    let (dst, num) = target(w);
                    let after = dst + num;
                    // então = pc+1..dst, senão = dst..dst+num, os dois dentro do bloco atual
                    if depth + 1 > STACK || dst < pc + 1 || end.is_some_and(|e| after > e) {
                        return Err(format!("IF em {pc:03x} fora de ordem"));
                    }
                    let inner = format!("{ind}    ");
                    let _ = writeln!(out, "{ind}if ({}) {{ // {pc:03x}: {}", Self::taken(w), OPNAMES[op as usize]);
                    let t1 = self.block(out, pc + 1, Some(dst), depth + 1, &inner)?;
                    let _ = writeln!(out, "{ind}}} else {{");
                    let t2 = self.block(out, dst, Some(after), depth + 1, &inner)?;
                    let _ = writeln!(out, "{ind}}}");
                    if t1 && t2 {
                        return Ok(true);
                    }
                    pc = after;
                }
                0x24..=0x26 => {
                    let (dst, num) = target(w);
                    if depth + 1 > STACK {
                        return Err(format!("CALL em {pc:03x}: pilha"));
                    }
                    let inner = format!("{ind}    ");
                    let cond = if op == 0x24 { "true".to_string() } else { Self::taken(w) };
                    let _ = writeln!(out, "{ind}if ({cond}) {{ // {pc:03x}: {} {dst:03x}+{num}", OPNAMES[op as usize]);
                    let t = self.block(out, dst, Some(dst + num), depth + 1, &inner)?;
                    let _ = writeln!(out, "{ind}}}");
                    if t && op == 0x24 {
                        return Ok(true);
                    }
                    pc += 1;
                }
                0x20 | 0x23 | 0x29 | 0x2c | 0x2d => {
                    return Err(format!("{} em {pc:03x}", OPNAMES[op as usize]));
                }
                _ => pc += 1, // NOP e afins
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // modo dispatch

    fn dispatch(&mut self, entry: u32, out: &mut String) -> Result<(), String> {
        let (pcs, flows) = explore(self.code, entry)?;
        // todo endereço em que a execução pode entrar ou em que um quadro pode terminar abre um trecho
        let mut starts = BTreeSet::from([entry]);
        for &f in &flows {
            let w = self.code[f as usize];
            let (dst, num) = target(w);
            match w >> 26 {
                0x24..=0x26 => starts.extend([dst, dst + num, f + 1]),
                0x27 | 0x28 => starts.extend([f + 1, dst, dst + num]),
                0x29 => starts.extend([f + 1, dst + 1]),
                0x2c | 0x2d => starts.extend([dst, f + 1]),
                _ => {
                    starts.insert(f + 1);
                }
            }
        }
        let _ = writeln!(out, "    uint pc = {entry}u;");
        out.push_str("    uint steps = 0u;\n");
        out.push_str("    for (;;) {\n");
        out.push_str("        if (pc >= 4096u) return;\n");
        out.push_str("        // fim do quadro do topo (custa um passo, como no interpretador)\n");
        out.push_str("        if (nfr > 0 && pc == fr_final[nfr - 1]) {\n");
        out.push_str("            steps++;\n");
        out.push_str("            if (steps > MAX_STEPS) return;\n");
        out.push_str("            int t = nfr - 1;\n");
        out.push_str("            al += fr_incr[t];\n");
        out.push_str("            if (fr_rep[t] == 0u) {\n                pc = fr_ret[t];\n                nfr--;\n            } else {\n                fr_rep[t]--;\n                pc = fr_loop[t];\n            }\n");
        out.push_str("            continue;\n        }\n");
        out.push_str("        switch (pc) {\n");
        for &s in starts.iter().filter(|s| pcs.contains(s)) {
            let mut at = s;
            let mut body = String::new();
            let mut len = 0u32;
            let ind = "            ";
            let exit = loop {
                if at >= CODE_WORDS || (at != s && starts.contains(&at)) {
                    break format!("{ind}pc = {at}u;\n");
                }
                let w = self.code[at as usize];
                let op = w >> 26;
                if is_arith(op) {
                    len += 1;
                    let mut ins = String::new();
                    if self.arith(at, &mut ins, ind) && len > 1 {
                        let _ = writeln!(body, "{ind}if (steps + {len}u > MAX_STEPS) return;");
                    }
                    body.push_str(&ins);
                    at += 1;
                    continue;
                }
                self.word(at);
                len += 1;
                if op == 0x22 {
                    break format!("{ind}return; // {at:03x}: end\n");
                }
                if is_flow(op) {
                    break self.flow(at, w, ind);
                }
                at += 1; // NOP e afins
            };
            if self.emitted > EMIT_LIMIT {
                return Err("código grande demais".into());
            }
            let _ = writeln!(out, "        case {s}u: {{ // {s:03x}");
            let _ = writeln!(out, "{}if (steps >= MAX_STEPS) return;", "            ");
            out.push_str(&body);
            let _ = writeln!(out, "            steps += {len}u;");
            out.push_str(&exit);
            out.push_str("            break;\n        }\n");
        }
        out.push_str("        default:\n            return;\n        }\n    }\n");
        Ok(())
    }

    /// uma instrução de fluxo no fim de um trecho, como o `run()` do ubershader a executa
    fn flow(&self, at: u32, w: u32, ind: &str) -> String {
        let op = w >> 26;
        let (dst, num) = target(w);
        let next = at + 1;
        let name = OPNAMES[op as usize];
        let c = Self::taken(w);
        match op {
            0x20 => format!("{ind}pc = brk({next}u); // {at:03x}: break\n"),
            0x23 => format!("{ind}pc = {c} ? brk({next}u) : {next}u; // {at:03x}: breakc\n"),
            0x24..=0x26 => {
                let cond = if op == 0x24 { "true".to_string() } else { c };
                format!("{ind}pc = {cond} ? push({dst}u, {num}u, {next}u, 0u, 0, false) : {next}u; // {at:03x}: {name}\n")
            }
            0x27 | 0x28 => {
                let then_n = dst.wrapping_sub(at).wrapping_sub(1);
                let after = dst + num;
                format!(
                    "{ind}pc = {c} ? push({next}u, {then_n}u, {after}u, 0u, 0, false) : push({dst}u, {num}u, {after}u, 0u, 0, false); // {at:03x}: {name}\n"
                )
            }
            0x29 => {
                let k = (w >> 22) & 3;
                let n = dst.wrapping_sub(at);
                format!(
                    "{ind}{{ // {at:03x}: loop\n{ind}    ivec4 iu = s.ci[{k}];\n{ind}    al = iu.y;\n{ind}    pc = push({next}u, {n}u, {}u, uint(iu.x), iu.z, true);\n{ind}}}\n",
                    dst + 1
                )
            }
            _ => format!("{ind}pc = {c} ? {dst}u : {next}u; // {at:03x}: {name}\n"),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct Open {
    end: u32,
    back: u32,
    start: u32,
    is_loop: bool,
}

/// empilha como `Stack::push`: cheia → descarta o quadro mais antigo
fn pushed(mut open: Vec<Open>, f: Open) -> Vec<Open> {
    if open.len() == STACK {
        open.remove(0);
    }
    open.push(f);
    open
}

/// BREAK: sai do laço mais interno (e do que está aberto dentro dele); sem laço, segue sem mexer na pilha
fn broken(mut open: Vec<Open>, next: u32) -> (u32, Vec<Open>) {
    match open.iter().rposition(|f| f.is_loop) {
        Some(i) => {
            let back = open[i].back;
            open.truncate(i);
            (back, open)
        }
        None => (next, open),
    }
}

/// Segue a execução a partir da entrada com a pilha que ela teria, por todos os caminhos que condições e contagens
/// de laço permitem (ideia do `reachable` do Zakuro). Devolve os endereços executados e os de instruções de fluxo.
fn explore(code: &[u32], entry: u32) -> Result<(BTreeSet<u32>, BTreeSet<u32>), String> {
    let mut pcs = BTreeSet::new();
    let mut flows = BTreeSet::new();
    let mut seen: HashSet<(u32, Vec<Open>)> = HashSet::new();
    let mut pending: Vec<(u32, Vec<Open>)> = vec![(entry, Vec::new())];
    while let Some((pc, open)) = pending.pop() {
        if pc >= CODE_WORDS || !seen.insert((pc, open.clone())) {
            continue;
        }
        if seen.len() > STATES {
            return Err(format!("fluxo com mais de {STATES} estados"));
        }
        if let Some(top) = open.last().filter(|t| t.end == pc).cloned() {
            if top.is_loop {
                pending.push((top.start, open.clone()));
            }
            let mut o = open;
            o.pop();
            pending.push((top.back, o));
            continue;
        }
        pcs.insert(pc);
        let w = code[pc as usize];
        let op = w >> 26;
        let (dst, num) = target(w);
        let next = pc + 1;
        if is_flow(op) {
            flows.insert(pc);
        }
        match op {
            0x22 => {}
            0x20 => pending.push(broken(open, next)),
            0x23 => {
                pending.push(broken(open.clone(), next));
                pending.push((next, open));
            }
            0x24..=0x26 => {
                let f = Open { end: dst + num, back: next, start: dst, is_loop: false };
                if op != 0x24 {
                    pending.push((next, open.clone()));
                }
                pending.push((dst, pushed(open, f)));
            }
            0x27 | 0x28 => {
                let after = dst + num;
                let then_end = next.wrapping_add(dst.wrapping_sub(pc).wrapping_sub(1));
                pending.push((next, pushed(open.clone(), Open { end: then_end, back: after, start: next, is_loop: false })));
                pending.push((dst, pushed(open, Open { end: after, back: after, start: dst, is_loop: false })));
            }
            0x29 => pending.push((next, pushed(open, Open { end: dst + 1, back: dst + 1, start: next, is_loop: true }))),
            0x2c | 0x2d => {
                pending.push((dst, open.clone()));
                pending.push((next, open));
            }
            _ => pending.push((next, open)),
        }
    }
    Ok((pcs, flows))
}

/// Traduz o programa (`code` com 4096 palavras, `desc` com 128). `force_dispatch` gera sempre o modo dispatch
/// (variante de teste: tem de dar o mesmo resultado).
pub fn translate(code: &[u32], desc: &[u32], entry: u32, force_dispatch: bool) -> Result<Translation, String> {
    assert!(code.len() == CODE_WORDS as usize && desc.len() == 128);
    if entry >= CODE_WORDS {
        return Err("entrada fora do código".into());
    }
    let new = || Gen {
        code,
        desc,
        code_deps: BTreeMap::new(),
        desc_deps: BTreeMap::new(),
        read_in: 0,
        written_out: 0,
        emitted: 0,
    };
    let mut run = String::new();
    let mut g = new();
    let mut mode = Mode::Straight;
    let structured = if force_dispatch { Err(String::new()) } else { g.block(&mut run, entry, None, 0, "    ") };
    if structured.is_ok() {
        let flows = g.code_deps.iter().any(|(_, &w)| is_flow(w >> 26));
        if flows {
            mode = Mode::Struct;
        }
    } else {
        g = new();
        run.clear();
        g.dispatch(entry, &mut run)?;
        mode = Mode::Dispatch;
    }

    let mut region = String::new();
    region.push_str("// @@AOT — programa traduzido (gerado por yw1-recomp: recomp/src/pica_glsl.rs)\n\n");
    let _ = writeln!(region, "void run() {{\n{run}}}\n");
    region.push_str("void loadInputs(uint index) {\n");
    for r in (0..16).filter(|r| g.read_in >> r & 1 != 0) {
        let _ = writeln!(region, "    vin[{r}] = inputRegister({r}u, index);");
    }
    region.push_str("}\n\n");
    region.push_str("// registradores de saída que o programa escreve; os outros ficam em (0, 0, 0, 1)\n");
    region.push_str("float outComp(uint x) {\n    switch (x) {\n");
    for r in (0..16u32).filter(|r| g.written_out >> r & 1 != 0) {
        for (c, n) in "xyzw".chars().enumerate() {
            let _ = writeln!(region, "    case {}u: return vout[{r}].{n};", r * 4 + c as u32);
        }
    }
    region.push_str("    default: return (x & 3u) == 3u ? 1.0 : 0.0;\n    }\n}\n");
    let instructions = g.emitted;
    Ok(Translation { mode, region, code_deps: g.code_deps, desc_deps: g.desc_deps, instructions })
}

/// Monta o shader: o `pica_vs.vert` com o trecho do interpretador trocado por `region` e um comentário de origem
/// logo depois do `#version`.
pub fn splice(template: &str, region: &str, header: &str) -> Result<String, String> {
    let b = template.find("\n// @@AOT-BEGIN").ok_or("pica_vs.vert sem @@AOT-BEGIN")? + 1;
    let e0 = template.find("\n// @@AOT-END").ok_or("pica_vs.vert sem @@AOT-END")? + 1;
    let e = template[e0..].find('\n').map_or(template.len(), |k| e0 + k + 1);
    let v = template.find('\n').ok_or("pica_vs.vert vazio")? + 1;
    if !(v <= b && b < e0) {
        return Err("marcadores fora de ordem".into());
    }
    let mut s = String::with_capacity(template.len() + region.len() + header.len());
    s.push_str(&template[..v]);
    s.push_str(header);
    s.push_str(&template[v..b]);
    s.push_str(region);
    s.push_str(&template[e..]);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(words: &[(u32, u32)], descs: &[(u32, u32)]) -> (Vec<u32>, Vec<u32>) {
        let mut c = vec![0u32; 4096];
        let mut d = vec![0u32; 128];
        for &(a, w) in words {
            c[a as usize] = w;
        }
        for &(i, v) in descs {
            d[i as usize] = v;
        }
        (c, d)
    }

    const ID: u32 = 0xf | 0x1b << 5 | 0x1b << 14; // máscara xyzw, seletores identidade
    const END: u32 = 0x22 << 26;

    #[test]
    fn straight_mul() {
        // mul o0, c0, v0 ; end ; (lixo depois do end não entra nas dependências)
        let (c, d) = prog(&[(0, 0x08 << 26 | 0x20 << 12), (1, END), (2, 0xdead_beef)], &[(0, ID)]);
        let t = translate(&c, &d, 0, false).unwrap();
        assert_eq!(t.mode, Mode::Straight);
        assert!(t.region.contains("pmul4(s1, s2)") && t.region.contains("vout[0] = r;"));
        assert!(t.region.contains("vin[0] = inputRegister(0u, index);"));
        assert_eq!(t.code_deps.keys().copied().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(t.desc_deps.keys().copied().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn ifc_struct_and_dispatch() {
        // 0: ifc (x) dst=3 num=1 ; 1: mov o0, v0 ; 2: nop ; 3: mov o1, v1 ; 4: end
        let ifc = 0x28 << 26 | 2 << 22 | 1 << 25 | 3 << 10 | 1;
        let mov = |o: u32, v: u32| 0x13 << 26 | o << 21 | v << 12;
        let (c, d) = prog(&[(0, ifc), (1, mov(0, 0)), (2, 0x21 << 26), (3, mov(1, 1)), (4, END)], &[(0, ID)]);
        let t = translate(&c, &d, 0, false).unwrap();
        assert_eq!(t.mode, Mode::Struct);
        assert!(t.region.contains("if (cond("));
        let t2 = translate(&c, &d, 0, true).unwrap();
        assert_eq!(t2.mode, Mode::Dispatch);
        assert!(t2.region.contains("push(1u, 2u, 4u, 0u, 0, false) : push(3u, 1u, 4u, 0u, 0, false)"));
        assert_eq!(t.code_deps, t2.code_deps);
    }

    #[test]
    fn loop_goes_to_dispatch() {
        // 0: loop i0 até 1 ; 1: add r0, r0, c0 ; 2: end
        let lp = 0x29 << 26 | 1 << 10;
        let add = 0x00 << 26 | 0x10 << 21 | 0x20 << 12 | 0x10 << 7;
        let (c, d) = prog(&[(0, lp), (1, add), (2, END)], &[(0, ID)]);
        let t = translate(&c, &d, 0, false).unwrap();
        assert_eq!(t.mode, Mode::Dispatch);
        assert!(t.region.contains("case 1u:") && t.region.contains("case 2u:"));
    }

    #[test]
    fn splice_markers() {
        let tpl = "#version 450\nA\n// @@AOT-BEGIN x\nINTERP\n// @@AOT-END\nB\n";
        let s = splice(tpl, "GEN\n", "// H\n").unwrap();
        assert_eq!(s, "#version 450\n// H\nA\nGEN\nB\n");
    }
}
