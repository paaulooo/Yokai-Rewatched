//! `IMGC` (`.xi`): textura em tiles 8×8 (ordem Morton) deduplicados por uma tabela de tiles.
//! Porte de `tools/imgc.py`/`tools/etc1.py` (1.233 `.xi` do jogo decodificados sem erro com o original).

use super::{i16s, l5, u16le, u32le, Result};

pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Pixels ARGB (`a << 24 | r << 16 | g << 8 | b`), linha 0 em cima.
    pub pixels: Vec<u32>,
}

fn argb(r: u32, g: u32, b: u32, a: u32) -> u32 {
    (a << 24) | (r << 16) | (g << 8) | b
}

pub fn decode(buf: &[u8]) -> Result<Image> {
    if buf.get(0..4) != Some(b"IMGC".as_slice()) {
        return Err("não é um IMGC".into());
    }
    let fmt = buf[10];
    let bd = buf[13] as usize;
    let w = u16le(buf, 16)? as usize;
    let h = u16le(buf, 18)? as usize;
    let info = u16le(buf, 24)? as usize;
    let dofs = u32le(buf, 28)? as usize;
    let (t0, ts) = (u32le(buf, info)? as usize, u32le(buf, info + 4)? as usize);
    let (d0, ds) = (
        u32le(buf, info + 8)? as usize,
        u32le(buf, info + 12)? as usize,
    );
    let slice = |a: usize, n: usize| {
        buf.get(dofs + a..dofs + a + n)
            .ok_or("IMGC: dados truncados")
    };
    let table = l5::decompress(slice(t0, ts)?)?;
    let data = l5::decompress(slice(d0, ds)?)?;
    if u16le(&table, 0)? == 0x453 {
        return Err("IMGC: tabela de tiles antiga não suportada".into());
    }
    let tile_bytes = 64 * bd / 8;
    let (pw, ph) = ((w + 7) & !7, (h + 7) & !7);
    let nx = pw / 8;
    // Tile ausente (índice negativo ou fora dos dados) = bytes zero, como em tools/imgc.py:
    // nos formatos com alfa isso é transparente.
    let empty = vec![0u8; tile_bytes];
    let tiles: Vec<&[u8]> = i16s(&table)
        .take(nx * (ph / 8))
        .map(|e| {
            usize::try_from(e)
                .ok()
                .and_then(|e| data.get(e * tile_bytes..(e + 1) * tile_bytes))
                .unwrap_or(&empty)
        })
        .collect();

    let mut img = vec![argb(0, 0, 0, 255); pw * ph];
    let etc = matches!(fmt, 0x1b..=0x1d) && (bd == 4 || bd == 8);
    for (t, tile) in tiles.iter().enumerate() {
        let (ty, tx) = (t / nx, t % nx);
        if etc {
            let bs = if bd == 4 { 8 } else { 16 };
            for sb in 0..4 {
                let (bx, by) = (sb >> 1, sb & 1);
                let raw = &tile[sb * bs..(sb + 1) * bs];
                let (alpha, color) = if bd == 8 {
                    (Some(&raw[..8]), &raw[8..16])
                } else {
                    (None, &raw[..8])
                };
                let v = u64::from_le_bytes(color.try_into().unwrap());
                let block = etc1_block(v);
                let (y0, x0) = (ty * 8 + by * 4, tx * 8 + bx * 4);
                for a in 0..4 {
                    for b in 0..4 {
                        let [r, g, bl] = block[b][a];
                        let al = alpha.map_or(255, |al| {
                            ((al[(a * 4 + b) / 2] >> (4 * ((a * 4 + b) & 1))) & 15) as u32 * 17
                        });
                        img[(y0 + a) * pw + x0 + b] = argb(r as u32, g as u32, bl as u32, al);
                    }
                }
            }
            continue;
        }
        for p in 0..64 {
            let even = (p & 1) | ((p >> 1) & 2) | ((p >> 2) & 4);
            let odd = ((p >> 1) & 1) | ((p >> 2) & 2) | ((p >> 3) & 4);
            let (x, y) = (odd, even);
            img[(ty * 8 + y) * pw + tx * 8 + x] = pixel(tile, p, fmt, bd);
        }
    }
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        pixels.extend_from_slice(&img[y * pw..y * pw + w]);
    }
    Ok(Image {
        width: w,
        height: h,
        pixels,
    })
}

fn pixel(tile: &[u8], p: usize, fmt: u8, bd: usize) -> u32 {
    match bd {
        32 => {
            let a = &tile[p * 4..p * 4 + 4];
            argb(a[3] as u32, a[2] as u32, a[1] as u32, a[0] as u32)
        }
        24 => {
            let a = &tile[p * 3..p * 3 + 3];
            argb(a[2] as u32, a[1] as u32, a[0] as u32, 255)
        }
        16 => {
            let v = tile[p * 2] as u32 | (tile[p * 2 + 1] as u32) << 8;
            match fmt {
                1 => argb(
                    ((v >> 12) & 15) * 17,
                    ((v >> 8) & 15) * 17,
                    ((v >> 4) & 15) * 17,
                    (v & 15) * 17,
                ),
                2 => argb(
                    ((v >> 11) & 31) * 255 / 31,
                    ((v >> 6) & 31) * 255 / 31,
                    ((v >> 1) & 31) * 255 / 31,
                    (v & 1) * 255,
                ),
                4 => argb(
                    ((v >> 11) & 31) * 255 / 31,
                    ((v >> 5) & 63) * 255 / 63,
                    (v & 31) * 255 / 31,
                    255,
                ),
                _ => {
                    let (l, a) = (tile[p * 2 + 1] as u32, tile[p * 2] as u32);
                    argb(l, l, l, a)
                }
            }
        }
        8 => {
            let a = tile[p] as u32;
            match fmt {
                0x0b | 0x0c => {
                    let (hi, lo) = ((a >> 4) * 17, (a & 15) * 17);
                    argb(hi, hi, hi, lo)
                }
                0x0e | 0x0f => argb(255, 255, 255, a),
                _ => argb(a, a, a, 255),
            }
        }
        4 => {
            let n = ((tile[p / 2] >> (4 * (p & 1))) & 15) as u32 * 17;
            if matches!(fmt, 0x0f | 0x10) {
                argb(255, 255, 255, n)
            } else {
                argb(n, n, n, 255)
            }
        }
        _ => argb(255, 0, 255, 255),
    }
}

const ETC1_MOD: [[i32; 4]; 8] = [
    [2, 8, -2, -8],
    [5, 17, -5, -17],
    [9, 29, -9, -29],
    [13, 42, -13, -42],
    [18, 60, -18, -60],
    [24, 80, -24, -80],
    [33, 106, -33, -106],
    [47, 183, -47, -183],
];

/// Bloco ETC1 4×4 indexado `[y][x]` (mesma convenção de `tools/etc1.py`).
fn etc1_block(v: u64) -> [[[u8; 3]; 4]; 4] {
    let bits = |s: u32, n: u32| ((v >> s) & ((1 << n) - 1)) as i32;
    let (diff, flip) = (bits(33, 1), bits(32, 1));
    let (c1, c2): ([i32; 3], [i32; 3]) = if diff != 0 {
        let base = [bits(59, 5), bits(51, 5), bits(43, 5)];
        let delta = [bits(56, 3), bits(48, 3), bits(40, 3)].map(|d| d - 8 * (d >> 2));
        let ext = |x: i32| (x << 3) | (x >> 2);
        (
            base.map(ext),
            [0, 1, 2].map(|k| ext((base[k] + delta[k]) & 31)),
        )
    } else {
        (
            [bits(60, 4), bits(52, 4), bits(44, 4)].map(|x| x * 17),
            [bits(56, 4), bits(48, 4), bits(40, 4)].map(|x| x * 17),
        )
    };
    let (cw1, cw2) = (bits(37, 3) as usize, bits(34, 3) as usize);
    let mut px = [[[0u8; 3]; 4]; 4];
    for (y, row) in px.iter_mut().enumerate() {
        for (x, out) in row.iter_mut().enumerate() {
            let i = (x * 4 + y) as u32;
            let first = if flip == 0 { x < 2 } else { y < 2 };
            let (c, table) = if first {
                (c1, ETC1_MOD[cw1])
            } else {
                (c2, ETC1_MOD[cw2])
            };
            let m = table[((bits(16 + i, 1) << 1) | bits(i, 1)) as usize];
            *out = c.map(|k| (k + m).clamp(0, 255) as u8);
        }
    }
    px
}

#[cfg(test)]
mod tests {
    /// Compara com referências geradas por tools/imgc.py (`work/title/etcref_*.rgba` + `.txt`).
    /// Manual: `cargo test --release -- --ignored etc1_matches_python`.
    #[test]
    #[ignore]
    fn etc1_matches_python() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let fa = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/title");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if !p
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("etcref_")
                || p.extension().unwrap() != "txt"
            {
                continue;
            }
            let meta = std::fs::read_to_string(&p).unwrap();
            let name = meta.split_whitespace().next().unwrap();
            let reference = std::fs::read(p.with_extension("rgba")).unwrap();
            let img = super::decode(&fa.read(name).unwrap()).unwrap();
            let ours: Vec<u8> = img
                .pixels
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, (p >> 24) as u8])
                .collect();
            let diff = ours
                .chunks(4)
                .zip(reference.chunks(4))
                .filter(|(a, b)| a != b)
                .count();
            println!(
                "{name}: {}x{}, {diff} pixels diferentes",
                img.width, img.height
            );
            assert_eq!(diff, 0);
            checked += 1;
        }
        assert!(checked > 0, "nenhuma referência ETC1 em work/title");
    }
}
