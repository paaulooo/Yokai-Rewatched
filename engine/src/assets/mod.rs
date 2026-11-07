//! Leitores dos formatos do jogo, nativos em Rust (portados de `tools/*.py`, validados contra eles).

pub mod anmc;
pub mod arc0;
pub mod cfgbin;
pub mod chrc;
pub mod font;
pub mod imgc;
pub mod l5;
pub mod xmpr;
pub mod xpck;
pub mod xpvb;

pub type Result<T> = std::result::Result<T, String>;

pub(crate) fn u16le(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("leitura fora dos limites em 0x{o:x}"))
}

pub(crate) fn u32le(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("leitura fora dos limites em 0x{o:x}"))
}

pub(crate) fn f32le(d: &[u8], o: usize) -> Result<f32> {
    u32le(d, o).map(f32::from_bits)
}

pub(crate) fn i16s(d: &[u8]) -> impl Iterator<Item = i16> + '_ {
    d.as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
}

pub(crate) fn cstr(d: &[u8], o: usize) -> Result<String> {
    let tail = d
        .get(o..)
        .ok_or_else(|| format!("string fora dos limites em 0x{o:x}"))?;
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
}

/// CRC-32 (IEEE), usado pela Level-5 como hash de nomes.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc32_matches_level5_hashes() {
        assert_eq!(super::crc32(b"bg00"), 0x046c_3b6e);
        assert_eq!(super::crc32(b"main_bg"), 0x1883_2ffb);
        assert_eq!(super::crc32(b"0"), 0xf4db_df21);
    }
}
