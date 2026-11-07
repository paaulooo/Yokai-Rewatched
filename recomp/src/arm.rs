//! Decodificação de uma instrução ARM (ARMv6K + VFPv2, modo ARM) e tradução para Rust.
//!
//! Cada instrução vira um trecho de Rust que opera sobre `c: &mut Cpu` e a memória guest (`rt::r32` etc.), mais a
//! informação de fluxo de controle ([`Flow`]) que o descobridor de funções e o emissor usam.
//! Leitura de `r15` = endereço da instrução + 8 (constante na geração). Literais lidos de `.text`/`.rodata` (só
//! leitura) viram constantes.
//!
//! Semântica: ARM Architecture Reference Manual (pseudocódigo ARMv6/ARMv7-A). O que é hipótese está marcado [H].

use std::fmt::Write;

/// Imagem do `code.bin` com os endereços virtuais dos segmentos.
pub struct Image {
    pub bytes: Vec<u8>,
    pub base: u32,
    /// fim do `.text` (exclusivo)
    pub text_end: u32,
    /// fim do `.rodata` (exclusivo): `[base, ro_end)` é só leitura → literais podem virar constantes
    pub ro_end: u32,
}

impl Image {
    pub fn word(&self, va: u32) -> Option<u32> {
        let off = va.checked_sub(self.base)? as usize;
        let b = self.bytes.get(off..off + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    #[cfg(test)]
    pub fn in_text(&self, va: u32) -> bool {
        va >= self.base && va < self.text_end && va & 3 == 0
    }
    /// Endereço dentro do `.text` (qualquer alinhamento).
    pub fn in_text_any(&self, va: u32) -> bool {
        va >= self.base && va < self.text_end
    }
    /// Chave de função (endereço | 1 = Thumb) aponta para código válido?
    pub fn key_in_text(&self, key: u32) -> bool {
        let a = key & !1;
        a >= self.base && a < self.text_end && (key & 1 == 1 || a & 3 == 0)
    }
    /// Valor constante de uma leitura de 32 bits (só em memória somente leitura).
    pub fn const_word(&self, va: u32) -> Option<u32> {
        if va >= self.base && va + 4 <= self.ro_end && va & 3 == 0 {
            self.word(va)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Flow {
    /// segue para a próxima instrução
    Next,
    /// `B alvo` (condicional se `cond != 14`)
    Branch(u32),
    /// `BL alvo`
    Call(u32),
    /// chamada indireta já feita pelo corpo (`BLX reg`, `mov lr, pc` + salto)
    CallInd,
    /// retorno (`bx lr`, `pop {.., pc}`, `ldr pc, [sp], #4`, `mov pc, lr`); o corpo faz os efeitos (pops) e deixa
    /// o destino em `t` — só sai da função se `t` for o endereço de retorno dela (o assembly escrito à mão do jogo
    /// também usa `adr lr, x` + salto como "chamada" interna, e volta com `mov pc, lr`)
    Return,
    /// salto computado: o corpo deixa o alvo em `t`
    JumpInd,
    /// tabela de saltos com alvos conhecidos: o corpo deixa o alvo em `t`
    Table(Vec<u32>),
    /// fim do caminho que nunca volta (`svc 0x3C` = Break do kernel)
    Halt,
    /// instrução não suportada / dado
    Undefined,
}

pub struct Ins {
    pub cond: u32,
    pub flow: Flow,
    pub body: String,
    /// tamanho em bytes (4 no ARM; 2 ou 4 no Thumb)
    pub len: u32,
}

fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}
fn bit(w: u32, b: u32) -> bool {
    w >> b & 1 != 0
}

/// Expressão Rust da condição (ARM ARM A8.3).
pub fn cond_expr(cond: u32) -> &'static str {
    match cond {
        0 => "c.z",
        1 => "!c.z",
        2 => "c.c",
        3 => "!c.c",
        4 => "c.n",
        5 => "!c.n",
        6 => "c.v",
        7 => "!c.v",
        8 => "(c.c && !c.z)",
        9 => "(!c.c || c.z)",
        10 => "(c.n == c.v)",
        11 => "(c.n != c.v)",
        12 => "(!c.z && c.n == c.v)",
        13 => "(c.z || c.n != c.v)",
        _ => "true",
    }
}

struct T<'a> {
    pc: u32,
    w: u32,
    img: &'a Image,
    out: String,
}

impl T<'_> {
    /// Leitura de registrador como expressão (`r15` = pc + 8).
    fn r(&self, n: u32) -> String {
        if n == 15 {
            format!("{:#x}u32", self.pc.wrapping_add(8))
        } else {
            format!("c.r[{n}]")
        }
    }
    fn line(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push(' ');
    }
}

fn ins(cond: u32, flow: Flow, body: String) -> Ins {
    Ins {
        cond,
        flow,
        body,
        len: 4,
    }
}

fn undef(cond: u32, pc: u32, w: u32) -> Ins {
    ins(
        cond,
        Flow::Undefined,
        format!("rt::undefined({pc:#x}, {w:#x});"),
    )
}

/// Traduz a instrução em `pc`.
pub fn translate(img: &Image, pc: u32) -> Ins {
    let Some(w) = img.word(pc) else {
        return undef(14, pc, 0);
    };
    let cond = w >> 28;
    let mut t = T {
        pc,
        w,
        img,
        out: String::new(),
    };
    if cond == 15 {
        return unconditional(&mut t);
    }
    let flow = match bits(w, 27, 25) {
        0b000 | 0b001 => group0(&mut t),
        0b010 => load_store(&mut t),
        0b011 => {
            if bit(w, 4) {
                media(&mut t)
            } else {
                load_store(&mut t)
            }
        }
        0b100 => block(&mut t),
        0b101 => {
            let off = ((w & 0x00ff_ffff) << 8) as i32 >> 6;
            let target = pc.wrapping_add(8).wrapping_add(off as u32);
            if bit(w, 24) {
                Some(Flow::Call(target))
            } else {
                Some(Flow::Branch(target))
            }
        }
        0b110 => coproc_ls(&mut t),
        _ => {
            if bits(w, 27, 24) == 0b1111 {
                let imm = w & 0x00ff_ffff;
                t.line(&format!("c.r[15] = {:#x}; rt::svc(c, {imm:#x});", pc + 4));
                Some(if imm == 0x3c { Flow::Halt } else { Flow::Next })
            } else {
                coproc_dp(&mut t)
            }
        }
    };
    match flow {
        Some(f) => {
            // `mov lr, pc` imediatamente antes de um salto indireto = chamada (estilo pré-BLX)
            let f = if f == Flow::JumpInd && img.word(pc.wrapping_sub(4)) == Some(0xe1a0_e00f) {
                t.line("rt::call(c, t);");
                Flow::CallInd
            } else {
                f
            };
            ins(cond, f, t.out)
        }
        None => undef(cond, pc, w),
    }
}

/// Instruções com cond = 1111: só PLD (dica), CLREX e barreiras viram nada; o resto é indefinido aqui.
fn unconditional(t: &mut T) -> Ins {
    let w = t.w;
    // PLD [Rn, #imm] / [Rn, Rm]: 1111 01x1 x101 ....
    if w & 0xfd70_f000 == 0xf550_f000 || w & 0xfd70_f010 == 0xf750_f000 {
        return ins(14, Flow::Next, String::new());
    }
    // BLX imediato: chama Thumb (alvo = pc + 8 + imm24*4 + H*2)
    if bits(w, 27, 25) == 0b101 {
        let off = ((w & 0x00ff_ffff) << 8) as i32 >> 6;
        let t_ = t.pc.wrapping_add(8).wrapping_add(off as u32) + (bits(w, 24, 24) << 1);
        return ins(14, Flow::Call(t_ | 1), String::new());
    }
    // CLREX, DSB, DMB, ISB (v6K/v7)
    if w == 0xf57f_f01f || w & 0xffff_fff0 == 0xf57f_f040 || w & 0xffff_fff0 == 0xf57f_f050 {
        return ins(14, Flow::Next, String::new());
    }
    undef(14, t.pc, w)
}

// ---------------------------------------------------------------- grupo 00x: processamento de dados e afins

fn group0(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let imm = bit(w, 25);
    let op = bits(w, 24, 21);
    let s = bit(w, 20);
    if !imm && bit(w, 4) && bit(w, 7) {
        return if bits(w, 6, 5) == 0 {
            match bits(w, 27, 23) {
                0b00000 | 0b00001 => multiply(t),
                0b00010 => swap(t),
                _ => exclusive(t),
            }
        } else {
            extra_ls(t)
        };
    }
    if (8..=11).contains(&op) && !s {
        return if imm { msr_imm(t) } else { misc(t) };
    }
    data_proc(t)
}

/// Segundo operando (shifter) → (expressão do valor, expressão do carry de saída).
fn shifter(t: &T) -> (String, String) {
    let w = t.w;
    if bit(w, 25) {
        let rot = bits(w, 11, 8) * 2;
        let v = (w & 0xff).rotate_right(rot);
        let carry = if rot == 0 {
            "c.c".to_string()
        } else {
            format!("{}", v >> 31 != 0)
        };
        return (format!("{v:#x}u32"), carry);
    }
    let rm_n = bits(w, 3, 0);
    let ty = bits(w, 6, 5);
    if bit(w, 4) {
        // shift por registrador: r15 lido = pc + 12
        let rm = if rm_n == 15 {
            format!("{:#x}u32", t.pc + 12)
        } else {
            format!("c.r[{rm_n}]")
        };
        let rs = format!("c.r[{}]", bits(w, 11, 8));
        let f = ["lsl", "lsr", "asr", "ror"][ty as usize];
        return (
            format!("rt::{f}({rm}, {rs})"),
            format!("rt::{f}_c({rm}, {rs}, c.c).1"),
        );
    }
    let rm = t.r(rm_n);
    let n = bits(w, 11, 7);
    match (ty, n) {
        (0, 0) => (rm, "c.c".into()),
        (0, n) => (format!("({rm} << {n})"), format!("({rm} >> {} & 1 != 0)", 32 - n)),
        (1, 0) => ("0u32".into(), format!("({rm} >> 31 != 0)")),
        (1, n) => (format!("({rm} >> {n})"), format!("({rm} >> {} & 1 != 0)", n - 1)),
        (2, 0) => (
            format!("((({rm}) as i32 >> 31) as u32)"),
            format!("({rm} >> 31 != 0)"),
        ),
        (2, n) => (
            format!("((({rm}) as i32 >> {n}) as u32)"),
            format!("({rm} >> {} & 1 != 0)", n - 1),
        ),
        (3, 0) => (
            format!("(({rm} >> 1) | ((c.c as u32) << 31))"),
            format!("({rm} & 1 != 0)"),
        ),
        (_, n) => (
            format!("{rm}.rotate_right({n})"),
            format!("({rm} >> {} & 1 != 0)", n - 1),
        ),
    }
}

fn data_proc(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let op = bits(w, 24, 21);
    let s = bit(w, 20);
    let rn = bits(w, 19, 16);
    let rd = bits(w, 15, 12);
    let (o, carry) = shifter(t);
    // r15 como Rn com shift por registrador = pc + 12
    let a = if rn == 15 && !bit(w, 25) && bit(w, 4) {
        format!("{:#x}u32", t.pc + 12)
    } else {
        t.r(rn)
    };
    let logical = matches!(op, 0 | 1 | 8 | 9 | 12 | 13 | 14 | 15);
    if s && rd == 15 && !matches!(op, 8..=11) {
        // `movs pc, lr` etc. = retorno de exceção, não existe em código de usuário
        return None;
    }
    let mut b = String::new();
    write!(b, "let o: u32 = {o};").unwrap();
    if !matches!(op, 13 | 15) {
        write!(b, " let a: u32 = {a};").unwrap();
    }
    if s && logical {
        write!(b, " let sc = {carry};").unwrap();
    }
    let val = match (op, s) {
        (0, _) => "a & o".to_string(),
        (1, _) => "a ^ o".into(),
        (2, false) => "a.wrapping_sub(o)".into(),
        (2, true) => "rt::sbc(c, a, o, true)".into(),
        (3, false) => "o.wrapping_sub(a)".into(),
        (3, true) => "rt::sbc(c, o, a, true)".into(),
        (4, false) => "a.wrapping_add(o)".into(),
        (4, true) => "rt::adc(c, a, o, false)".into(),
        (5, false) => "a.wrapping_add(o).wrapping_add(c.c as u32)".into(),
        (5, true) => "{ let ci = c.c; rt::adc(c, a, o, ci) }".into(),
        (6, false) => "a.wrapping_sub(o).wrapping_sub(!c.c as u32)".into(),
        (6, true) => "{ let ci = c.c; rt::sbc(c, a, o, ci) }".into(),
        (7, false) => "o.wrapping_sub(a).wrapping_sub(!c.c as u32)".into(),
        (7, true) => "{ let ci = c.c; rt::sbc(c, o, a, ci) }".into(),
        (8, _) => "a & o".into(),
        (9, _) => "a ^ o".into(),
        (10, _) => "rt::sbc(c, a, o, true)".into(),
        (11, _) => "rt::adc(c, a, o, false)".into(),
        (12, _) => "a | o".into(),
        (13, _) => "o".into(),
        (14, _) => "a & !o".into(),
        _ => "!o".into(),
    };
    if matches!(op, 8..=11) {
        // TST/TEQ/CMP/CMN: só flags
        if logical {
            write!(b, " let r = {val}; rt::nz(c, r); c.c = sc;").unwrap();
        } else {
            write!(b, " {val};").unwrap();
        }
        t.line(&b);
        return Some(Flow::Next);
    }
    write!(b, " let r = {val};").unwrap();
    if s && logical {
        b.push_str(" rt::nz(c, r); c.c = sc;");
    }
    if rd != 15 {
        write!(b, " c.r[{rd}] = r;").unwrap();
        t.line(&b);
        return Some(Flow::Next);
    }
    // escreve no pc
    let rm = bits(w, 3, 0);
    if op == 13 && !bit(w, 25) && bits(w, 11, 4) == 0 && rm == 14 {
        // mov pc, lr
        t.line("let t = c.r[14];");
        return Some(Flow::Return);
    }
    // tabela de saltos do RVCT: `cmp rX, #N` ; `addls pc, pc, rX, lsl #2` ; `b default` ; `b caso0` …
    if op == 4 && rn == 15 && !bit(w, 25) && bits(w, 11, 4) == 0b0001_0000 {
        let n = t
            .img
            .word(t.pc.wrapping_sub(4))
            .filter(|p| p & 0x0ff0_f000 == 0x0350_0000 && bits(*p, 19, 16) == rm)
            .map(|p| (p & 0xff).rotate_right(bits(p, 11, 8) * 2));
        let n = n.unwrap_or_else(|| {
            // sem o cmp: conta os `B` seguidos
            let mut k = 0;
            while k < 1024
                && t
                    .img
                    .word(t.pc + 8 + 4 * k)
                    .is_some_and(|x| bits(x, 27, 24) == 0b1010)
            {
                k += 1;
            }
            k.saturating_sub(1)
        });
        let targets = (0..=n).map(|i| t.pc + 8 + 4 * i).collect();
        t.line(&format!("{b} let t = r;"));
        return Some(Flow::Table(targets));
    }
    // tabela relativa escrita à mão: `sub/add rB, pc, #k` (base) ; `ldr rA, [rB, rX, lsl #2]` ; `add pc, rA, rB`
    // → alvo = base + tabela[i]
    if op == 4 && !bit(w, 25) && bits(w, 11, 4) == 0 {
        if let Some(targets) = relative_table(t, rn, rm) {
            t.line(&format!("{b} let t = r;"));
            return Some(Flow::Table(targets));
        }
    }
    t.line(&format!("{b} let t = r;"));
    Some(Flow::JumpInd)
}

/// Procura (até 12 instruções antes) o `ldr` indexado e o ADR da base; devolve os alvos da tabela.
fn relative_table(t: &T, ra: u32, rb: u32) -> Option<Vec<u32>> {
    let mut base_reg = None;
    let mut base = None;
    for k in 1..=12u32 {
        let pa = t.pc.checked_sub(4 * k)?;
        let p = t.img.word(pa)?;
        // ldr rA, [rB, rX{, lsl #2}] (P=1, U=1, registrador, sem writeback; índice em palavras ou já em bytes)
        if base_reg.is_none() && p & 0x0ff0_0070 == 0x0790_0000 && matches!(bits(p, 11, 7), 0 | 2) {
            let (rn, rt_) = (bits(p, 19, 16), bits(p, 15, 12));
            if (rt_ == ra && rn == rb) || (rt_ == rb && rn == ra) {
                base_reg = Some(rn);
            }
        }
        if let Some(br) = base_reg {
            // add/sub br, pc, #imm
            let imm = (p & 0xff).rotate_right(bits(p, 11, 8) * 2);
            if p & 0x0fff_f000 == 0x028f_0000 | br << 12 {
                base = Some((pa + 8).wrapping_add(imm));
                break;
            }
            if p & 0x0fff_f000 == 0x024f_0000 | br << 12 {
                base = Some((pa + 8).wrapping_sub(imm));
                break;
            }
        }
    }
    let base = base?;
    let mut out = Vec::new();
    for i in 0..256u32 {
        let Some(v) = t.img.word(base + 4 * i) else {
            break;
        };
        let tg = base.wrapping_add(v);
        if !t.img.in_text_any(tg) || tg & 3 != 0 || tg.abs_diff(t.pc) > 0x1_0000 {
            break;
        }
        out.push(tg);
    }
    (!out.is_empty()).then_some(out)
}

fn msr_imm(t: &mut T) -> Option<Flow> {
    let w = t.w;
    if bit(w, 22) {
        return None; // SPSR
    }
    if !bit(w, 21) {
        return None;
    }
    let mask = bits(w, 19, 16);
    if mask == 0 {
        // NOP / YIELD / WFE / WFI / SEV (v6K)
        return Some(Flow::Next);
    }
    let v = (w & 0xff).rotate_right(bits(w, 11, 8) * 2);
    t.line(&format!("c.set_cpsr({v:#x}, {mask});"));
    Some(Flow::Next)
}

fn misc(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let op = bits(w, 22, 21);
    let rd = bits(w, 15, 12);
    let rm = bits(w, 3, 0);
    match bits(w, 7, 4) {
        0b0000 => {
            if bit(w, 22) {
                return None;
            }
            if op & 1 == 0 {
                t.line(&format!("c.r[{rd}] = c.cpsr();"));
            } else {
                let mask = bits(w, 19, 16);
                let v = t.r(rm);
                t.line(&format!("let v = {v}; c.set_cpsr(v, {mask});"));
            }
            Some(Flow::Next)
        }
        0b0001 if op == 1 => {
            if rm == 14 {
                t.line("let t = c.r[14];");
                Some(Flow::Return)
            } else {
                let v = t.r(rm);
                t.line(&format!("let t = {v};"));
                Some(Flow::JumpInd)
            }
        }
        0b0001 if op == 3 => {
            let v = t.r(rm);
            t.line(&format!("c.r[{rd}] = {v}.leading_zeros();"));
            Some(Flow::Next)
        }
        0b0011 if op == 1 => {
            let v = t.r(rm);
            t.line(&format!(
                "let t = {v}; c.r[14] = {:#x}; rt::call(c, t);",
                t.pc + 4
            ));
            Some(Flow::CallInd)
        }
        0b0101 => {
            let f = ["qadd", "qsub", "qdadd", "qdsub"][op as usize];
            let (a, b) = (t.r(rm), t.r(bits(w, 19, 16)));
            t.line(&format!(
                "let a = {a}; let b = {b}; c.r[{rd}] = rt::{f}(c, a, b);"
            ));
            Some(Flow::Next)
        }
        0b0111 => None, // BKPT
        x if x & 0b1001 == 0b1000 => {
            // multiplicações 16×16 (v5TE)
            let rdh = bits(w, 19, 16);
            let ra = bits(w, 15, 12);
            let rs = bits(w, 11, 8);
            let xh = bit(w, 5);
            let yh = bit(w, 6);
            let half = |r: String, h: bool| {
                if h {
                    format!("rt::hi16({r})")
                } else {
                    format!("rt::lo16({r})")
                }
            };
            let n = half(t.r(rm), xh);
            let m = half(t.r(rs), yh);
            let body = match op {
                0 => format!(
                    "let p = ({n} as i64) * ({m} as i64); let a = {} as i32 as i64; c.r[{rdh}] = rt::add_q(c, p, a);",
                    t.r(ra)
                ),
                1 => {
                    let wv = t.r(rm);
                    let p = format!("((({wv}) as i32 as i64) * ({m} as i64)) >> 16");
                    if xh {
                        format!("c.r[{rdh}] = ({p}) as u32;")
                    } else {
                        format!(
                            "let p = {p}; let a = {} as i32 as i64; c.r[{rdh}] = rt::add_q(c, p, a);",
                            t.r(ra)
                        )
                    }
                }
                2 => format!(
                    "let acc = ((c.r[{rdh}] as u64) << 32 | c.r[{ra}] as u64).wrapping_add((({n} as i64) * ({m} as i64)) as u64); c.r[{ra}] = acc as u32; c.r[{rdh}] = (acc >> 32) as u32;"
                ),
                _ => format!("c.r[{rdh}] = (({n} as i64) * ({m} as i64)) as u32;"),
            };
            t.line(&body);
            Some(Flow::Next)
        }
        _ => None,
    }
}

fn multiply(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let s = bit(w, 20);
    let rd = bits(w, 19, 16);
    let ra = bits(w, 15, 12);
    let rs = t.r(bits(w, 11, 8));
    let rm = t.r(bits(w, 3, 0));
    let mut b = format!("let m: u32 = {rm}; let s: u32 = {rs};");
    match bits(w, 23, 21) {
        0 => write!(b, " let r = m.wrapping_mul(s); c.r[{rd}] = r;").unwrap(),
        1 => write!(
            b,
            " let r = m.wrapping_mul(s).wrapping_add(c.r[{ra}]); c.r[{rd}] = r;"
        )
        .unwrap(),
        2 => {
            write!(b, " let r = (m as u64) * (s as u64) + c.r[{ra}] as u64 + c.r[{rd}] as u64; c.r[{ra}] = r as u32; c.r[{rd}] = (r >> 32) as u32;").unwrap();
            t.line(&b);
            return Some(Flow::Next);
        }
        x @ 4..=7 => {
            let prod = if x >= 6 {
                "((m as i32 as i64) * (s as i32 as i64)) as u64"
            } else {
                "(m as u64) * (s as u64)"
            };
            let acc = if x & 1 == 1 {
                format!(".wrapping_add((c.r[{rd}] as u64) << 32 | c.r[{ra}] as u64)")
            } else {
                String::new()
            };
            write!(
                b,
                " let r = ({prod}){acc}; c.r[{ra}] = r as u32; c.r[{rd}] = (r >> 32) as u32;"
            )
            .unwrap();
            if s {
                b.push_str(" c.n = (r as i64) < 0; c.z = r == 0;");
            }
            t.line(&b);
            return Some(Flow::Next);
        }
        _ => return None,
    }
    if s {
        b.push_str(" rt::nz(c, r);");
    }
    t.line(&b);
    Some(Flow::Next)
}

fn swap(t: &mut T) -> Option<Flow> {
    let w = t.w;
    if bit(w, 23) {
        return exclusive(t);
    }
    let rn = bits(w, 19, 16);
    let rt_ = bits(w, 15, 12);
    let rt2 = t.r(bits(w, 3, 0));
    let (rd, wr) = if bit(w, 22) {
        ("rt::r8", "rt::w8")
    } else {
        ("rt::r32", "rt::w32")
    };
    t.line(&format!(
        "let a = c.r[{rn}]; let n = {rt2}; let v = {rd}(a); {wr}(a, n); c.r[{rt_}] = v;"
    ));
    Some(Flow::Next)
}

/// LDREX/STREX: só uma thread guest roda por vez e a troca só acontece em syscall → o STREX sempre consegue. [H]
fn exclusive(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let rn = bits(w, 19, 16);
    let rd = bits(w, 15, 12);
    let rt_ = bits(w, 3, 0);
    let size = bits(w, 22, 21);
    let (rf, wf) = match size {
        0 | 1 => ("rt::r32", "rt::w32"),
        2 => ("rt::r8", "rt::w8"),
        _ => ("rt::r16", "rt::w16"),
    };
    if bit(w, 20) {
        if size == 1 {
            t.line(&format!(
                "let a = c.r[{rn}]; c.r[{rd}] = rt::r32(a); c.r[{}] = rt::r32(a + 4);",
                rd + 1
            ));
        } else {
            t.line(&format!("let a = c.r[{rn}]; c.r[{rd}] = {rf}(a);"));
        }
    } else if size == 1 {
        t.line(&format!(
            "let a = c.r[{rn}]; rt::w32(a, c.r[{rt_}]); rt::w32(a + 4, c.r[{}]); c.r[{rd}] = 0;",
            rt_ + 1
        ));
    } else {
        t.line(&format!(
            "let a = c.r[{rn}]; {wf}(a, c.r[{rt_}]); c.r[{rd}] = 0;"
        ));
    }
    Some(Flow::Next)
}

/// Endereço de load/store: devolve (endereço usado, novo valor de Rn se houver writeback).
fn address(t: &T, rn: u32, off: String) -> (String, Option<String>) {
    let w = t.w;
    let p = bit(w, 24);
    let u = bit(w, 23);
    let wb = bit(w, 21);
    let base = if rn == 15 {
        format!("{:#x}u32", (t.pc + 8) & !3)
    } else {
        format!("c.r[{rn}]")
    };
    let moved = if u {
        format!("{base}.wrapping_add({off})")
    } else {
        format!("{base}.wrapping_sub({off})")
    };
    if p {
        (moved.clone(), if wb { Some(moved) } else { None })
    } else {
        (base, Some(moved))
    }
}

/// Constante da memória somente leitura para `ldr rX, [pc, #imm]`.
fn literal(t: &T) -> Option<u32> {
    let w = t.w;
    if bits(w, 19, 16) != 15 || bit(w, 25) || !bit(w, 24) || bit(w, 21) || bit(w, 22) {
        return None;
    }
    let imm = w & 0xfff;
    let a = if bit(w, 23) {
        (t.pc + 8).wrapping_add(imm)
    } else {
        (t.pc + 8).wrapping_sub(imm)
    };
    t.img.const_word(a)
}

fn load_store(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let load = bit(w, 20);
    let byte = bit(w, 22);
    let rn = bits(w, 19, 16);
    let rt_ = bits(w, 15, 12);
    let off = if bit(w, 25) {
        // registrador com shift imediato
        let rm = t.r(bits(w, 3, 0));
        let n = bits(w, 11, 7);
        match (bits(w, 6, 5), n) {
            (0, 0) => rm,
            (0, n) => format!("({rm} << {n})"),
            (1, 0) => "0u32".into(),
            (1, n) => format!("({rm} >> {n})"),
            (2, 0) => format!("((({rm}) as i32 >> 31) as u32)"),
            (2, n) => format!("((({rm}) as i32 >> {n}) as u32)"),
            (3, 0) => format!("(({rm} >> 1) | ((c.c as u32) << 31))"),
            (_, n) => format!("{rm}.rotate_right({n})"),
        }
    } else {
        format!("{:#x}u32", w & 0xfff)
    };
    if load && !byte {
        if let Some(k) = literal(t) {
            if rt_ == 15 {
                t.line(&format!("let t = {k:#x}u32;"));
                return Some(Flow::JumpInd);
            }
            t.line(&format!("c.r[{rt_}] = {k:#x};"));
            return Some(Flow::Next);
        }
    }
    let (addr, wb) = address(t, rn, off);
    let mut b = format!("let a: u32 = {addr};");
    if load {
        write!(
            b,
            " let v = {}(a);",
            if byte { "rt::r8" } else { "rt::r32" }
        )
        .unwrap();
        if let Some(nv) = &wb {
            write!(b, " c.r[{rn}] = {};", nv.replace(&format!("c.r[{rn}]"), "a_base")).unwrap();
            b = b.replacen("let a: u32", &format!("let a_base = c.r[{rn}]; let a: u32"), 1);
        }
        if rt_ == 15 {
            write!(b, " let t = v;").unwrap();
            t.line(&b);
            // `ldr pc, [sp], #4` = pop {pc}
            return Some(if rn == 13 && !bit(w, 24) {
                Flow::Return
            } else {
                Flow::JumpInd
            });
        }
        write!(b, " c.r[{rt_}] = v;").unwrap();
    } else {
        let v = t.r(rt_);
        write!(
            b,
            " {}(a, {v});",
            if byte { "rt::w8" } else { "rt::w32" }
        )
        .unwrap();
        if let Some(nv) = &wb {
            write!(b, " c.r[{rn}] = {nv};").unwrap();
        }
    }
    t.line(&b);
    Some(Flow::Next)
}

/// LDRH/STRH/LDRSB/LDRSH/LDRD/STRD
fn extra_ls(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let load = bit(w, 20);
    let rn = bits(w, 19, 16);
    let rt_ = bits(w, 15, 12);
    let off = if bit(w, 22) {
        format!("{:#x}u32", bits(w, 11, 8) << 4 | bits(w, 3, 0))
    } else {
        t.r(bits(w, 3, 0))
    };
    let (addr, wb) = address(t, rn, off);
    let kind = bits(w, 6, 5);
    let mut b = format!("let a_base = {}; let a: u32 = {};", t.r(rn), addr.replace(&format!("c.r[{rn}]"), "a_base"));
    let wb_s = wb.map(|nv| format!(" c.r[{rn}] = {};", nv.replace(&format!("c.r[{rn}]"), "a_base")));
    match (kind, load) {
        (1, false) => {
            write!(b, " rt::w16(a, {});", t.r(rt_)).unwrap();
            b.push_str(wb_s.as_deref().unwrap_or(""));
        }
        (1, true) | (2, true) | (3, true) => {
            let rd = match kind {
                1 => "rt::r16(a)",
                2 => "(rt::r8(a) as u8 as i8 as i32 as u32)",
                _ => "(rt::r16(a) as u16 as i16 as i32 as u32)",
            };
            write!(b, " let v = {rd};").unwrap();
            b.push_str(wb_s.as_deref().unwrap_or(""));
            if rt_ == 15 {
                return None;
            }
            write!(b, " c.r[{rt_}] = v;").unwrap();
        }
        (2, false) => {
            // LDRD
            if rt_ & 1 != 0 || rt_ == 14 {
                return None;
            }
            write!(b, " let v0 = rt::r32(a); let v1 = rt::r32(a.wrapping_add(4));").unwrap();
            b.push_str(wb_s.as_deref().unwrap_or(""));
            write!(b, " c.r[{rt_}] = v0; c.r[{}] = v1;", rt_ + 1).unwrap();
        }
        _ => {
            // STRD
            if rt_ & 1 != 0 || rt_ == 14 {
                return None;
            }
            write!(
                b,
                " rt::w32(a, c.r[{rt_}]); rt::w32(a.wrapping_add(4), c.r[{}]);",
                rt_ + 1
            )
            .unwrap();
            b.push_str(wb_s.as_deref().unwrap_or(""));
        }
    }
    t.line(&b);
    Some(Flow::Next)
}

/// LDM/STM (todas as formas, sem o bit S).
fn block(t: &mut T) -> Option<Flow> {
    let w = t.w;
    if bit(w, 22) {
        return None;
    }
    let p = bit(w, 24);
    let u = bit(w, 23);
    let wb = bit(w, 21);
    let load = bit(w, 20);
    let rn = bits(w, 19, 16);
    let list = w & 0xffff;
    let n = list.count_ones();
    if n == 0 || rn == 15 {
        return None;
    }
    let start: i64 = match (p, u) {
        (false, true) => 0,
        (true, true) => 4,
        (false, false) => -4 * n as i64 + 4,
        (true, false) => -4 * n as i64,
    };
    let delta: i64 = if u { 4 * n as i64 } else { -4 * n as i64 };
    let mut b = format!(
        "let base = c.r[{rn}]; let a = base.wrapping_add({}u32);",
        start as i32 as u32
    );
    let mut k = 0u32;
    let regs: Vec<u32> = (0..16).filter(|i| list >> i & 1 != 0).collect();
    if load {
        if wb {
            write!(b, " c.r[{rn}] = base.wrapping_add({}u32);", delta as i32 as u32).unwrap();
        }
        let mut ret = false;
        for &r in &regs {
            if r == 15 {
                write!(b, " let t = rt::r32(a.wrapping_add({}));", 4 * k).unwrap();
                ret = true;
            } else {
                write!(b, " c.r[{r}] = rt::r32(a.wrapping_add({}));", 4 * k).unwrap();
            }
            k += 1;
        }
        t.line(&b);
        if ret {
            return Some(if rn == 13 && wb && u {
                Flow::Return
            } else {
                Flow::JumpInd
            });
        }
    } else {
        for &r in &regs {
            let v = if r == 15 {
                format!("{:#x}u32", t.pc + 8)
            } else if r == rn {
                "base".to_string()
            } else {
                format!("c.r[{r}]")
            };
            write!(b, " rt::w32(a.wrapping_add({}), {v});", 4 * k).unwrap();
            k += 1;
        }
        if wb {
            write!(b, " c.r[{rn}] = base.wrapping_add({}u32);", delta as i32 as u32).unwrap();
        }
        t.line(&b);
    }
    Some(Flow::Next)
}

// ---------------------------------------------------------------- mídia (ARMv6)

fn media(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let rn = bits(w, 19, 16);
    let rd = bits(w, 15, 12);
    let rm = bits(w, 3, 0);
    let op1 = bits(w, 22, 20);
    let op2 = bits(w, 7, 5);
    match bits(w, 24, 23) {
        0b00 => {
            if op1 & 3 == 0 || !matches!(op2, 0 | 1 | 2 | 3 | 4 | 7) {
                return None;
            }
            let (a, b) = (t.r(rn), t.r(rm));
            t.line(&format!(
                "let a = {a}; let b = {b}; c.r[{rd}] = rt::parallel(c, {op1}, {op2}, a, b);"
            ));
            Some(Flow::Next)
        }
        0b01 => {
            let m = t.r(rm);
            if op1 == 0 && op2 & 1 == 0 {
                let sh = bits(w, 11, 7);
                let n = t.r(rn);
                if !bit(w, 6) {
                    t.line(&format!(
                        "c.r[{rd}] = ({n} & 0xffff) | (({m} << {sh}) & 0xffff_0000);"
                    ));
                } else {
                    let s = if sh == 0 { 31 } else { sh };
                    t.line(&format!(
                        "c.r[{rd}] = ({n} & 0xffff_0000) | (((({m}) as i32 >> {s}) as u32) & 0xffff);"
                    ));
                }
                return Some(Flow::Next);
            }
            if op1 == 0 && op2 == 0b101 {
                let n = t.r(rn);
                t.line(&format!("c.r[{rd}] = rt::sel(c, {n}, {m});"));
                return Some(Flow::Next);
            }
            if op1 & 0b010 == 0b010 && op2 & 1 == 0 {
                // SSAT (op1 = 01x) / USAT (11x)
                let sat = bits(w, 20, 16);
                let sh = bits(w, 11, 7);
                let v = if bit(w, 6) {
                    let s = if sh == 0 { 31 } else { sh };
                    format!("((({m}) as i32 >> {s}) as i64)")
                } else {
                    format!("((({m} << {sh}) as i32) as i64)")
                };
                if bit(w, 22) {
                    t.line(&format!("c.r[{rd}] = rt::usat(c, {v}, {sat});"));
                } else {
                    t.line(&format!("c.r[{rd}] = rt::ssat(c, {v}, {});", sat + 1));
                }
                return Some(Flow::Next);
            }
            match (op1, op2) {
                (0b010, 0b001) => {
                    t.line(&format!(
                        "c.r[{rd}] = rt::ssat16(c, {m}, {});",
                        bits(w, 19, 16) + 1
                    ));
                    return Some(Flow::Next);
                }
                (0b110, 0b001) => {
                    t.line(&format!(
                        "c.r[{rd}] = rt::usat16(c, {m}, {});",
                        bits(w, 19, 16)
                    ));
                    return Some(Flow::Next);
                }
                (0b011, 0b001) => {
                    t.line(&format!("c.r[{rd}] = {m}.swap_bytes();"));
                    return Some(Flow::Next);
                }
                (0b011, 0b101) => {
                    t.line(&format!(
                        "let v = {m}; c.r[{rd}] = ((v & 0x00ff_00ff) << 8) | ((v >> 8) & 0x00ff_00ff);"
                    ));
                    return Some(Flow::Next);
                }
                (0b111, 0b101) => {
                    t.line(&format!(
                        "c.r[{rd}] = (({m} as u16).swap_bytes() as i16) as i32 as u32;"
                    ));
                    return Some(Flow::Next);
                }
                _ => {}
            }
            if op2 == 0b011 {
                // SXT*/UXT* (+ A)
                let rot = bits(w, 11, 10) * 8;
                let v = format!("{m}.rotate_right({rot})");
                let add = rn != 15;
                let n = t.r(rn);
                let e = match op1 {
                    0b000 | 0b100 => {
                        // B16: bytes 0 e 2 → metades
                        let ext = if op1 == 0 {
                            "(x as u8 as i8 as i16 as u16 as u32) | ((((x >> 16) as u8 as i8 as i16 as u16) as u32) << 16)"
                        } else {
                            "x & 0x00ff_00ff"
                        };
                        if add {
                            format!("{{ let x = {v}; let e = {ext}; let n = {n}; (n.wrapping_add(e) & 0xffff) | ((n >> 16).wrapping_add(e >> 16) << 16) }}")
                        } else {
                            format!("{{ let x = {v}; {ext} }}")
                        }
                    }
                    0b010 => format!("({v} as u8 as i8 as i32 as u32)"),
                    0b011 => format!("({v} as u16 as i16 as i32 as u32)"),
                    0b110 => format!("({v} & 0xff)"),
                    0b111 => format!("({v} & 0xffff)"),
                    _ => return None,
                };
                let e = if add && !matches!(op1, 0 | 4) {
                    format!("{n}.wrapping_add({e})")
                } else {
                    e
                };
                t.line(&format!("c.r[{rd}] = {e};"));
                return Some(Flow::Next);
            }
            None
        }
        0b10 => {
            // multiplicações com sinal (v6): Rd = 19:16, Ra = 15:12, Rm = 11:8, Rn = 3:0
            let rdh = bits(w, 19, 16);
            let ra = bits(w, 15, 12);
            let mm = t.r(bits(w, 11, 8));
            let nn = t.r(rm);
            let xsw = bit(w, 5);
            let m2 = if xsw {
                format!("{mm}.rotate_right(16)")
            } else {
                mm.clone()
            };
            match op1 {
                0b000 => {
                    let sub = bit(w, 6);
                    let mut b = format!(
                        "let n = {nn}; let m = {m2}; let p1 = (rt::lo16(n) * rt::lo16(m)) as i64; let p2 = (rt::hi16(n) * rt::hi16(m)) as i64;"
                    );
                    let sum = if sub { "p1 - p2" } else { "p1 + p2" };
                    if ra == 15 {
                        if sub {
                            write!(b, " c.r[{rdh}] = ({sum}) as u32;").unwrap();
                        } else {
                            write!(b, " c.r[{rdh}] = rt::add_q(c, p1, p2);").unwrap();
                        }
                    } else {
                        write!(
                            b,
                            " let a = c.r[{ra}] as i32 as i64; c.r[{rdh}] = rt::add_q(c, {sum}, a);"
                        )
                        .unwrap();
                    }
                    t.line(&b);
                    Some(Flow::Next)
                }
                0b100 => {
                    let sub = bit(w, 6);
                    let sum = if sub { "p1 - p2" } else { "p1 + p2" };
                    t.line(&format!(
                        "let n = {nn}; let m = {m2}; let p1 = (rt::lo16(n) * rt::lo16(m)) as i64; let p2 = (rt::hi16(n) * rt::hi16(m)) as i64; let acc = ((c.r[{rdh}] as u64) << 32 | c.r[{ra}] as u64).wrapping_add(({sum}) as u64); c.r[{ra}] = acc as u32; c.r[{rdh}] = (acc >> 32) as u32;"
                    ));
                    Some(Flow::Next)
                }
                0b101 => {
                    let round = if bit(w, 5) { "0x8000_0000i64" } else { "0i64" };
                    let p = format!("({nn} as i32 as i64) * ({mm} as i32 as i64)");
                    let e = match (bits(w, 7, 6), ra) {
                        (0b00, 15) => format!("(({p}).wrapping_add({round}) >> 32) as u32"),
                        (0b00, _) => format!(
                            "((((c.r[{ra}] as i32 as i64) << 32).wrapping_add({p}).wrapping_add({round})) >> 32) as u32"
                        ),
                        (0b11, _) => format!(
                            "((((c.r[{ra}] as i32 as i64) << 32).wrapping_sub({p}).wrapping_add({round})) >> 32) as u32"
                        ),
                        _ => return None,
                    };
                    t.line(&format!("c.r[{rdh}] = {e};"));
                    Some(Flow::Next)
                }
                _ => None,
            }
        }
        _ => {
            if op1 == 0 && op2 == 0 {
                let rdh = bits(w, 19, 16);
                let ra = bits(w, 15, 12);
                let mm = t.r(bits(w, 11, 8));
                let nn = t.r(rm);
                let acc = if ra == 15 {
                    String::new()
                } else {
                    format!(".wrapping_add(c.r[{ra}])")
                };
                t.line(&format!("c.r[{rdh}] = rt::usad8({nn}, {mm}){acc};"));
                return Some(Flow::Next);
            }
            None
        }
    }
}

// ---------------------------------------------------------------- coprocessadores: VFP (cp10/cp11) e cp15

fn sreg(hi4: u32, lo1: u32) -> u32 {
    hi4 << 1 | lo1
}

/// Load/store de coprocessador e transferências de 64 bits.
fn coproc_ls(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let cp = bits(w, 11, 8);
    if cp != 10 && cp != 11 {
        return None;
    }
    let dbl = cp == 11;
    let p = bit(w, 24);
    let u = bit(w, 23);
    let dbit = bits(w, 22, 22);
    let wb = bit(w, 21);
    let load = bit(w, 20);
    let rn = bits(w, 19, 16);
    let vd = bits(w, 15, 12);
    let imm8 = w & 0xff;
    // VMOV de 64 bits: 1100 010x
    if bits(w, 24, 21) == 0b0010 {
        let rt_ = bits(w, 15, 12);
        let rt2 = bits(w, 19, 16);
        if bits(w, 7, 6) != 0 || !bit(w, 4) {
            return None;
        }
        if dbl {
            let dm = bits(w, 3, 0) | bits(w, 5, 5) << 4;
            if dm >= 16 {
                return None;
            }
            if load {
                t.line(&format!(
                    "c.r[{rt_}] = c.s[{}]; c.r[{rt2}] = c.s[{}];",
                    2 * dm,
                    2 * dm + 1
                ));
            } else {
                t.line(&format!(
                    "c.s[{}] = c.r[{rt_}]; c.s[{}] = c.r[{rt2}];",
                    2 * dm,
                    2 * dm + 1
                ));
            }
        } else {
            let sm = sreg(bits(w, 3, 0), bits(w, 5, 5));
            if sm >= 31 {
                return None;
            }
            if load {
                t.line(&format!(
                    "c.r[{rt_}] = c.s[{sm}]; c.r[{rt2}] = c.s[{}];",
                    sm + 1
                ));
            } else {
                t.line(&format!(
                    "c.s[{sm}] = c.r[{rt_}]; c.s[{}] = c.r[{rt2}];",
                    sm + 1
                ));
            }
        }
        return Some(Flow::Next);
    }
    let first = if dbl {
        if dbit != 0 {
            return None;
        }
        2 * vd
    } else {
        sreg(vd, dbit)
    };
    // VLDR/VSTR: P=1, W=0
    if p && !wb {
        let off = imm8 * 4;
        let base = if rn == 15 {
            let a = (t.pc + 8) & !3;
            let a = if u { a + off } else { a - off };
            if load {
                if let (Some(lo), true) = (t.img.const_word(a), true) {
                    let hi = if dbl { t.img.const_word(a + 4) } else { Some(0) };
                    if let Some(hi) = hi {
                        if dbl {
                            t.line(&format!(
                                "c.s[{first}] = {lo:#x}; c.s[{}] = {hi:#x};",
                                first + 1
                            ));
                        } else {
                            t.line(&format!("c.s[{first}] = {lo:#x};"));
                        }
                        return Some(Flow::Next);
                    }
                }
            }
            format!("{a:#x}u32")
        } else if u {
            format!("c.r[{rn}].wrapping_add({off})")
        } else {
            format!("c.r[{rn}].wrapping_sub({off})")
        };
        let mut b = format!("let a: u32 = {base};");
        let n = if dbl { 2 } else { 1 };
        for i in 0..n {
            if load {
                write!(b, " c.s[{}] = rt::r32(a.wrapping_add({}));", first + i, 4 * i).unwrap();
            } else {
                write!(b, " rt::w32(a.wrapping_add({}), c.s[{}]);", 4 * i, first + i).unwrap();
            }
        }
        t.line(&b);
        return Some(Flow::Next);
    }
    // VLDM/VSTM: IA (P=0,U=1) ou DB com writeback (P=1,U=0,W=1)
    if p == u || rn == 15 {
        return None;
    }
    let words = if dbl { imm8 & !1 } else { imm8 };
    if words == 0 || first + words > 32 {
        return None;
    }
    let mut b = format!("let base = c.r[{rn}];");
    if u {
        b.push_str(" let a = base;");
    } else {
        write!(b, " let a = base.wrapping_sub({});", 4 * imm8).unwrap();
    }
    for i in 0..words {
        if load {
            write!(b, " c.s[{}] = rt::r32(a.wrapping_add({}));", first + i, 4 * i).unwrap();
        } else {
            write!(b, " rt::w32(a.wrapping_add({}), c.s[{}]);", 4 * i, first + i).unwrap();
        }
    }
    if wb {
        if u {
            write!(b, " c.r[{rn}] = base.wrapping_add({});", 4 * imm8).unwrap();
        } else {
            write!(b, " c.r[{rn}] = a;").unwrap();
        }
    }
    t.line(&b);
    Some(Flow::Next)
}

/// CDP/MCR/MRC: VFP (processamento e transferências de 32 bits) e cp15.
fn coproc_dp(t: &mut T) -> Option<Flow> {
    let w = t.w;
    let cp = bits(w, 11, 8);
    if cp == 15 {
        if !bit(w, 4) {
            return None;
        }
        let rt_ = bits(w, 15, 12);
        if bit(w, 20) {
            // MRC p15, 0, Rt, c13, c0, 3 = TPIDRURO (TLS da thread)
            if w & 0x00ef_0fff == 0x000d_0f70 && bits(w, 23, 21) == 0 {
                t.line(&format!("c.r[{rt_}] = c.tls;"));
                return Some(Flow::Next);
            }
            return None;
        }
        // MCR p15: manutenção de cache / barreiras → nada a fazer no host
        return Some(Flow::Next);
    }
    if cp != 10 && cp != 11 {
        return None;
    }
    let dbl = cp == 11;
    if bit(w, 4) {
        return vfp_xfer(t, dbl);
    }
    // processamento de dados VFP
    let d = bits(w, 22, 22);
    let vd = bits(w, 15, 12);
    let vn = bits(w, 19, 16);
    let nbit = bits(w, 7, 7);
    let vm = bits(w, 3, 0);
    let mbit = bits(w, 5, 5);
    let opc1 = bits(w, 23, 23) << 2 | bits(w, 21, 20);
    let opb = bit(w, 6);
    if dbl && opc1 != 0b111 && (d | nbit | mbit) != 0 {
        return None;
    }
    if dbl && opc1 == 0b111 && matches!(bits(w, 19, 16), 0b0000 | 0b0001 | 0b0100 | 0b0101) && (d | mbit) != 0 {
        return None;
    }
    if dbl && opc1 == 0b111 && matches!(bits(w, 19, 16), 0b0111 | 0b1100 | 0b1101) && mbit != 0 {
        return None;
    }
    let (dd, nn, mm) = if dbl {
        (vd, vn, vm)
    } else {
        (sreg(vd, d), sreg(vn, nbit), sreg(vm, mbit))
    };
    let (ty, rd_, set) = if dbl {
        ("f64", "rt::fin64(c, rt::df(c, {}))", "rt::set_df(c, {}, ")
    } else {
        ("f32", "rt::fin32(c, rt::sf(c, {}))", "rt::set_sf(c, {}, ")
    };
    let get = |i: u32| rd_.replace("{}", &i.to_string());
    let put = |i: u32, e: &str| format!("{}{e});", set.replace("{}", &i.to_string()));
    let (a, n, m) = (get(dd), get(nn), get(mm));
    let three = |e: &str| format!("let a: {ty} = {a}; let n: {ty} = {n}; let m: {ty} = {m}; {}", put(dd, e));
    let body = match opc1 {
        0b000 => three(if opb { "a - n * m" } else { "a + n * m" }),
        0b001 => three(if opb { "-a - n * m" } else { "-a + n * m" }),
        0b010 => three(if opb { "-(n * m)" } else { "n * m" }),
        0b011 => three(if opb { "n - m" } else { "n + m" }),
        0b100 if !opb => three("n / m"),
        0b111 => {
            let opc2 = bits(w, 19, 16);
            let opc3 = bits(w, 7, 6);
            if opc3 & 1 == 0 {
                return None; // VMOV imediato (VFPv3)
            }
            match (opc2, opc3) {
                (0b0000, 0b01) => {
                    if dbl {
                        format!(
                            "c.s[{}] = c.s[{}]; c.s[{}] = c.s[{}];",
                            2 * dd,
                            2 * mm,
                            2 * dd + 1,
                            2 * mm + 1
                        )
                    } else {
                        format!("c.s[{dd}] = c.s[{mm}];")
                    }
                }
                (0b0000, 0b11) => format!("let m: {ty} = {m}; {}", put(dd, "m.abs()")),
                (0b0001, 0b01) => format!("let m: {ty} = {m}; {}", put(dd, "-m")),
                (0b0001, 0b11) => format!("let m: {ty} = {m}; {}", put(dd, "m.sqrt()")),
                (0b0100, _) => format!("let a: {ty} = {a}; let m: {ty} = {m}; rt::vcmp(c, a as f64, m as f64);"),
                (0b0101, _) => format!("let a: {ty} = {a}; rt::vcmp(c, a as f64, 0.0);"),
                (0b0111, 0b11) => {
                    if dbl {
                        // double → single: Sd = Vd:D, Dm
                        let sd = sreg(vd, d);
                        format!("let m = rt::fin64(c, rt::df(c, {vm})); rt::set_sf(c, {sd}, m as f32);")
                    } else {
                        // single → double: Dd = Vd, Sm
                        if d != 0 {
                            return None;
                        }
                        format!("let m = rt::fin32(c, rt::sf(c, {mm})); rt::set_df(c, {vd}, m as f64);")
                    }
                }
                (0b1000, _) => {
                    // inteiro (em Sm) → float
                    let sm = sreg(vm, mbit);
                    let src = if bit(w, 7) {
                        format!("c.s[{sm}] as i32")
                    } else {
                        format!("c.s[{sm}]")
                    };
                    if dbl {
                        if d != 0 {
                            return None;
                        }
                        format!("let v = {src} as f64; rt::set_df(c, {vd}, v);")
                    } else {
                        format!("let v = {src} as f32; rt::set_sf(c, {}, v);", sreg(vd, d))
                    }
                }
                (0b1100, _) | (0b1101, _) => {
                    let sd = sreg(vd, d);
                    let rz = bit(w, 7);
                    let f = if opc2 == 0b1101 { "to_s32" } else { "to_u32" };
                    let src = if dbl {
                        format!("rt::fin64(c, rt::df(c, {vm}))")
                    } else {
                        format!("rt::fin32(c, rt::sf(c, {})) as f64", sreg(vm, mbit))
                    };
                    format!("let v = {src}; c.s[{sd}] = rt::{f}(v, {rz});")
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    t.line(&body);
    Some(Flow::Next)
}

/// VMOV core↔single, core↔escalar, VMRS/VMSR.
fn vfp_xfer(t: &mut T, dbl: bool) -> Option<Flow> {
    let w = t.w;
    let rt_ = bits(w, 15, 12);
    let to_core = bit(w, 20);
    let opc1 = bits(w, 23, 21);
    if !dbl {
        if opc1 == 0 {
            let sn = sreg(bits(w, 19, 16), bits(w, 7, 7));
            if to_core {
                if rt_ == 15 {
                    return None;
                }
                t.line(&format!("c.r[{rt_}] = c.s[{sn}];"));
            } else {
                let v = t.r(rt_);
                t.line(&format!("c.s[{sn}] = {v};"));
            }
            return Some(Flow::Next);
        }
        if opc1 == 0b111 {
            let reg = bits(w, 19, 16);
            if to_core {
                let v = match reg {
                    1 => "c.fpscr".to_string(),
                    0 => "0x4101_20b4u32".to_string(), // FPSID do VFP11 [H]
                    8 => "0x4000_0000u32".to_string(), // FPEXC.EN
                    _ => return None,
                };
                if rt_ == 15 {
                    if reg != 1 {
                        return None;
                    }
                    t.line("rt::fmstat(c);");
                } else {
                    t.line(&format!("c.r[{rt_}] = {v};"));
                }
            } else {
                if reg == 1 {
                    let v = t.r(rt_);
                    t.line(&format!("c.fpscr = {v};"));
                }
                // FPEXC/FPSID: ignorado
            }
            return Some(Flow::Next);
        }
        return None;
    }
    // escalar: vmov.32 d[x], rt / vmov.32 rt, d[x]
    if opc1 & 0b110 != 0 || bits(w, 6, 5) != 0 {
        return None;
    }
    let dd = bits(w, 19, 16) | bits(w, 7, 7) << 4;
    if dd >= 16 {
        return None;
    }
    let idx = 2 * dd + (opc1 & 1);
    if to_core {
        t.line(&format!("c.r[{rt_}] = c.s[{idx}];"));
    } else {
        let v = t.r(rt_);
        t.line(&format!("c.s[{idx}] = {v};"));
    }
    Some(Flow::Next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(words: &[u32]) -> Image {
        let mut bytes = Vec::new();
        for w in words {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        let end = 0x100000 + bytes.len() as u32;
        Image {
            bytes,
            base: 0x100000,
            text_end: end,
            ro_end: end,
        }
    }

    fn tr(w: u32) -> Ins {
        translate(&img(&[w]), 0x100000)
    }

    #[test]
    fn control_flow() {
        assert_eq!(tr(0xe12f_ff1e).flow, Flow::Return); // bx lr
        assert_eq!(tr(0xe8bd_8010).flow, Flow::Return); // pop {r4, pc}
        assert_eq!(tr(0xe49d_f004).flow, Flow::Return); // ldr pc, [sp], #4
        assert_eq!(tr(0xe1a0_f00e).flow, Flow::Return); // mov pc, lr
        assert_eq!(tr(0xeb00_0000).flow, Flow::Call(0x100008)); // bl .+8
        assert_eq!(tr(0x0aff_fffe).flow, Flow::Branch(0x100000)); // beq .
        assert_eq!(tr(0x0aff_fffe).cond, 0);
        assert_eq!(tr(0xe12f_ff33).flow, Flow::CallInd); // blx r3
        assert_eq!(tr(0xe12f_ff13).flow, Flow::JumpInd); // bx r3
        assert_eq!(tr(0xef00_0032).flow, Flow::Next); // svc 0x32
        assert!(tr(0xef00_0032).body.contains("rt::svc(c, 0x32)"));
    }

    #[test]
    fn jump_table() {
        // cmp r0, #2 ; addls pc, pc, r0, lsl #2 ; b default ; b c0 ; b c1 ; b c2
        let i = img(&[0xe350_0002, 0x908f_f100, 0xea00_0010, 0xea00_0010, 0xea00_0010, 0xea00_0010]);
        let x = translate(&i, 0x100004);
        assert_eq!(x.flow, Flow::Table(vec![0x10000c, 0x100010, 0x100014]));
        assert_eq!(x.cond, 9);
    }

    #[test]
    fn decodes_common_forms() {
        for w in [
            0xe1a0_0001u32, // mov r0, r1
            0xe092_0003,    // adds r0, r2, r3
            0xe350_0000,    // cmp r0, #0
            0xe591_0004,    // ldr r0, [r1, #4]
            0xe5c1_0000,    // strb r0, [r1]
            0xe1d1_00b2,    // ldrh r0, [r1, #2]
            0xe1c1_00d8,    // ldrd r0, [r1, #8]
            0xe92d_4010,    // push {r4, lr}
            0xe000_0291,    // mul r0, r1, r2
            0xe081_0392,    // umull r0, r1, r2, r3
            0xe6ef_0071,    // uxtb r0, r1
            0xe6bf_0f31,    // rev r0, r1
            0xe666_0ff1,    // uqsub8 r0, r6, r1
            0xe680_0fb1,    // sel r0, r0, r1
            0xe6e8_0011,    // usat r0, #8, r1
            0xed91_0a01,    // vldr s0, [r1, #4]
            0xee20_0a01,    // vmul.f32 s0, s0, s2
            0xeeb4_0ac1,    // vcmpe.f32 s0, s2
            0xeef1_fa10,    // vmrs APSR_nzcv, fpscr
            0xeeb8_0ac0,    // vcvt.f32.s32 s0, s0
            0xeebd_0ac0,    // vcvt.s32.f32 s0, s0
            0xee10_0a10,    // vmov r0, s0
            0xed2d_8b02,    // vpush {d8}
            0xecbd_8b02,    // vpop {d8}
            0xee1d_0f70,    // mrc p15, 0, r0, c13, c0, 3
            0xe169_0080,    // smulbb r9, r0, r0
            0xe101_0092,    // swp r0, r2, [r1]
            0xe191_0f9f,    // ldrex r0, [r1]
            0xe181_2f90,    // strex r2, r0, [r1]
            0xe320_f000,    // nop
            0xf5d1_f000,    // pld [r1]
            0xec41_0b10,    // vmov d0, r0, r1
            0xe16f_0f11,    // clz r0, r1
        ] {
            let x = tr(w);
            assert_ne!(x.flow, Flow::Undefined, "{w:#010x}");
        }
        assert!(tr(0xee1d_0f70).body.contains("c.tls"));
        assert!(tr(0xe12f_ff1e).body.contains("c.r[14]"));
    }
}
