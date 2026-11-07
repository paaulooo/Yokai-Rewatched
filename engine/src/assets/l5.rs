//! Compressão Level-5: cabeçalho u32 = `tamanho << 3 | método`
//! (0 cru, 1 LZ10, 2 Huffman 4 bits, 3 Huffman 8 bits, 4 RLE). Porte de `tools/l5.py`.

use super::Result;

pub fn decompress(buf: &[u8]) -> Result<Vec<u8>> {
    let h = super::u32le(buf, 0)?;
    let size = (h >> 3) as usize;
    let body = &buf[4..];
    match h & 7 {
        0 => body
            .get(..size)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| "L5: bloco cru truncado".to_string()),
        1 => lz10(body, size),
        2 => huffman(body, size, 4),
        3 => huffman(body, size, 8),
        4 => rle(body, size),
        m => Err(format!("L5: método de compressão {m} desconhecido")),
    }
}

fn byte(d: &[u8], i: usize) -> Result<u8> {
    d.get(i)
        .copied()
        .ok_or_else(|| "L5: dados comprimidos truncados".to_string())
}

fn lz10(d: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    let mut i = 0;
    while out.len() < size {
        let flags = byte(d, i)?;
        i += 1;
        for b in 0..8 {
            if out.len() >= size {
                break;
            }
            if flags & (0x80 >> b) != 0 {
                let (x, y) = (byte(d, i)? as usize, byte(d, i + 1)? as usize);
                i += 2;
                let len = (x >> 4) + 3;
                let disp = (((x & 15) << 8) | y) + 1;
                if disp > out.len() {
                    return Err("L5/LZ10: referência antes do início".into());
                }
                for _ in 0..len {
                    out.push(out[out.len() - disp]);
                }
            } else {
                out.push(byte(d, i)?);
                i += 1;
            }
        }
    }
    out.truncate(size);
    Ok(out)
}

fn rle(d: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    let mut i = 0;
    while out.len() < size {
        let f = byte(d, i)?;
        i += 1;
        if f & 0x80 != 0 {
            let v = byte(d, i)?;
            i += 1;
            out.extend(std::iter::repeat_n(v, (f & 0x7f) as usize + 3));
        } else {
            let n = (f & 0x7f) as usize + 1;
            out.extend_from_slice(d.get(i..i + n).ok_or("L5/RLE: truncado")?);
            i += n;
        }
    }
    out.truncate(size);
    Ok(out)
}

fn huffman(d: &[u8], size: usize, bits: u32) -> Result<Vec<u8>> {
    let tree_size = (byte(d, 0)? as usize + 1) * 2;
    let tree = d.get(..tree_size).ok_or("L5/Huffman: árvore truncada")?;
    let stream = &d[tree_size..];
    let mut words = stream
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
    let (mut word, mut left) = (0u32, 0u32);
    let mut out = Vec::with_capacity(size);
    let mut low_nibble: Option<u8> = None;
    while out.len() < size {
        let mut cur = 1usize;
        let value = loop {
            if left == 0 {
                word = words.next().ok_or("L5/Huffman: fim dos dados")?;
                left = 32;
            }
            left -= 1;
            let bit = (word >> left) & 1;
            let node = *tree.get(cur).ok_or("L5/Huffman: nó fora da árvore")?;
            let next = (cur & !1) + (node as usize & 0x3f) * 2 + 2;
            let (leaf, child) = if bit == 0 {
                (node & 0x80, next)
            } else {
                (node & 0x40, next + 1)
            };
            if leaf != 0 {
                break *tree.get(child).ok_or("L5/Huffman: folha fora da árvore")?;
            }
            cur = child;
        };
        if bits == 8 {
            out.push(value);
        } else if let Some(lo) = low_nibble.take() {
            out.push(lo | (value << 4));
        } else {
            low_nibble = Some(value);
        }
    }
    out.truncate(size);
    Ok(out)
}
