//! Shader de vértice do PICA200 executado na CPU (backend de referência `soft`; o Vulkan/DX12 traduz para SPIR-V/HLSL).
//!
//! Codificação conforme o nihstro (`shader_bytecode.h`) e a semântica do interpretador do Citra/Azahar
//! (`shader_interpreter.cpp`), já conferidas contra o shader real da captura em docs/GPU-PICA.md:
//! - formato 1 (ADD…MOV, CMP): desc 0–6, src2 7–11, src1 12–18, idx 19–20, dest 21–25 (CMP: op y 21–23, op x 24–26)
//! - formato 1i (DPHI/DSTI/SGEI/SLTI): src2 7–13 (7 bits, com idx), src1 14–18
//! - MAD (0x38–0x3F): desc 0–4, src3 5–9, src2 10–16 (idx), src1 17–21, idx 22–23, dest 24–28
//! - MADI (0x30–0x37): desc 0–4, src3 5–11 (idx), src2 12–16, src1 17–21, idx 22–23, dest 24–28
//! - fluxo: num 0–7, destino 10–21, op/refy/refx 22–25 (condicional) ou id do uniform bool/int 22–25
//! - descritor: máscara de destino 0–3 (bit 3 = x), neg1 4, sel1 5–12, neg2 13, sel2 14–21, neg3 22, sel3 23–30
//!   (seletor de 8 bits: x nos bits 7–6, y 5–4, z 3–2, w 1–0)
//!
//! Aritmética em f32 do host; multiplicação com a regra do PICA "0 × qualquer coisa = 0" (inclusive ±inf), como o
//! Citra faz em `float24::operator*`. [H] diferenças de precisão do f24 nos demais casos.

pub type V4 = [f32; 4];

/// Programa e constantes (estado de shader do PICA).
pub struct Program {
    pub code: Vec<u32>,
    pub desc: Vec<u32>,
    pub f: Vec<V4>,
    /// inteiros: (x = contagem, y = início, z = passo)
    pub i: [[u8; 4]; 4],
    pub b: u16,
    pub entry: u32,
}

impl Default for Program {
    fn default() -> Self {
        Program {
            code: vec![0; 4096],
            desc: vec![0; 128],
            f: vec![[0.0; 4]; 96],
            i: [[0; 4]; 4],
            b: 0,
            entry: 0,
        }
    }
}

#[inline(always)]
fn pmul(a: f32, b: f32) -> f32 {
    // PICA: 0 * inf = 0 (não NaN)
    if a == 0.0 && !b.is_nan() || b == 0.0 && !a.is_nan() {
        0.0
    } else {
        a * b
    }
}

fn swz(v: V4, sel: u32, neg: bool) -> V4 {
    let mut o = [
        v[((sel >> 6) & 3) as usize],
        v[((sel >> 4) & 3) as usize],
        v[((sel >> 2) & 3) as usize],
        v[(sel & 3) as usize],
    ];
    if neg {
        for x in &mut o {
            *x = -*x;
        }
    }
    o
}

struct Frame {
    final_addr: u32,
    ret: u32,
    repeat: u8,
    incr: u8,
    loop_addr: u32,
    /// quadro de LOOP (BREAK/BREAKC saem do mais interno)
    looped: bool,
}

pub struct Exec<'a> {
    pub p: &'a Program,
    pub input: [V4; 16],
    pub temp: [V4; 16],
    pub out: [V4; 16],
    a0: [i32; 2],
    al: i32,
    cc: [bool; 2],
}

impl<'a> Exec<'a> {
    pub fn new(p: &'a Program) -> Self {
        Exec {
            p,
            input: [[0.0; 4]; 16],
            temp: [[0.0; 4]; 16],
            out: [[0.0, 0.0, 0.0, 1.0]; 16],
            a0: [0; 2],
            al: 0,
            cc: [false; 2],
        }
    }

    /// Lê um registrador-fonte (7 bits: 0x00–0x0F v, 0x10–0x1F r, 0x20–0x7F c). `rel` = deslocamento do endereçamento
    /// relativo (só vale para uniforms).
    fn src(&self, reg: u32, rel: i32) -> V4 {
        match reg {
            0x00..=0x0f => self.input[reg as usize],
            0x10..=0x1f => self.temp[(reg - 0x10) as usize],
            _ => {
                let idx = (reg as i32 - 0x20 + rel) & 0x7f;
                // fora das 96 constantes: o Citra devolve (1,1,1,1) [H]
                if idx < 96 {
                    self.p.f[idx as usize]
                } else {
                    [1.0; 4]
                }
            }
        }
    }

    fn rel(&self, idx: u32) -> i32 {
        match idx {
            1 => self.a0[0],
            2 => self.a0[1],
            3 => self.al,
            _ => 0,
        }
    }

    fn write(&mut self, dest: u32, mask: u32, v: V4) {
        let d = match dest {
            0x00..=0x0f => &mut self.out[dest as usize],
            0x10..=0x1f => &mut self.temp[(dest - 0x10) as usize],
            _ => return,
        };
        for (k, x) in v.iter().enumerate() {
            if mask >> (3 - k) & 1 != 0 {
                d[k] = *x;
            }
        }
    }

    fn cond(&self, w: u32) -> bool {
        let refy = w >> 24 & 1 != 0;
        let refx = w >> 25 & 1 != 0;
        let (x, y) = (self.cc[0] == refx, self.cc[1] == refy);
        match (w >> 22) & 3 {
            0 => x || y,
            1 => x && y,
            2 => x,
            _ => y,
        }
    }

    /// Executa a partir do ponto de entrada até `END` (com limite de passos contra laços infinitos).
    pub fn run(&mut self) {
        let mut pc = self.p.entry;
        let mut stack: Vec<Frame> = Vec::with_capacity(8);
        let mut steps = 0u32;
        loop {
            steps += 1;
            if steps > 100_000 || pc as usize >= self.p.code.len() {
                return;
            }
            if let Some(top) = stack.last_mut() {
                if pc == top.final_addr {
                    self.al = self.al.wrapping_add(top.incr as i8 as i32);
                    if top.repeat == 0 {
                        pc = top.ret;
                        stack.pop();
                    } else {
                        top.repeat -= 1;
                        pc = top.loop_addr;
                    }
                    continue;
                }
            }
            let w = self.p.code[pc as usize];
            let op = w >> 26;
            let mut next = pc + 1;
            let call = |stack: &mut Vec<Frame>, off: u32, num: u32, ret: u32, repeat: u8, incr: u8, looped: bool| {
                stack.push(Frame {
                    final_addr: off + num,
                    ret,
                    repeat,
                    incr,
                    loop_addr: off,
                    looped,
                });
                off
            };
            let brk = |stack: &mut Vec<Frame>| -> Option<u32> {
                let pos = stack.iter().rposition(|f| f.looped)?;
                let r = stack[pos].ret;
                stack.truncate(pos);
                Some(r)
            };
            match op {
                // formato 1 / 1i / CMP
                0x00..=0x1b | 0x2e | 0x2f => {
                    let d = self.p.desc[(w & 0x7f) as usize];
                    let inv = (0x18..=0x1b).contains(&op);
                    let idx = (w >> 19) & 3;
                    let (r1, r2) = if inv {
                        (self.src((w >> 14) & 0x1f, 0), self.src((w >> 7) & 0x7f, self.rel(idx)))
                    } else {
                        (self.src((w >> 12) & 0x7f, self.rel(idx)), self.src((w >> 7) & 0x1f, 0))
                    };
                    let s1 = swz(r1, (d >> 5) & 0xff, d >> 4 & 1 != 0);
                    let s2 = swz(r2, (d >> 14) & 0xff, d >> 13 & 1 != 0);
                    let mask = d & 0xf;
                    let dest = (w >> 21) & 0x1f;
                    let dot = |n: usize, s1: &V4, s2: &V4| -> f32 { (0..n).map(|k| pmul(s1[k], s2[k])).sum() };
                    match op {
                        0x00 => self.write(dest, mask, [s1[0] + s2[0], s1[1] + s2[1], s1[2] + s2[2], s1[3] + s2[3]]),
                        0x01 => {
                            let v = dot(3, &s1, &s2);
                            self.write(dest, mask, [v; 4])
                        }
                        0x02 => {
                            let v = dot(4, &s1, &s2);
                            self.write(dest, mask, [v; 4])
                        }
                        0x03 | 0x18 => {
                            let v = dot(3, &s1, &s2) + s2[3];
                            self.write(dest, mask, [v; 4])
                        }
                        0x04 | 0x19 => self.write(dest, mask, [1.0, pmul(s1[1], s2[1]), s1[2], s2[3]]),
                        0x05 => self.write(dest, mask, [s1[0].exp2(); 4]),
                        0x06 => self.write(dest, mask, [s1[0].log2(); 4]),
                        0x07 => {
                            // LITP (Citra): x = max(s.x, 0), y = clamp(s.y, -127.9961, 127.9961), z = s.z,
                            // w = max(s.w, 0); cc = (s.x > 0, s.w > 0)
                            let r = [s1[0].max(0.0), s1[1].clamp(-127.9961, 127.9961), s1[2], s1[3].max(0.0)];
                            self.cc = [s1[0] > 0.0, s1[3] > 0.0];
                            self.write(dest, mask, r)
                        }
                        0x08 => self.write(dest, mask, [0, 1, 2, 3].map(|k| pmul(s1[k], s2[k]))),
                        0x09 | 0x1a => self.write(dest, mask, [0, 1, 2, 3].map(|k| (s1[k] >= s2[k]) as u32 as f32)),
                        0x0a | 0x1b => self.write(dest, mask, [0, 1, 2, 3].map(|k| (s1[k] < s2[k]) as u32 as f32)),
                        0x0b => self.write(dest, mask, s1.map(f32::floor)),
                        0x0c => self.write(dest, mask, [0, 1, 2, 3].map(|k| if s1[k] > s2[k] { s1[k] } else { s2[k] })),
                        0x0d => self.write(dest, mask, [0, 1, 2, 3].map(|k| if s1[k] < s2[k] { s1[k] } else { s2[k] })),
                        0x0e => self.write(dest, mask, [1.0 / s1[0]; 4]),
                        0x0f => self.write(dest, mask, [1.0 / s1[0].sqrt(); 4]),
                        0x12 => {
                            if mask & 8 != 0 {
                                self.a0[0] = s1[0] as i32;
                            }
                            if mask & 4 != 0 {
                                self.a0[1] = s1[1] as i32;
                            }
                        }
                        0x13 => self.write(dest, mask, s1),
                        0x2e | 0x2f => {
                            let c = |a: f32, b: f32, o: u32| match o {
                                0 => a == b,
                                1 => a != b,
                                2 => a < b,
                                3 => a <= b,
                                4 => a > b,
                                5 => a >= b,
                                _ => true,
                            };
                            let ox = (w >> 24) & 7;
                            let oy = (w >> 21) & 7;
                            self.cc = [c(s1[0], s2[0], ox), c(s1[1], s2[1], oy)];
                        }
                        _ => {}
                    }
                }
                // MADI / MAD
                0x30..=0x3f => {
                    let d = self.p.desc[(w & 0x1f) as usize];
                    let idx = (w >> 22) & 3;
                    let rel = self.rel(idx);
                    let (r1, r2, r3) = if op >= 0x38 {
                        (self.src((w >> 17) & 0x1f, 0), self.src((w >> 10) & 0x7f, rel), self.src((w >> 5) & 0x1f, 0))
                    } else {
                        (self.src((w >> 17) & 0x1f, 0), self.src((w >> 12) & 0x1f, 0), self.src((w >> 5) & 0x7f, rel))
                    };
                    let s1 = swz(r1, (d >> 5) & 0xff, d >> 4 & 1 != 0);
                    let s2 = swz(r2, (d >> 14) & 0xff, d >> 13 & 1 != 0);
                    let s3 = swz(r3, (d >> 23) & 0xff, d >> 22 & 1 != 0);
                    let v = [0, 1, 2, 3].map(|k| pmul(s1[k], s2[k]) + s3[k]);
                    self.write((w >> 24) & 0x1f, d & 0xf, v);
                }
                // BREAK: sai do laço mais interno
                0x20 => {
                    if let Some(r) = brk(&mut stack) {
                        next = r;
                    }
                }
                0x21 => {}
                0x22 => return,
                // BREAKC
                0x23 => {
                    if self.cond(w) {
                        if let Some(r) = brk(&mut stack) {
                            next = r;
                        }
                    }
                }
                // CALL / CALLC / CALLU
                0x24 | 0x25 | 0x26 => {
                    let num = w & 0xff;
                    let dst = (w >> 10) & 0xfff;
                    let go = match op {
                        0x24 => true,
                        0x25 => self.cond(w),
                        _ => self.p.b >> ((w >> 22) & 0xf) & 1 != 0,
                    };
                    if go {
                        next = call(&mut stack, dst, num, pc + 1, 0, 0, false);
                    }
                }
                // IFU / IFC
                0x27 | 0x28 => {
                    let num = w & 0xff;
                    let dst = (w >> 10) & 0xfff;
                    let c = if op == 0x27 {
                        self.p.b >> ((w >> 22) & 0xf) & 1 != 0
                    } else {
                        self.cond(w)
                    };
                    next = if c {
                        call(&mut stack, pc + 1, dst - pc - 1, dst + num, 0, 0, false)
                    } else {
                        call(&mut stack, dst, num, dst + num, 0, 0, false)
                    };
                }
                // LOOP
                0x29 => {
                    let dst = (w >> 10) & 0xfff;
                    let iu = self.p.i[((w >> 22) & 3) as usize];
                    self.al = iu[1] as i32;
                    next = call(&mut stack, pc + 1, dst - pc, dst + 1, iu[0], iu[2], true);
                }
                // JMPC / JMPU
                0x2c => {
                    if self.cond(w) {
                        next = (w >> 10) & 0xfff;
                    }
                }
                0x2d => {
                    let b = self.p.b >> ((w >> 22) & 0xf) & 1 != 0;
                    if b == (w & 1 == 0) {
                        next = (w >> 10) & 0xfff;
                    }
                }
                _ => {}
            }
            pc = next;
        }
    }
}

/// f24 do PICA (1 sinal, 7 expoente com viés 63, 16 mantissa) → f32.
pub fn f24(v: u32) -> f32 {
    let v = v & 0xff_ffff;
    if v & 0x7f_ffff == 0 {
        return if v >> 23 != 0 { -0.0 } else { 0.0 };
    }
    let sign = (v >> 23) & 1;
    let exp = (v >> 16) & 0x7f;
    let mant = v & 0xffff;
    f32::from_bits(sign << 31 | (exp + 64) << 23 | mant << 7)
}

/// 4 f24 empacotados em 3 palavras na ordem de escrita (w, z, y, x) — `Citra ShaderSetup` (uniforms e atributos fixos).
pub fn unpack_f24x4(b: &[u32; 4]) -> V4 {
    let w = f24(b[0] >> 8);
    let z = f24((b[0] & 0xff) << 16 | (b[1] >> 16) & 0xffff);
    let y = f24((b[1] & 0xffff) << 8 | (b[2] >> 24) & 0xff);
    let x = f24(b[2] & 0xff_ffff);
    [x, y, z, w]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f24_values() {
        // valores reais do VIEWPORT da captura (docs/GPU-PICA.md)
        assert_eq!(f24(0x46e000), 240.0);
        assert_eq!(f24(0x469000), 200.0);
        assert_eq!(f24(0x3f0000), 1.0);
        assert_eq!(f24(0xbf0000), -1.0);
    }

    #[test]
    fn mov_mul_dp4() {
        let mut p = Program::default();
        // desc 0: máscara xyzw, seletores identidade (x=0,y=1,z=2,w=3 → 0b00_01_10_11 = 0x1b) para src1 e src2
        p.desc[0] = 0xf | 0x1b << 5 | 0x1b << 14;
        // mul o0, v0, c0  (op 0x08, dest 0, src1 = 0x20 (c0), src2 = 0x00 (v0))
        p.code[0] = 0x08 << 26 | 0 << 21 | 0x20 << 12 | 0 << 7;
        // dp4 o1, c0, v0
        p.code[1] = 0x02 << 26 | 1 << 21 | 0x20 << 12 | 0 << 7;
        p.code[2] = 0x22 << 26;
        p.f[0] = [2.0, 3.0, 0.0, 1.0];
        let mut e = Exec::new(&p);
        e.input[0] = [1.0, 2.0, f32::INFINITY, 4.0];
        e.run();
        assert_eq!(e.out[0], [2.0, 6.0, 0.0, 4.0]); // 0 * inf = 0
        assert_eq!(e.out[1], [12.0; 4]);
    }
}
