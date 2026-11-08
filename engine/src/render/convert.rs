//! Conversão rápida entre uma superfície do PICA na memória guest (blocos 8×8 em ordem de Morton, linhas de baixo
//! para cima: pixel (x, y) da imagem fica em `tiled_offset(x, altura − 1 − y)`) e a imagem linear da GPU (cor RGBA8;
//! profundidade f32 = zi·2⁻²⁴ seguida do estêncil u8 por pixel).
//!
//! Mesmo resultado bit a bit que `hle::gpu::{read_pixel, write_pixel}` pixel a pixel (testado abaixo), mas por bloco,
//! com a tabela de Morton pronta e uma linha de blocos por tarefa em paralelo. Antes a conversão pixel a pixel da
//! devolução custava até ~20% do tempo do host no quarto.

use rayon::prelude::*;

/// Para cada índice de Morton 0–63 do bloco: (x, y) dentro do bloco.
const INV_MORTON: [(u32, u32); 64] = {
    let mut t = [(0u32, 0u32); 64];
    let mut m = 0;
    while m < 64 {
        let mut x = 0;
        let mut y = 0;
        let mut i = 0;
        while i < 3 {
            x |= ((m >> (2 * i)) & 1) << i;
            y |= ((m >> (2 * i + 1)) & 1) << i;
            i += 1;
        }
        t[m as usize] = (x, y);
        m += 1;
    }
    t
};

/// Bytes → RGBA8 no formato do GSP (0 RGBA8, 1 RGB8, 2 RGB565, 3 RGB5A1, 4 RGBA4) — igual a `read_pixel`.
#[inline(always)]
pub fn decode(fmt: u32, b: &[u8]) -> [u8; 4] {
    match fmt & 7 {
        0 => [b[3], b[2], b[1], b[0]],
        1 => [b[2], b[1], b[0], 255],
        2 => {
            let v = u16::from_le_bytes([b[0], b[1]]) as u32;
            let (r, g, bl) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (bl << 3 | bl >> 2) as u8, 255]
        }
        3 => {
            let v = u16::from_le_bytes([b[0], b[1]]) as u32;
            let c = |s: u32| {
                let x = (v >> s) & 31;
                (x << 3 | x >> 2) as u8
            };
            [c(11), c(6), c(1), if v & 1 != 0 { 255 } else { 0 }]
        }
        _ => {
            let v = u16::from_le_bytes([b[0], b[1]]) as u32;
            let c = |s: u32| (((v >> s) & 15) * 17) as u8;
            [c(12), c(8), c(4), c(0)]
        }
    }
}

/// RGBA8 → bytes no formato do GSP — igual a `write_pixel`.
#[inline(always)]
pub fn encode(fmt: u32, p: [u8; 4], out: &mut [u8]) {
    let [r, g, b, a] = p.map(|x| x as u32);
    match fmt & 7 {
        0 => out[..4].copy_from_slice(&(r << 24 | g << 16 | b << 8 | a).to_le_bytes()),
        1 => {
            out[0] = b as u8;
            out[1] = g as u8;
            out[2] = r as u8;
        }
        2 => out[..2].copy_from_slice(&(((r >> 3) << 11 | (g >> 2) << 5 | (b >> 3)) as u16).to_le_bytes()),
        3 => out[..2].copy_from_slice(&(((r >> 3) << 11 | (g >> 3) << 6 | (b >> 3) << 1 | (a >> 7)) as u16).to_le_bytes()),
        _ => out[..2].copy_from_slice(&(((r >> 4) << 12 | (g >> 4) << 8 | (b >> 4) << 4 | (a >> 4)) as u16).to_le_bytes()),
    }
}

fn ok(w: u32, h: u32) -> bool {
    w > 0 && h > 0 && w % 8 == 0 && h % 8 == 0
}

/// Percorre a superfície por linha de blocos da MEMÓRIA (em paralelo): `f(linha da memória, bytes dessa linha de
/// blocos, linha de blocos da imagem)`. Linha de blocos da memória `ty` = linhas de imagem `h−8−8·ty .. h−8·ty`.
fn rows_mut<'a>(mem: &'a mut [u8], w: u32, bpp: u32) -> impl ParallelIterator<Item = (usize, &'a mut [u8])> {
    mem.par_chunks_mut((w * 8 * bpp) as usize).enumerate()
}

/// Cor: memória (formato do GSP `fmt`) → imagem RGBA8 linear (`out` = 4·w·h). Falso se o tamanho não for múltiplo de 8.
pub fn color_to_linear(mem: &[u8], w: u32, h: u32, fmt: u32, out: &mut [u8]) -> bool {
    let bpp = crate::hle::gpu::bpp(fmt);
    if !ok(w, h) || mem.len() < (w * h * bpp) as usize || out.len() < (w * h * 4) as usize {
        return false;
    }
    let tiles = h / 8;
    out[..(w * h * 4) as usize].par_chunks_mut((w * 8 * 4) as usize).enumerate().for_each(|(c, rows)| {
        // linhas de imagem 8c..8c+8 = linha de blocos da memória ty = tiles − 1 − c
        let ty = tiles - 1 - c as u32;
        let base = (ty * w * 8 * bpp) as usize;
        for tx in 0..w / 8 {
            let tb = base + (tx * 64 * bpp) as usize;
            for (m, &(ix, iy)) in INV_MORTON.iter().enumerate() {
                let src = &mem[tb + m * bpp as usize..];
                // y da memória dentro do bloco = iy → linha da imagem dentro das 8 = 7 − iy
                let o = (((7 - iy) * w + tx * 8 + ix) * 4) as usize;
                rows[o..o + 4].copy_from_slice(&decode(fmt, src));
            }
        }
    });
    true
}

/// Cor: imagem RGBA8 linear → memória (formato do GSP `fmt`).
pub fn color_from_linear(src: &[u8], w: u32, h: u32, fmt: u32, mem: &mut [u8]) -> bool {
    let bpp = crate::hle::gpu::bpp(fmt);
    if !ok(w, h) || mem.len() < (w * h * bpp) as usize || src.len() < (w * h * 4) as usize {
        return false;
    }
    let tiles = h / 8;
    rows_mut(&mut mem[..(w * h * bpp) as usize], w, bpp).for_each(|(ty, row)| {
        let c = tiles - 1 - ty as u32;
        for tx in 0..w / 8 {
            let tb = (tx * 64 * bpp) as usize;
            for (m, &(ix, iy)) in INV_MORTON.iter().enumerate() {
                let y = 8 * c + 7 - iy;
                let i = ((y * w + tx * 8 + ix) * 4) as usize;
                let p = [src[i], src[i + 1], src[i + 2], src[i + 3]];
                encode(fmt, p, &mut row[tb + m * bpp as usize..]);
            }
        }
    });
    true
}

/// Profundidade: memória (2, 3 ou 4 bytes; formato 3 = D24S8) → `out` = f32 (4·n) seguido do estêncil (n).
pub fn depth_to_linear(mem: &[u8], w: u32, h: u32, bpp: u32, fmt: u32, out: &mut [u8]) -> bool {
    let n = (w * h) as usize;
    if !ok(w, h) || mem.len() < n * bpp as usize || out.len() < 5 * n {
        return false;
    }
    let tiles = h / 8;
    let (zs, st) = out[..5 * n].split_at_mut(4 * n);
    let row_px = (w * 8) as usize;
    zs.par_chunks_mut(row_px * 4).zip(st.par_chunks_mut(row_px)).enumerate().for_each(|(c, (zrow, srow))| {
        let ty = tiles - 1 - c as u32;
        let base = (ty * w * 8 * bpp) as usize;
        for tx in 0..w / 8 {
            let tb = base + (tx * 64 * bpp) as usize;
            for (m, &(ix, iy)) in INV_MORTON.iter().enumerate() {
                let a = &mem[tb + m * bpp as usize..];
                let (zi, s) = match bpp {
                    2 => (u16::from_le_bytes([a[0], a[1]]) as u32, 0),
                    3 => (a[0] as u32 | (a[1] as u32) << 8 | (a[2] as u32) << 16, 0),
                    _ => {
                        let v = u32::from_le_bytes([a[0], a[1], a[2], a[3]]);
                        (v & 0xff_ffff, if fmt == 3 { v >> 24 } else { 0 })
                    }
                };
                let i = ((7 - iy) * w + tx * 8 + ix) as usize;
                let f = zi as f32 * (1.0 / 16_777_216.0);
                zrow[4 * i..4 * i + 4].copy_from_slice(&f.to_le_bytes());
                srow[i] = s as u8;
            }
        }
    });
    true
}

/// Profundidade: f32 + estêncil → memória. Em D24 sem estêncil (formato ≠ 3, 4 bytes) o byte alto fica como estava.
pub fn depth_from_linear(src: &[u8], w: u32, h: u32, bpp: u32, fmt: u32, mem: &mut [u8]) -> bool {
    let n = (w * h) as usize;
    if !ok(w, h) || mem.len() < n * bpp as usize || src.len() < 5 * n {
        return false;
    }
    let tiles = h / 8;
    rows_mut(&mut mem[..n * bpp as usize], w, bpp).for_each(|(ty, row)| {
        let c = tiles - 1 - ty as u32;
        for tx in 0..w / 8 {
            let tb = (tx * 64 * bpp) as usize;
            for (m, &(ix, iy)) in INV_MORTON.iter().enumerate() {
                let i = ((8 * c + 7 - iy) * w + tx * 8 + ix) as usize;
                let f = f32::from_le_bytes([src[4 * i], src[4 * i + 1], src[4 * i + 2], src[4 * i + 3]]);
                let zi = ((f * 16_777_216.0).round() as u32).min(0xff_ffff);
                let a = &mut row[tb + m * bpp as usize..];
                match bpp {
                    2 => a[..2].copy_from_slice(&(zi.min(0xffff) as u16).to_le_bytes()),
                    3 => a[..3].copy_from_slice(&zi.to_le_bytes()[..3]),
                    _ => {
                        a[..3].copy_from_slice(&zi.to_le_bytes()[..3]);
                        if fmt == 3 {
                            a[3] = src[4 * n + i];
                        }
                    }
                }
            }
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::gpu::{morton8, tiled_offset};

    #[test]
    fn inverse_morton_matches() {
        for (m, &(x, y)) in INV_MORTON.iter().enumerate() {
            assert_eq!(morton8(x, y), m as u32);
        }
    }

    /// Mesma posição que o caminho antigo (`tiled_offset(x, h − 1 − y)`) e mesma codificação ida e volta.
    #[test]
    fn color_roundtrip_matches_tiled_offset() {
        let (w, h) = (16u32, 24u32);
        for fmt in 0..5u32 {
            let bpp = crate::hle::gpu::bpp(fmt);
            let mut mem = vec![0u8; (w * h * bpp) as usize];
            for (i, b) in mem.iter_mut().enumerate() {
                *b = (i * 37 + 11) as u8;
            }
            let mut img = vec![0u8; (w * h * 4) as usize];
            assert!(color_to_linear(&mem, w, h, fmt, &mut img));
            for y in 0..h {
                for x in 0..w {
                    let a = tiled_offset(x, h - 1 - y, w, bpp) as usize;
                    let i = ((y * w + x) * 4) as usize;
                    assert_eq!(&img[i..i + 4], &decode(fmt, &mem[a..]), "fmt {fmt} ({x},{y})");
                }
            }
            let mut back = vec![0u8; mem.len()];
            assert!(color_from_linear(&img, w, h, fmt, &mut back));
            // decodificar de novo dá a mesma imagem (formatos com perda: decode∘encode∘decode = decode)
            let mut img2 = vec![0u8; img.len()];
            color_to_linear(&back, w, h, fmt, &mut img2);
            assert_eq!(img, img2, "fmt {fmt}");
        }
    }

    #[test]
    fn depth_roundtrip() {
        let (w, h) = (8u32, 16u32);
        for (bpp, fmt) in [(2u32, 0u32), (3, 2), (4, 3)] {
            let mut mem = vec![0u8; (w * h * bpp) as usize];
            for (i, b) in mem.iter_mut().enumerate() {
                *b = (i * 53 + 7) as u8;
            }
            let mut img = vec![0u8; (w * h * 5) as usize];
            assert!(depth_to_linear(&mem, w, h, bpp, fmt, &mut img));
            let mut back = vec![0u8; mem.len()];
            assert!(depth_from_linear(&img, w, h, bpp, fmt, &mut back));
            assert_eq!(mem, back, "bpp {bpp}");
        }
    }
}
