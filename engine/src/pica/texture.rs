//! Leitura de texels do PICA (Citra `texture_decode.cpp` / `rasterizer.cpp`, e `tools/frame_sim.py` validado na
//! captura): blocos 8×8 em Morton (x no bit 0), linhas de baixo para cima (`t = altura − 1 − t`), ETC1 com 4
//! sub-blocos 4×4 por bloco (ordem x primeiro) e texels em ordem coluna-maior.
//!
//! Formatos: 0 RGBA8 · 1 RGB8 · 2 RGBA5551 · 3 RGB565 · 4 RGBA4 · 5 IA8 · 6 RG8 · 7 I8 · 8 A8 · 9 IA4 · 10 I4 · 11 A4 ·
//! 12 ETC1 · 13 ETC1A4.

use crate::hle::gpu::morton8;
use yw1_rt::{r16, r32, r8};

#[derive(Clone, Copy, Debug, Default)]
pub struct Tex {
    /// endereço virtual guest dos dados
    pub addr: u32,
    pub w: u32,
    pub h: u32,
    pub format: u32,
    pub wrap_s: u32,
    pub wrap_t: u32,
    pub border: [u8; 4],
    pub linear: bool,
}

pub fn bits_per_texel(f: u32) -> u32 {
    match f {
        0 => 32,
        1 => 24,
        2..=6 => 16,
        7..=9 => 8,
        10 | 11 => 4,
        12 => 4,
        _ => 8,
    }
}

const ETC_MOD: [[i32; 2]; 8] = [[2, 8], [5, 17], [9, 29], [13, 42], [18, 60], [24, 80], [33, 106], [47, 183]];

fn etc1(v: u64, x: u32, y: u32) -> [u8; 3] {
    let flip = v >> 32 & 1 != 0;
    let diff = v >> 33 & 1 != 0;
    let second = if flip { y >= 2 } else { x >= 2 };
    let base: [i32; 3] = if diff {
        let c = |hi: u32, lo: u32| -> i32 {
            let b = ((v >> hi) & 31) as i32;
            let d = ((v >> lo) & 7) as i32;
            let d = if d >= 4 { d - 8 } else { d };
            let x = if second { (b + d) & 31 } else { b };
            (x << 3) | (x >> 2)
        };
        [c(59, 56), c(51, 48), c(43, 40)]
    } else {
        let c = |a: u32, b: u32| -> i32 { (((v >> if second { b } else { a }) & 15) as i32) * 17 };
        [c(60, 56), c(52, 48), c(44, 40)]
    };
    let table = ((v >> if second { 34 } else { 37 }) & 7) as usize;
    let i = x * 4 + y;
    let lsb = (v >> i) & 1;
    let msb = (v >> (16 + i)) & 1;
    let m = ETC_MOD[table][lsb as usize];
    let m = if msb != 0 { -m } else { m };
    base.map(|c| (c + m).clamp(0, 255) as u8)
}

/// Texel na coluna `s` e na LINHA DE MEMÓRIA `row` (linha 0 = v perto de 1; ver `fetch`).
pub fn texel(t: &Tex, s: u32, row: u32) -> [u8; 4] {
    let y = row;
    let x = s;
    let tile = (y / 8) * (t.w / 8) + x / 8;
    let (lx, ly) = (x & 7, y & 7);
    match t.format {
        12 | 13 => {
            let alpha = t.format == 13;
            let bs = if alpha { 16 } else { 8 };
            let sub = (lx / 4) + 2 * (ly / 4);
            let base = t.addr + tile * 4 * bs + sub * bs;
            let (xs, ys) = (lx & 3, ly & 3);
            let a = if alpha {
                let al = r32(base) as u64 | (r32(base + 4) as u64) << 32;
                (((al >> (4 * (xs * 4 + ys))) & 15) * 17) as u8
            } else {
                255
            };
            let cb = if alpha { base + 8 } else { base };
            let v = r32(cb) as u64 | (r32(cb + 4) as u64) << 32;
            let c = etc1(v, xs, ys);
            [c[0], c[1], c[2], a]
        }
        f => {
            let m = morton8(lx, ly);
            let bpt = bits_per_texel(f);
            let bit = (tile * 64 + m) * bpt;
            let a = t.addr + bit / 8;
            match f {
                0 => {
                    let v = r32(a);
                    [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]
                }
                1 => [r8(a + 2) as u8, r8(a + 1) as u8, r8(a) as u8, 255],
                2 => {
                    let v = r16(a);
                    let c = |sh: u32| {
                        let x = (v >> sh) & 31;
                        (x << 3 | x >> 2) as u8
                    };
                    [c(11), c(6), c(1), if v & 1 != 0 { 255 } else { 0 }]
                }
                3 => {
                    let v = r16(a);
                    let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
                    [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8, 255]
                }
                4 => {
                    let v = r16(a);
                    let c = |sh: u32| (((v >> sh) & 15) * 17) as u8;
                    [c(12), c(8), c(4), c(0)]
                }
                5 => {
                    let (al, i) = (r8(a) as u8, r8(a + 1) as u8);
                    [i, i, i, al]
                }
                6 => [r8(a + 1) as u8, r8(a) as u8, 0, 255],
                7 => {
                    let i = r8(a) as u8;
                    [i, i, i, 255]
                }
                8 => [0, 0, 0, r8(a) as u8],
                9 => {
                    let b = r8(a);
                    let i = ((b >> 4) * 17) as u8;
                    [i, i, i, ((b & 15) * 17) as u8]
                }
                10 | 11 => {
                    let b = r8(a);
                    let n = if bit & 4 != 0 { b >> 4 } else { b & 15 };
                    let v = (n * 17) as u8;
                    if f == 10 {
                        [v, v, v, 255]
                    } else {
                        [0, 0, 0, v]
                    }
                }
                _ => [255, 0, 255, 255],
            }
        }
    }
}

/// Coordenada inteira com o modo de repetição; `None` = fora (borda).
fn wrap(mode: u32, v: i32, size: u32) -> Option<u32> {
    let n = size as i32;
    match mode & 3 {
        0 => Some(v.clamp(0, n - 1) as u32),
        1 => (0..n).contains(&v).then_some(v as u32),
        2 => Some(v.rem_euclid(n) as u32),
        _ => {
            let c = v.rem_euclid(2 * n);
            Some(if c >= n { 2 * n - 1 - c } else { c } as u32)
        }
    }
}

/// Texel pela coordenada inteira (s, t) com t = ⌊v·altura⌋: Citra `rasterizer.cpp` faz
/// `t = altura − 1 − wrap(t)` antes de ler a memória.
fn fetch(t: &Tex, s: i32, tt: i32) -> [u8; 4] {
    match (wrap(t.wrap_s, s, t.w), wrap(t.wrap_t, tt, t.h)) {
        (Some(x), Some(y)) => texel(t, x, t.h - 1 - y),
        _ => t.border,
    }
}

/// Amostra a textura em (u, v) normalizados. Filtro linear se a unidade pedir (bilinear como o GPU do Azahar);
/// senão o texel mais próximo (como o renderizador por software do Citra).
pub fn sample(t: &Tex, u: f32, v: f32) -> [u8; 4] {
    if t.w == 0 || t.h == 0 || t.addr == 0 {
        return [0, 0, 0, 255];
    }
    let fs = u * t.w as f32;
    let ft = v * t.h as f32;
    if !t.linear {
        return fetch(t, fs.floor() as i32, ft.floor() as i32);
    }
    let (x0, y0) = ((fs - 0.5).floor(), (ft - 0.5).floor());
    let (fx, fy) = (fs - 0.5 - x0, ft - 0.5 - y0);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let c00 = fetch(t, x0, y0);
    let c10 = fetch(t, x0 + 1, y0);
    let c01 = fetch(t, x0, y0 + 1);
    let c11 = fetch(t, x0 + 1, y0 + 1);
    let mut o = [0u8; 4];
    for k in 0..4 {
        let a = c00[k] as f32 * (1.0 - fx) + c10[k] as f32 * fx;
        let b = c01[k] as f32 * (1.0 - fx) + c11[k] as f32 * fx;
        o[k] = (a * (1.0 - fy) + b * fy + 0.5) as u8;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etc1_individual_mode() {
        // modo individual, cores base 0xF (255) e 0x0, tabela 0, índices 0 → +2 / 0+2
        let v: u64 = (0xf << 60) | (0xf << 52) | (0xf << 44);
        assert_eq!(etc1(v, 0, 0), [255, 255, 255]);
        assert_eq!(etc1(v, 3, 0), [2, 2, 2]);
    }

    #[test]
    fn wrap_modes() {
        assert_eq!(wrap(2, -1, 8), Some(7));
        assert_eq!(wrap(3, 8, 8), Some(7));
        assert_eq!(wrap(3, -1, 8), Some(0));
        assert_eq!(wrap(0, 99, 8), Some(7));
        assert_eq!(wrap(1, 8, 8), None);
    }
}
