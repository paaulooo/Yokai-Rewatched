//! Fonte da Level-5 (`fnt/ft_nrm.xf`, `fnt/ft_sml.xf`): XPCK com `000.xi` (folha de glyphs) e `FNT.bin` (`FNTC01`).
//!
//! `FNT.bin` (decifrado com os dados e conferido no leitor do jogo, `code.bin.c` ~linha 413187, que aloca
//! `u16@0x1e × 4`, `u16@0x22 × 8` e `u16@0x26 × 8` bytes):
//!   0x08 u16 nº de texturas · 0x0c u16 altura da linha (18 na normal) · 0x10 u16 ? (929; HIPÓTESE: caractere
//!   de reserva) · 0x1c/0x1e (offset em palavras, nº) tabela de tamanhos · 0x20/0x22 caracteres "grandes" ·
//!   0x24/0x26 caracteres "pequenos". Cada tabela é um bloco L5.
//!   Tamanho (4 B): i8 deslocamento x, i8 deslocamento y, u8 largura, u8 altura.
//!   Caractere (8 B): u16 código (UCS-2) · u16 (bits 0..9 índice de tamanho, 10..15 avanço) ·
//!   u32 (bits 0..3 canal da textura, 4..17 x, 18..31 y) — única divisão que cabe na textura 1024×504 nos
//!   8.000 caracteres; "A"/"g" desenhados de cada canal saem corretos.
//!   Os glyphs ficam em 3 folhas sobrepostas nos canais R (0), G (1) e B (2) da mesma textura.

use super::{imgc, l5, u16le, u32le, xpck::Pack, Result};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub struct Glyph {
    pub ox: i8,
    pub oy: i8,
    pub w: u8,
    pub h: u8,
    pub advance: u8,
    pub channel: u8,
    pub x: u16,
    pub y: u16,
}

pub struct Font {
    pub line_height: u16,
    glyphs: HashMap<u16, Glyph>,
    sheet: imgc::Image,
}

impl Font {
    pub fn parse(xf: &[u8]) -> Result<Font> {
        let pack = Pack::parse(xf)?;
        let d = pack.get("FNT.bin")?;
        if d.get(0..6) != Some(b"FNTC01".as_slice()) {
            return Err("FNT.bin não é FNTC01".into());
        }
        let table = |at: usize| -> Result<(Vec<u8>, usize)> {
            let off = u16le(d, at)? as usize * 4;
            let n = u16le(d, at + 2)? as usize;
            if n == 0 {
                return Ok((Vec::new(), 0));
            }
            Ok((
                l5::decompress(d.get(off..).ok_or("FNTC: offset inválido")?)?,
                n,
            ))
        };
        let (sizes, ns) = table(0x1c)?;
        let (chars, nc) = table(0x20)?;
        let mut glyphs = HashMap::with_capacity(nc);
        for i in 0..nc {
            let code = u16le(&chars, 8 * i)?;
            let info = u16le(&chars, 8 * i + 2)?;
            let img = u32le(&chars, 8 * i + 4)?;
            let si = (info & 0x3ff) as usize;
            if si >= ns {
                continue;
            }
            let s = &sizes[4 * si..4 * si + 4];
            glyphs.insert(
                code,
                Glyph {
                    ox: s[0] as i8,
                    oy: s[1] as i8,
                    w: s[2],
                    h: s[3],
                    advance: (info >> 10) as u8,
                    channel: (img & 0xf) as u8,
                    x: ((img >> 4) & 0x3fff) as u16,
                    y: (img >> 18) as u16,
                },
            );
        }
        Ok(Font {
            line_height: u16le(d, 0x0c)?,
            glyphs,
            sheet: imgc::decode(pack.get("000.xi")?)?,
        })
    }

    pub fn glyph(&self, c: char) -> Option<&Glyph> {
        let code = u16::try_from(c as u32).ok()?;
        self.glyphs.get(&code)
    }

    /// Largura de uma linha em pixels (escala 1).
    pub fn width(&self, line: &str) -> i32 {
        line.chars()
            .map(|c| self.glyph(c).map_or(0, |g| g.advance as i32))
            .sum()
    }

    /// Cobertura do glyph (0..255) no ponto (gx, gy) dentro dele.
    pub fn coverage(&self, g: &Glyph, gx: u32, gy: u32) -> u8 {
        let (x, y) = (g.x as usize + gx as usize, g.y as usize + gy as usize);
        if x >= self.sheet.width || y >= self.sheet.height {
            return 0;
        }
        let p = self.sheet.pixels[y * self.sheet.width + x];
        let shift = match g.channel {
            0 => 16,
            1 => 8,
            _ => 0,
        };
        (p >> shift) as u8
    }

    #[allow(clippy::too_many_arguments)]
    /// Desenha `text` (já sem marcações) com o canto superior esquerdo da linha em (x, y).
    pub fn draw(
        &self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        (x, y): (f32, f32),
        text: &str,
        rgb: u32,
        scale: f32,
        alpha: f32,
    ) {
        let mut pen_y = y;
        for line in text.split('\n') {
            let mut pen_x = x;
            for c in line.chars() {
                let Some(g) = self.glyph(c) else { continue };
                let (gw, gh) = ((g.w as f32 * scale) as i32, (g.h as f32 * scale) as i32);
                let (bx, by) = (pen_x + g.ox as f32 * scale, pen_y + g.oy as f32 * scale);
                for dy in 0..gh {
                    let py = by as i32 + dy;
                    if py < 0 || py >= height as i32 {
                        continue;
                    }
                    for dx in 0..gw {
                        let px = bx as i32 + dx;
                        if px < 0 || px >= width as i32 {
                            continue;
                        }
                        let cov = self.coverage(
                            g,
                            (dx as f32 / scale) as u32,
                            (dy as f32 / scale) as u32,
                        );
                        let a = (cov as f32 * alpha) as u32;
                        if a == 0 {
                            continue;
                        }
                        let dst = &mut pixels[py as usize * width + px as usize];
                        let mix = |s: u32| {
                            let (sc, dc) = ((rgb >> s) & 0xff, (*dst >> s) & 0xff);
                            ((sc * a + dc * (255 - a)) / 255) << s
                        };
                        *dst = mix(16) | mix(8) | mix(0);
                    }
                }
                pen_x += g.advance as f32 * scale;
            }
            pen_y += self.line_height as f32 * scale;
        }
    }
}

/// Texto dos menus: remove marcações `<TAG>`/`</C>` e converte `\n` literal em quebra de linha.
pub fn clean(text: &str) -> String {
    let t = text.replace("\\n", "\n");
    let mut out = String::with_capacity(t.len());
    let mut in_tag = false;
    for c in t.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Desenha uma frase com a fonte real e grava RGBA cru em work/font/ (conferir visualmente).
    #[test]
    fn font_loads_and_renders() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        let f = Font::parse(&fa.read("fnt/ft_nrm.xf").unwrap()).unwrap();
        assert_eq!(f.line_height, 18);
        assert!(f.glyph('A').is_some() && f.glyph('ケ').is_some());
        assert_eq!(f.glyph(' ').unwrap().advance, 5);
        let (w, h) = (320usize, 48usize);
        let mut px = vec![0xffff_ffffu32; w * h];
        f.draw(
            &mut px,
            w,
            h,
            (4.0, 4.0),
            "Start a new file.\nYo-kai Watch ケータ",
            0x202020,
            1.0,
            1.0,
        );
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/font");
        if std::fs::create_dir_all(&dir).is_ok() {
            let raw: Vec<u8> = px
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            let _ = std::fs::write(dir.join("rust_text_320x48.rgba"), raw);
        }
    }

    #[test]
    fn clean_strips_tags() {
        assert_eq!(clean("a<CR>b</C>\\nc <SAVE_SLOT>."), "ab\nc .");
    }
}
