//! Decodificação Thumb (16 bits, ARMv6; BL/BLX em par de meias-palavras) → Rust.
//!
//! Medido no `code.bin`: há pouco código Thumb (≈30 alvos de `BLX imm` vindos do ARM, rotinas de biblioteca), mas ele
//! existe — a hipótese "só ARM" do plano estava errada.
//! Convenções iguais às de `arm.rs`; leitura de `r15` = endereço + 4 (alinhado a 4 nos acessos relativos ao pc).
//! Chaves de função: endereço | 1 = Thumb (é também o valor que `bx`/`blx` recebem).

use crate::arm::{Flow, Image, Ins};
use std::fmt::Write;

fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}

fn half(img: &Image, va: u32) -> Option<u32> {
    let w = img.word(va & !3)?;
    Some(if va & 2 != 0 { w >> 16 } else { w & 0xffff })
}

fn r(n: u32, pc: u32) -> String {
    if n == 15 {
        format!("{:#x}u32", pc + 4)
    } else {
        format!("c.r[{n}]")
    }
}

fn mk(cond: u32, flow: Flow, body: String, len: u32) -> Ins {
    Ins {
        cond,
        flow,
        body,
        len,
    }
}

fn undef(pc: u32, h: u32) -> Ins {
    mk(
        14,
        Flow::Undefined,
        format!("rt::undefined({pc:#x}, {h:#x});"),
        2,
    )
}

/// Traduz a instrução Thumb em `pc` (par).
pub fn translate(img: &Image, pc: u32) -> Ins {
    let Some(h) = half(img, pc) else {
        return undef(pc, 0);
    };
    let ok = |body: String| mk(14, Flow::Next, body, 2);
    let rd = bits(h, 2, 0);
    let rm3 = bits(h, 5, 3);
    match h >> 11 {
        // shift imediato
        0b00000..=0b00010 => {
            let n = bits(h, 10, 6);
            let m = format!("c.r[{rm3}]");
            let (v, cy) = match (h >> 11, n) {
                (0, 0) => (m.clone(), "c.c".to_string()),
                (0, n) => (format!("{m} << {n}"), format!("{m} >> {} & 1 != 0", 32 - n)),
                (1, 0) => ("0u32".into(), format!("{m} >> 31 != 0")),
                (1, n) => (format!("{m} >> {n}"), format!("{m} >> {} & 1 != 0", n - 1)),
                (_, 0) => (format!("(({m} as i32) >> 31) as u32"), format!("{m} >> 31 != 0")),
                (_, n) => (format!("(({m} as i32) >> {n}) as u32"), format!("{m} >> {} & 1 != 0", n - 1)),
            };
            ok(format!("let sc = {cy}; let v = {v}; c.r[{rd}] = rt::nz(c, v); c.c = sc;"))
        }
        0b00011 => {
            let rn = rm3;
            let o = if h >> 10 & 1 != 0 {
                format!("{}u32", bits(h, 8, 6))
            } else {
                format!("c.r[{}]", bits(h, 8, 6))
            };
            let f = if h >> 9 & 1 != 0 { "rt::sbc(c, a, o, true)" } else { "rt::adc(c, a, o, false)" };
            ok(format!("let a = c.r[{rn}]; let o: u32 = {o}; c.r[{rd}] = {f};"))
        }
        // MOV/CMP/ADD/SUB imediato de 8 bits
        0b00100..=0b00111 => {
            let d = bits(h, 10, 8);
            let imm = h & 0xff;
            ok(match bits(h, 12, 11) {
                0 => format!("c.r[{d}] = rt::nz(c, {imm});"),
                1 => format!("let a = c.r[{d}]; rt::sbc(c, a, {imm}, true);"),
                2 => format!("let a = c.r[{d}]; c.r[{d}] = rt::adc(c, a, {imm}, false);"),
                _ => format!("let a = c.r[{d}]; c.r[{d}] = rt::sbc(c, a, {imm}, true);"),
            })
        }
        0b01000 => {
            if h >> 10 & 1 == 0 {
                // ALU registrador-registrador (sempre com flags)
                let m = format!("c.r[{rm3}]");
                let d = format!("c.r[{rd}]");
                let b = match bits(h, 9, 6) {
                    0 => format!("let v = {d} & {m}; {d} = rt::nz(c, v);"),
                    1 => format!("let v = {d} ^ {m}; {d} = rt::nz(c, v);"),
                    2 => format!("let (v, sc) = rt::lsl_c({d}, {m}, c.c); {d} = rt::nz(c, v); c.c = sc;"),
                    3 => format!("let (v, sc) = rt::lsr_c({d}, {m}, c.c); {d} = rt::nz(c, v); c.c = sc;"),
                    4 => format!("let (v, sc) = rt::asr_c({d}, {m}, c.c); {d} = rt::nz(c, v); c.c = sc;"),
                    5 => format!("let a = {d}; let o = {m}; let ci = c.c; {d} = rt::adc(c, a, o, ci);"),
                    6 => format!("let a = {d}; let o = {m}; let ci = c.c; {d} = rt::sbc(c, a, o, ci);"),
                    7 => format!("let (v, sc) = rt::ror_c({d}, {m}, c.c); {d} = rt::nz(c, v); c.c = sc;"),
                    8 => format!("let v = {d} & {m}; rt::nz(c, v);"),
                    9 => format!("let o = {m}; {d} = rt::sbc(c, 0, o, true);"),
                    10 => format!("let a = {d}; let o = {m}; rt::sbc(c, a, o, true);"),
                    11 => format!("let a = {d}; let o = {m}; rt::adc(c, a, o, false);"),
                    12 => format!("let v = {d} | {m}; {d} = rt::nz(c, v);"),
                    13 => format!("let v = {d}.wrapping_mul({m}); {d} = rt::nz(c, v);"),
                    14 => format!("let v = {d} & !{m}; {d} = rt::nz(c, v);"),
                    _ => format!("let v = !{m}; {d} = rt::nz(c, v);"),
                };
                return ok(b);
            }
            // registradores altos / BX / BLX
            let d = bits(h, 2, 0) | (h >> 7 & 1) << 3;
            let m = bits(h, 6, 3);
            let mv = r(m, pc);
            match bits(h, 9, 8) {
                0 => {
                    if d == 15 {
                        mk(14, Flow::JumpInd, format!("let t = {}u32.wrapping_add({mv}) | 1;", pc + 4), 2)
                    } else {
                        ok(format!("c.r[{d}] = {}.wrapping_add({mv});", r(d, pc)))
                    }
                }
                1 => ok(format!("let a = {}; let o = {mv}; rt::sbc(c, a, o, true);", r(d, pc))),
                2 => {
                    if d == 15 {
                        if m == 14 {
                            mk(14, Flow::Return, "let t = c.r[14];".into(), 2)
                        } else {
                            mk(14, Flow::JumpInd, format!("let t = {mv} | 1;"), 2)
                        }
                    } else {
                        ok(format!("c.r[{d}] = {mv};"))
                    }
                }
                _ => {
                    if h >> 7 & 1 != 0 {
                        mk(
                            14,
                            Flow::CallInd,
                            format!("let t = {mv}; c.r[14] = {:#x}; rt::call(c, t);", (pc + 2) | 1),
                            2,
                        )
                    } else if m == 14 {
                        mk(14, Flow::Return, "let t = c.r[14];".into(), 2)
                    } else {
                        mk(14, Flow::JumpInd, format!("let t = {mv};"), 2)
                    }
                }
            }
        }
        0b01001 => {
            let d = bits(h, 10, 8);
            let a = ((pc + 4) & !3) + (h & 0xff) * 4;
            match img.const_word(a) {
                Some(k) => ok(format!("c.r[{d}] = {k:#x};")),
                None => ok(format!("c.r[{d}] = rt::r32({a:#x});")),
            }
        }
        0b01010 | 0b01011 => {
            let rmr = bits(h, 8, 6);
            let a = format!("let a = c.r[{rm3}].wrapping_add(c.r[{rmr}]);");
            ok(match bits(h, 11, 9) {
                0 => format!("{a} rt::w32(a, c.r[{rd}]);"),
                1 => format!("{a} rt::w16(a, c.r[{rd}]);"),
                2 => format!("{a} rt::w8(a, c.r[{rd}]);"),
                3 => format!("{a} c.r[{rd}] = rt::r8(a) as u8 as i8 as i32 as u32;"),
                4 => format!("{a} c.r[{rd}] = rt::r32(a);"),
                5 => format!("{a} c.r[{rd}] = rt::r16(a);"),
                6 => format!("{a} c.r[{rd}] = rt::r8(a);"),
                _ => format!("{a} c.r[{rd}] = rt::r16(a) as u16 as i16 as i32 as u32;"),
            })
        }
        0b01100..=0b01111 => {
            let byte = h >> 12 & 1 != 0;
            let load = h >> 11 & 1 != 0;
            let off = bits(h, 10, 6) * if byte { 1 } else { 4 };
            let a = format!("let a = c.r[{rm3}].wrapping_add({off});");
            let (rf, wf) = if byte { ("rt::r8", "rt::w8") } else { ("rt::r32", "rt::w32") };
            ok(if load {
                format!("{a} c.r[{rd}] = {rf}(a);")
            } else {
                format!("{a} {wf}(a, c.r[{rd}]);")
            })
        }
        0b10000 | 0b10001 => {
            let off = bits(h, 10, 6) * 2;
            let a = format!("let a = c.r[{rm3}].wrapping_add({off});");
            ok(if h >> 11 & 1 != 0 {
                format!("{a} c.r[{rd}] = rt::r16(a);")
            } else {
                format!("{a} rt::w16(a, c.r[{rd}]);")
            })
        }
        0b10010 | 0b10011 => {
            let d = bits(h, 10, 8);
            let a = format!("let a = c.r[13].wrapping_add({});", (h & 0xff) * 4);
            ok(if h >> 11 & 1 != 0 {
                format!("{a} c.r[{d}] = rt::r32(a);")
            } else {
                format!("{a} rt::w32(a, c.r[{d}]);")
            })
        }
        0b10100 | 0b10101 => {
            let d = bits(h, 10, 8);
            let imm = (h & 0xff) * 4;
            ok(if h >> 11 & 1 != 0 {
                format!("c.r[{d}] = c.r[13].wrapping_add({imm});")
            } else {
                format!("c.r[{d}] = {:#x};", ((pc + 4) & !3) + imm)
            })
        }
        0b10110 | 0b10111 => misc(h, pc),
        0b11000 | 0b11001 => {
            let load = h >> 11 & 1 != 0;
            let n = bits(h, 10, 8);
            let list = h & 0xff;
            if list == 0 {
                return undef(pc, h);
            }
            let mut b = format!("let a = c.r[{n}];");
            let mut k = 0;
            for i in 0..8 {
                if list >> i & 1 == 0 {
                    continue;
                }
                if load {
                    write!(b, " c.r[{i}] = rt::r32(a.wrapping_add({}));", 4 * k).unwrap();
                } else {
                    let v = if i == n { "a".to_string() } else { format!("c.r[{i}]") };
                    write!(b, " rt::w32(a.wrapping_add({}), {v});", 4 * k).unwrap();
                }
                k += 1;
            }
            if !load || list >> n & 1 == 0 {
                write!(b, " c.r[{n}] = a.wrapping_add({});", 4 * k).unwrap();
            }
            ok(b)
        }
        0b11010 | 0b11011 => {
            let cond = bits(h, 11, 8);
            if cond == 15 {
                let body = format!("c.r[15] = {:#x}; rt::svc(c, {});", (pc + 2) | 1, h & 0xff);
                let flow = if h & 0xff == 0x3c { Flow::Halt } else { Flow::Next };
                return mk(14, flow, body, 2);
            }
            if cond == 14 {
                return undef(pc, h);
            }
            let off = ((h & 0xff) as u8 as i8 as i32) << 1;
            mk(cond, Flow::Branch((pc as i32 + 4 + off) as u32 | 1), String::new(), 2)
        }
        0b11100 => {
            let off = (((h & 0x7ff) << 21) as i32) >> 20;
            mk(14, Flow::Branch((pc as i32 + 4 + off) as u32 | 1), String::new(), 2)
        }
        0b11110 => {
            let Some(h2) = half(img, pc + 2) else {
                return undef(pc, h);
            };
            let hi = (((h & 0x7ff) << 21) as i32) >> 9;
            let lo = (h2 & 0x7ff) << 1;
            let t = (pc as i32 + 4 + hi) as u32 + lo;
            match h2 >> 11 {
                0b11111 => mk(14, Flow::Call(t | 1), String::new(), 4),
                0b11101 => mk(14, Flow::Call(t & !3), String::new(), 4),
                _ => undef(pc, h),
            }
        }
        _ => undef(pc, h),
    }
}

fn misc(h: u32, pc: u32) -> Ins {
    let ok = |body: String| mk(14, Flow::Next, body, 2);
    let rd = bits(h, 2, 0);
    let rm = bits(h, 5, 3);
    match bits(h, 11, 8) {
        0b0000 => {
            let imm = (h & 0x7f) * 4;
            if h >> 7 & 1 != 0 {
                ok(format!("c.r[13] = c.r[13].wrapping_sub({imm});"))
            } else {
                ok(format!("c.r[13] = c.r[13].wrapping_add({imm});"))
            }
        }
        0b0010 => ok(match bits(h, 7, 6) {
            0 => format!("c.r[{rd}] = c.r[{rm}] as u16 as i16 as i32 as u32;"),
            1 => format!("c.r[{rd}] = c.r[{rm}] as u8 as i8 as i32 as u32;"),
            2 => format!("c.r[{rd}] = c.r[{rm}] & 0xffff;"),
            _ => format!("c.r[{rd}] = c.r[{rm}] & 0xff;"),
        }),
        0b0100 | 0b0101 => {
            // PUSH {lista, lr?}
            let list = h & 0xff;
            let lr = h >> 8 & 1 != 0;
            let n = list.count_ones() + lr as u32;
            let mut b = format!("let a = c.r[13].wrapping_sub({});", 4 * n);
            let mut k = 0;
            for i in 0..8 {
                if list >> i & 1 != 0 {
                    write!(b, " rt::w32(a.wrapping_add({}), c.r[{i}]);", 4 * k).unwrap();
                    k += 1;
                }
            }
            if lr {
                write!(b, " rt::w32(a.wrapping_add({}), c.r[14]);", 4 * k).unwrap();
            }
            b.push_str(" c.r[13] = a;");
            ok(b)
        }
        0b1100 | 0b1101 => {
            // POP {lista, pc?}
            let list = h & 0xff;
            let pcb = h >> 8 & 1 != 0;
            let mut b = String::from("let a = c.r[13];");
            let mut k = 0;
            for i in 0..8 {
                if list >> i & 1 != 0 {
                    write!(b, " c.r[{i}] = rt::r32(a.wrapping_add({}));", 4 * k).unwrap();
                    k += 1;
                }
            }
            if pcb {
                write!(b, " let t = rt::r32(a.wrapping_add({}));", 4 * k).unwrap();
                k += 1;
            }
            write!(b, " c.r[13] = a.wrapping_add({});", 4 * k).unwrap();
            if pcb {
                mk(14, Flow::Return, b, 2)
            } else {
                ok(b)
            }
        }
        0b1010 => ok(match bits(h, 7, 6) {
            0 => format!("c.r[{rd}] = c.r[{rm}].swap_bytes();"),
            1 => format!("let v = c.r[{rm}]; c.r[{rd}] = ((v & 0x00ff_00ff) << 8) | ((v >> 8) & 0x00ff_00ff);"),
            3 => format!("c.r[{rd}] = ((c.r[{rm}] as u16).swap_bytes() as i16) as i32 as u32;"),
            _ => return undef(pc, h),
        }),
        0b1111 => ok(String::new()), // dicas (nop)
        _ => undef(pc, h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(halves: &[u16]) -> Image {
        let mut bytes = Vec::new();
        for h in halves {
            bytes.extend_from_slice(&h.to_le_bytes());
        }
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let end = 0x100000 + bytes.len() as u32;
        Image { bytes, base: 0x100000, text_end: end, ro_end: end }
    }

    #[test]
    fn thumb_forms() {
        let i = img(&[0xb510, 0xbd10, 0x4770, 0xf000, 0xf802, 0xd0fe, 0x2001, 0x4b02]);
        assert_eq!(translate(&i, 0x100000).flow, Flow::Next); // push {r4, lr}
        assert_eq!(translate(&i, 0x100002).flow, Flow::Return); // pop {r4, pc}
        assert_eq!(translate(&i, 0x100004).flow, Flow::Return); // bx lr
        let bl = translate(&i, 0x100006);
        assert_eq!((bl.flow, bl.len), (Flow::Call(0x10000e | 1), 4)); // bl .+8
        let b = translate(&i, 0x10000a);
        assert_eq!((b.cond, b.flow), (0, Flow::Branch(0x10000a | 1))); // beq .
        assert!(translate(&i, 0x10000c).body.contains("rt::nz(c, 1)")); // movs r0, #1
    }
}
