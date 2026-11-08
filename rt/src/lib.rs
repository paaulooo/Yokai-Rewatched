//! Runtime do código ARM11 recompilado (`yw1-recomp` gera Rust que chama isto).
//!
//! - [`Cpu`]: registradores do ARM (r0–r15, NZCVQ, GE), VFPv2 (s0–s31 = d0–d15) e o ponteiro de TLS da thread.
//! - Memória guest: um único bloco reservado de 4 GB no host; endereço guest `va` = `BASE + va` (o motor reserva e
//!   faz commit das regiões; acesso fora de região mapeada = falha de página no host, como no console).
//! - Semântica das instruções que não cabem numa expressão curta (flags, shifts com carry, aritmética paralela,
//!   saturação, VFP).
//! - Ligações com o resto do programa por símbolo (`extern "Rust"`): `yw1_svc` (kernel HLE, no motor) e
//!   `yw1_dispatch` (tabela de entradas, gerada).
//!
//! Referência das semânticas: ARM Architecture Reference Manual (ARMv6/ARMv7-A, pseudocódigo das instruções).

use std::sync::atomic::{AtomicPtr, Ordering};

#[derive(Clone, Debug)]
pub struct Cpu {
    pub r: [u32; 16],
    pub n: bool,
    pub z: bool,
    pub c: bool,
    pub v: bool,
    pub q: bool,
    /// GE[3:0] (instruções SIMD de 8/16 bits, `SEL`)
    pub ge: u32,
    /// VFP: 32 registros simples; `d[i]` = `s[2i]` (parte baixa) + `s[2i+1]` (parte alta)
    pub s: [u32; 32],
    pub fpscr: u32,
    /// valor lido por `mrc p15, 0, rX, c13, c0, 3` (TLS da thread; o buffer de IPC fica em TLS+0x80)
    pub tls: u32,
    /// função guest em execução (chave; gravada na entrada e depois de cada chamada) — para diagnóstico
    pub func: u32,
}

impl Default for Cpu {
    fn default() -> Self {
        Cpu {
            r: [0; 16],
            n: false,
            z: false,
            c: false,
            v: false,
            q: false,
            ge: 0,
            s: [0; 32],
            // RunFast (FZ | DN), arredondamento ao mais próximo — o que o RVCT configura [H até medir no Azahar]
            fpscr: 0x0300_0000,
            tls: 0,
            func: 0,
        }
    }
}

impl Cpu {
    /// CPSR com as flags (modo usuário, ARM).
    pub fn cpsr(&self) -> u32 {
        (self.n as u32) << 31
            | (self.z as u32) << 30
            | (self.c as u32) << 29
            | (self.v as u32) << 28
            | (self.q as u32) << 27
            | (self.ge & 0xf) << 16
            | 0x10
    }

    /// `MSR CPSR_<máscara>, valor`: só os campos de flags (f = bits 31–24) e GE (s = 23–16) valem em modo usuário.
    pub fn set_cpsr(&mut self, val: u32, mask: u32) {
        if mask & 8 != 0 {
            self.n = val >> 31 & 1 != 0;
            self.z = val >> 30 & 1 != 0;
            self.c = val >> 29 & 1 != 0;
            self.v = val >> 28 & 1 != 0;
            self.q = val >> 27 & 1 != 0;
        }
        if mask & 4 != 0 {
            self.ge = val >> 16 & 0xf;
        }
    }
}

// ---------------------------------------------------------------- memória

static BASE: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());

/// # Safety
/// `base` precisa apontar para uma reserva de 4 GB que dure o programa todo.
pub unsafe fn set_base(base: *mut u8) {
    BASE.store(base, Ordering::Relaxed);
}

#[inline(always)]
pub fn base() -> *mut u8 {
    BASE.load(Ordering::Relaxed)
}

#[inline(always)]
fn at(a: u32) -> *mut u8 {
    // SAFETY: a reserva cobre os 4 GB de endereços guest
    unsafe { base().add(a as usize) }
}

#[inline(always)]
pub fn r8(a: u32) -> u32 {
    unsafe { *at(a) as u32 }
}
#[inline(always)]
pub fn r16(a: u32) -> u32 {
    unsafe { (at(a) as *const u16).read_unaligned() as u32 }
}
#[inline(always)]
pub fn r32(a: u32) -> u32 {
    unsafe { (at(a) as *const u32).read_unaligned() }
}
#[inline(always)]
pub fn w8(a: u32, v: u32) {
    unsafe { *at(a) = v as u8 }
}
#[inline(always)]
pub fn w16(a: u32, v: u32) {
    unsafe { (at(a) as *mut u16).write_unaligned(v as u16) }
}
#[inline(always)]
pub fn w32(a: u32, v: u32) {
    unsafe { (at(a) as *mut u32).write_unaligned(v) }
}

/// Cópia de/para a memória guest (para o HLE).
pub fn read_bytes(a: u32, out: &mut [u8]) {
    unsafe { std::ptr::copy_nonoverlapping(at(a), out.as_mut_ptr(), out.len()) }
}
pub fn write_bytes(a: u32, data: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), at(a), data.len()) }
}
/// Ponteiro host para um endereço guest (o chamador garante que a região está mapeada).
pub fn host_ptr(a: u32) -> *mut u8 {
    at(a)
}

// ---------------------------------------------------------------- ligações

/// Preempção pendente: o kernel liga quando uma interrupção externa (VBlank, DSP…) acorda uma thread guest de
/// prioridade maior que a que está rodando. O código gerado checa em saltos para trás e na entrada de função.
pub static PREEMPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ponto de checagem de preempção (custo: uma leitura atômica relaxada).
#[inline(always)]
pub fn poll(c: &mut Cpu) {
    if PREEMPT.load(Ordering::Relaxed) {
        preempt_slow(c);
    }
}

#[cold]
#[inline(never)]
fn preempt_slow(c: &mut Cpu) {
    unsafe { yw1_preempt(c) }
}

extern "Rust" {
    fn yw1_preempt(c: &mut Cpu);
    fn yw1_svc(c: &mut Cpu, imm: u32);
    fn yw1_dispatch(c: &mut Cpu, va: u32, ret: u32);
}

/// `svc #imm`
#[inline(always)]
pub fn svc(c: &mut Cpu, imm: u32) {
    unsafe { yw1_svc(c, imm) }
}

/// Chamada indireta para `va` (vtables, ponteiros de função): volta para o `lr` recém-gravado.
#[inline(always)]
pub fn call(c: &mut Cpu, va: u32) {
    let ret = c.r[14];
    unsafe { yw1_dispatch(c, va, ret) }
}

/// Salto para fora da função atual (chamada de cauda): o destino volta para quem chamou a função atual (`ret`).
#[inline(always)]
pub fn jump(c: &mut Cpu, va: u32, ret: u32) {
    unsafe { yw1_dispatch(c, va, ret) }
}

/// Entrada inválida numa função recompilada (só acontece se a tabela de despacho estiver errada).
#[cold]
#[inline(never)]
pub fn bad_entry(func: u32, pc: u32) -> ! {
    panic!("[recomp] função {func:#010x} chamada com entrada {pc:#010x} que não é início de bloco")
}

/// `svc 0x3C` (Break) voltou do kernel: o jogo pediu para abortar.
#[cold]
#[inline(never)]
pub fn halted(pc: u32) -> ! {
    panic!("[recomp] o jogo chamou svcBreak em {pc:#010x} (abort/assert)")
}

/// Instrução que o recompilador não traduziu (codificação reservada/indefinida, Thumb...).
#[cold]
#[inline(never)]
pub fn undefined(pc: u32, word: u32) -> ! {
    panic!("[recomp] instrução não suportada em {pc:#010x}: {word:#010x}")
}

// ---------------------------------------------------------------- flags e shifts

/// `a + b + cin` com NZCV (ADD/ADC/CMN; SUB = `adc(a, !b, true)`, SBC = `adc(a, !b, c)`).
#[inline(always)]
pub fn adc(c: &mut Cpu, a: u32, b: u32, cin: bool) -> u32 {
    let wide = a as u64 + b as u64 + cin as u64;
    let r = wide as u32;
    c.n = (r as i32) < 0;
    c.z = r == 0;
    c.c = wide > 0xffff_ffff;
    c.v = ((a ^ r) & (b ^ r)) >> 31 != 0;
    r
}

#[inline(always)]
pub fn sbc(c: &mut Cpu, a: u32, b: u32, cin: bool) -> u32 {
    adc(c, a, !b, cin)
}

#[inline(always)]
pub fn nz(c: &mut Cpu, r: u32) -> u32 {
    c.n = (r as i32) < 0;
    c.z = r == 0;
    r
}

/// Shift por registrador (byte baixo de Rs), sem carry.
#[inline(always)]
pub fn lsl(v: u32, n: u32) -> u32 {
    let n = n & 0xff;
    if n >= 32 {
        0
    } else {
        v << n
    }
}
#[inline(always)]
pub fn lsr(v: u32, n: u32) -> u32 {
    let n = n & 0xff;
    if n >= 32 {
        0
    } else {
        v >> n
    }
}
#[inline(always)]
pub fn asr(v: u32, n: u32) -> u32 {
    let n = (n & 0xff).min(31);
    ((v as i32) >> n) as u32
}
#[inline(always)]
pub fn ror(v: u32, n: u32) -> u32 {
    v.rotate_right(n & 31)
}

/// Shift por registrador com carry de saída (para instruções lógicas com S).
#[inline(always)]
pub fn lsl_c(v: u32, n: u32, cin: bool) -> (u32, bool) {
    match n & 0xff {
        0 => (v, cin),
        n @ 1..=31 => (v << n, v >> (32 - n) & 1 != 0),
        32 => (0, v & 1 != 0),
        _ => (0, false),
    }
}
#[inline(always)]
pub fn lsr_c(v: u32, n: u32, cin: bool) -> (u32, bool) {
    match n & 0xff {
        0 => (v, cin),
        n @ 1..=31 => (v >> n, v >> (n - 1) & 1 != 0),
        32 => (0, v >> 31 != 0),
        _ => (0, false),
    }
}
#[inline(always)]
pub fn asr_c(v: u32, n: u32, cin: bool) -> (u32, bool) {
    match n & 0xff {
        0 => (v, cin),
        n @ 1..=31 => (((v as i32) >> n) as u32, v >> (n - 1) & 1 != 0),
        _ => (((v as i32) >> 31) as u32, v >> 31 != 0),
    }
}
#[inline(always)]
pub fn ror_c(v: u32, n: u32, cin: bool) -> (u32, bool) {
    let n8 = n & 0xff;
    if n8 == 0 {
        return (v, cin);
    }
    let r = v.rotate_right(n8 & 31);
    (r, r >> 31 != 0)
}

// ---------------------------------------------------------------- saturação

#[inline(always)]
pub fn ssat(c: &mut Cpu, v: i64, bits: u32) -> u32 {
    let hi = (1i64 << (bits - 1)) - 1;
    let lo = -(1i64 << (bits - 1));
    if v > hi {
        c.q = true;
        hi as u32
    } else if v < lo {
        c.q = true;
        lo as u32
    } else {
        v as u32
    }
}

#[inline(always)]
pub fn usat(c: &mut Cpu, v: i64, bits: u32) -> u32 {
    let hi = (1i64 << bits) - 1;
    if v > hi {
        c.q = true;
        hi as u32
    } else if v < 0 {
        c.q = true;
        0
    } else {
        v as u32
    }
}

#[inline(always)]
pub fn ssat16(c: &mut Cpu, v: u32, bits: u32) -> u32 {
    let lo = ssat(c, (v as u16 as i16) as i64, bits) & 0xffff;
    let hi = ssat(c, ((v >> 16) as u16 as i16) as i64, bits) & 0xffff;
    lo | hi << 16
}

#[inline(always)]
pub fn usat16(c: &mut Cpu, v: u32, bits: u32) -> u32 {
    let lo = usat(c, (v as u16 as i16) as i64, bits) & 0xffff;
    let hi = usat(c, ((v >> 16) as u16 as i16) as i64, bits) & 0xffff;
    lo | hi << 16
}

/// QADD/QSUB/QDADD/QDSUB
#[inline(always)]
pub fn qadd(c: &mut Cpu, a: u32, b: u32) -> u32 {
    ssat(c, a as i32 as i64 + b as i32 as i64, 32)
}
#[inline(always)]
pub fn qsub(c: &mut Cpu, a: u32, b: u32) -> u32 {
    ssat(c, a as i32 as i64 - b as i32 as i64, 32)
}
#[inline(always)]
pub fn qdadd(c: &mut Cpu, a: u32, b: u32) -> u32 {
    let d = ssat(c, 2 * (b as i32 as i64), 32);
    ssat(c, a as i32 as i64 + d as i32 as i64, 32)
}
#[inline(always)]
pub fn qdsub(c: &mut Cpu, a: u32, b: u32) -> u32 {
    let d = ssat(c, 2 * (b as i32 as i64), 32);
    ssat(c, a as i32 as i64 - d as i32 as i64, 32)
}

/// Soma com Q (SMLAxy, SMLAWy, SMUAD, SMLAD): resultado de 32 bits, Q se estourou.
#[inline(always)]
pub fn add_q(c: &mut Cpu, a: i64, b: i64) -> u32 {
    let r = a + b;
    if r != r as i32 as i64 {
        c.q = true;
    }
    r as u32
}

// ---------------------------------------------------------------- SIMD de 8/16 bits

/// Aritmética paralela (ARM ARM "Parallel addition and subtraction").
/// `pre`: 1 = S, 2 = Q, 3 = SH, 5 = U, 6 = UQ, 7 = UH (bit 2 = sem sinal, bits 1–0 = op1).
/// `kind`: 0 ADD16, 1 ASX, 2 SAX, 3 SUB16, 4 ADD8, 7 SUB8.
#[inline(always)]
pub fn parallel(c: &mut Cpu, pre: u32, kind: u32, a: u32, b: u32) -> u32 {
    let unsigned = pre & 4 != 0;
    let mode = pre & 3;
    let h = |x: u32, i: u32| -> i32 {
        let v = (x >> (16 * i)) & 0xffff;
        if unsigned {
            v as i32
        } else {
            v as u16 as i16 as i32
        }
    };
    let by = |x: u32, i: u32| -> i32 {
        let v = (x >> (8 * i)) & 0xff;
        if unsigned {
            v as i32
        } else {
            v as u8 as i8 as i32
        }
    };
    // (resultado completo, foi soma?) por faixa
    let mut lanes = [(0i32, false); 4];
    let (n, width) = match kind {
        0 => {
            lanes[0] = (h(a, 0) + h(b, 0), true);
            lanes[1] = (h(a, 1) + h(b, 1), true);
            (2, 16)
        }
        1 => {
            lanes[0] = (h(a, 0) - h(b, 1), false);
            lanes[1] = (h(a, 1) + h(b, 0), true);
            (2, 16)
        }
        2 => {
            lanes[0] = (h(a, 0) + h(b, 1), true);
            lanes[1] = (h(a, 1) - h(b, 0), false);
            (2, 16)
        }
        3 => {
            lanes[0] = (h(a, 0) - h(b, 0), false);
            lanes[1] = (h(a, 1) - h(b, 1), false);
            (2, 16)
        }
        4 => {
            for (i, l) in lanes.iter_mut().enumerate() {
                *l = (by(a, i as u32) + by(b, i as u32), true);
            }
            (4, 8)
        }
        _ => {
            for (i, l) in lanes.iter_mut().enumerate() {
                *l = (by(a, i as u32) - by(b, i as u32), false);
            }
            (4, 8)
        }
    };
    let mask = (1u32 << width) - 1;
    let mut out = 0u32;
    let mut ge = 0u32;
    for (i, &(v, add)) in lanes.iter().take(n).enumerate() {
        let r = match mode {
            1 => {
                // GE: com sinal → resultado ≥ 0; sem sinal → soma com carry / subtração sem empréstimo
                let g = if unsigned {
                    if add {
                        v >= 1 << width
                    } else {
                        v >= 0
                    }
                } else {
                    v >= 0
                };
                if g {
                    ge |= if width == 16 { 3 << (2 * i) } else { 1 << i };
                }
                v as u32
            }
            2 => {
                if unsigned {
                    v.clamp(0, mask as i32) as u32
                } else {
                    let lim = 1i32 << (width - 1);
                    v.clamp(-lim, lim - 1) as u32
                }
            }
            _ => (v >> 1) as u32,
        };
        out |= (r & mask) << (width * i as u32);
    }
    if mode == 1 {
        c.ge = ge;
    }
    out
}

/// SEL: byte i de `a` se GE[i], senão de `b`.
#[inline(always)]
pub fn sel(c: &Cpu, a: u32, b: u32) -> u32 {
    let mut m = 0u32;
    for i in 0..4 {
        if c.ge >> i & 1 != 0 {
            m |= 0xff << (8 * i);
        }
    }
    (a & m) | (b & !m)
}

/// USAD8: soma das diferenças absolutas dos 4 bytes.
#[inline(always)]
pub fn usad8(a: u32, b: u32) -> u32 {
    (0..4)
        .map(|i| ((a >> (8 * i)) & 0xff).abs_diff((b >> (8 * i)) & 0xff))
        .sum()
}

#[inline(always)]
pub fn lo16(x: u32) -> i32 {
    x as u16 as i16 as i32
}
#[inline(always)]
pub fn hi16(x: u32) -> i32 {
    (x >> 16) as u16 as i16 as i32
}

// ---------------------------------------------------------------- VFP

#[inline(always)]
pub fn sf(c: &Cpu, i: usize) -> f32 {
    f32::from_bits(c.s[i])
}
#[inline(always)]
pub fn set_sf(c: &mut Cpu, i: usize, v: f32) {
    c.s[i] = flush32(c, v).to_bits();
}
#[inline(always)]
pub fn d_bits(c: &Cpu, i: usize) -> u64 {
    c.s[2 * i] as u64 | (c.s[2 * i + 1] as u64) << 32
}
#[inline(always)]
pub fn set_d_bits(c: &mut Cpu, i: usize, v: u64) {
    c.s[2 * i] = v as u32;
    c.s[2 * i + 1] = (v >> 32) as u32;
}
#[inline(always)]
pub fn df(c: &Cpu, i: usize) -> f64 {
    f64::from_bits(d_bits(c, i))
}
#[inline(always)]
pub fn set_df(c: &mut Cpu, i: usize, v: f64) {
    let v = flush64(c, v);
    set_d_bits(c, i, v.to_bits())
}

/// Flush-to-zero (FPSCR.FZ): subnormais viram zero com o mesmo sinal.
#[inline(always)]
fn flush32(c: &Cpu, v: f32) -> f32 {
    if c.fpscr & (1 << 24) != 0 && v.is_subnormal() {
        0.0f32.copysign(v)
    } else {
        v
    }
}
#[inline(always)]
fn flush64(c: &Cpu, v: f64) -> f64 {
    if c.fpscr & (1 << 24) != 0 && v.is_subnormal() {
        0.0f64.copysign(v)
    } else {
        v
    }
}
/// Entradas também passam pelo flush (FZ vale para operandos).
#[inline(always)]
pub fn fin32(c: &Cpu, v: f32) -> f32 {
    flush32(c, v)
}
#[inline(always)]
pub fn fin64(c: &Cpu, v: f64) -> f64 {
    flush64(c, v)
}

/// VCMP/VCMPE: NZCV do FPSCR (igual 0110, menor 1000, maior 0010, não ordenado 0011).
#[inline(always)]
pub fn vcmp(c: &mut Cpu, a: f64, b: f64) {
    let f = if a.is_nan() || b.is_nan() {
        0b0011
    } else if a == b {
        0b0110
    } else if a < b {
        0b1000
    } else {
        0b0010
    };
    c.fpscr = (c.fpscr & 0x0fff_ffff) | f << 28;
}

/// `vmrs APSR_nzcv, fpscr`
#[inline(always)]
pub fn fmstat(c: &mut Cpu) {
    let f = c.fpscr >> 28;
    c.n = f & 8 != 0;
    c.z = f & 4 != 0;
    c.c = f & 2 != 0;
    c.v = f & 1 != 0;
}

/// Conversões para inteiro: `rz` = arredondar para zero (VCVT), senão o modo do FPSCR (VCVTR; só "mais próximo"
/// é usado pelo jogo [H]).
#[inline(always)]
pub fn to_s32(v: f64, rz: bool) -> u32 {
    (if rz { v } else { v.round_ties_even() }) as i32 as u32
}
#[inline(always)]
pub fn to_u32(v: f64, rz: bool) -> u32 {
    (if rz { v } else { v.round_ties_even() }) as u32
}

/// Sinaliza que o código gerado precisa saber em que endereço uma falha aconteceu: o motor lê isto ao capturar um
/// panic (atualizado só nos pontos de chamada, custo baixo).
pub static LAST_CALL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[no_mangle]
    fn yw1_svc(_c: &mut Cpu, _imm: u32) {}
    #[no_mangle]
    fn yw1_preempt(_c: &mut Cpu) {}
    #[no_mangle]
    fn yw1_dispatch(_c: &mut Cpu, _va: u32, _ret: u32) {}

    #[test]
    fn add_sub_flags() {
        let mut c = Cpu::default();
        // 0x7fffffff + 1 → overflow com sinal
        assert_eq!(adc(&mut c, 0x7fff_ffff, 1, false), 0x8000_0000);
        assert!(c.n && !c.z && !c.c && c.v);
        // 0xffffffff + 1 → carry, zero
        assert_eq!(adc(&mut c, 0xffff_ffff, 1, false), 0);
        assert!(!c.n && c.z && c.c && !c.v);
        // CMP 5, 5 → Z, C (sem empréstimo)
        sbc(&mut c, 5, 5, true);
        assert!(c.z && c.c && !c.n && !c.v);
        // CMP 3, 5 → N, empréstimo (C=0)
        sbc(&mut c, 3, 5, true);
        assert!(c.n && !c.c);
        // CMP 0x80000000, 1 → V
        sbc(&mut c, 0x8000_0000, 1, true);
        assert!(c.v && !c.n && c.c);
        // SBC com carry 0 = a - b - 1
        assert_eq!(sbc(&mut c, 10, 3, false), 6);
        // RSC/ADC com carry 1
        assert_eq!(adc(&mut c, 1, 2, true), 4);
    }

    #[test]
    fn shifts_by_register() {
        assert_eq!(lsl_c(0x8000_0001, 1, false), (2, true));
        assert_eq!(lsl_c(1, 32, false), (0, true));
        assert_eq!(lsl_c(1, 33, true), (0, false));
        assert_eq!(lsr_c(0x8000_0000, 32, false), (0, true));
        assert_eq!(asr_c(0x8000_0000, 40, false), (0xffff_ffff, true));
        assert_eq!(ror_c(0x0000_0001, 32, false), (1, false));
        assert_eq!(ror_c(0x8000_0000, 32, false), (0x8000_0000, true));
        assert_eq!(ror_c(3, 1, false), (0x8000_0001, true));
        assert_eq!(lsl_c(5, 0, true), (5, true));
        assert_eq!(asr(0x8000_0000, 255), 0xffff_ffff);
        assert_eq!(lsr(0xffff_ffff, 256), 0xffff_ffff); // só o byte baixo conta
    }

    #[test]
    fn parallel_ops() {
        let mut c = Cpu::default();
        // UQSUB8: satura em 0
        assert_eq!(parallel(&mut c, 6, 7, 0x10_20_30_40, 0x20_10_40_30), 0x00_10_00_10);
        // UADD8: GE = carry por byte
        assert_eq!(parallel(&mut c, 5, 4, 0xff_01_80_00, 0x01_01_80_00), 0x00_02_00_00);
        assert_eq!(c.ge, 0b1010);
        // USUB8: GE = sem empréstimo
        parallel(&mut c, 5, 7, 0x05_01_00_10, 0x04_02_00_20);
        assert_eq!(c.ge, 0b1010);
        assert_eq!(sel(&c, 0xaa_bb_cc_dd, 0x11_22_33_44), 0xaa_22_cc_44);
        // UHADD8: (a+b)>>1 por byte
        assert_eq!(parallel(&mut c, 7, 4, 0xff_02_00_10, 0xff_04_00_20), 0xff_03_00_18);
        // SADD16 com GE por metade
        assert_eq!(parallel(&mut c, 1, 0, 0xffff_0001, 0xffff_0001), 0xfffe_0002);
        assert_eq!(c.ge, 0b0011);
        // QADD16 satura com sinal
        assert_eq!(parallel(&mut c, 2, 0, 0x7fff_8000, 0x0001_ffff), 0x7fff_8000);
        // SSUB8
        assert_eq!(parallel(&mut c, 1, 7, 0x00_00_00_05, 0x00_00_00_06) & 0xff, 0xff);
        assert_eq!(c.ge & 1, 0);
    }

    #[test]
    fn saturation() {
        let mut c = Cpu::default();
        assert_eq!(usat(&mut c, -5, 8), 0);
        assert!(c.q);
        c.q = false;
        assert_eq!(usat(&mut c, 300, 8), 255);
        assert_eq!(ssat(&mut c, 200, 8), 127);
        assert_eq!(ssat(&mut c, -200, 8) as i32, -128);
        assert_eq!(qadd(&mut c, 0x7fff_ffff, 1), 0x7fff_ffff);
        assert_eq!(usad8(0x01_02_03_04, 0x04_03_02_01), 3 + 1 + 1 + 3);
    }

    #[test]
    fn vfp_compare_and_convert() {
        let mut c = Cpu::default();
        vcmp(&mut c, 1.0, 2.0);
        fmstat(&mut c);
        assert!(c.n && !c.z && !c.c && !c.v); // LT
        vcmp(&mut c, 2.0, 2.0);
        fmstat(&mut c);
        assert!(c.z && c.c);
        vcmp(&mut c, f64::NAN, 2.0);
        fmstat(&mut c);
        assert!(c.c && c.v);
        assert_eq!(to_s32(-1.7, true) as i32, -1);
        assert_eq!(to_s32(2.5, false), 2);
        assert_eq!(to_u32(-3.0, true), 0);
        assert_eq!(to_s32(1e20, true), 0x7fff_ffff);
        set_df(&mut c, 1, 1.5);
        assert_eq!(df(&c, 1), 1.5);
        assert_eq!(c.s[2], 0);
        assert_eq!(c.s[3], 0x3ff8_0000);
    }
}
