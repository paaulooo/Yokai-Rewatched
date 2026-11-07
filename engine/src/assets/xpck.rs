//! `XPCK`: contêiner de arquivos (`.xa .xc .xv ...`). Porte de `tools/xpck.py`.

use super::{cstr, l5, u16le, Result};

pub struct Pack {
    pub files: Vec<(String, Vec<u8>)>,
}

impl Pack {
    pub fn parse(buf: &[u8]) -> Result<Pack> {
        if buf.get(0..4) != Some(b"XPCK".as_slice()) {
            return Err("não é um XPCK".into());
        }
        let count = (u16le(buf, 4)? & 0xfff) as usize;
        let info = u16le(buf, 6)? as usize * 4;
        let name_ofs = u16le(buf, 8)? as usize * 4;
        let data_ofs = u16le(buf, 10)? as usize * 4;
        let name_size = u16le(buf, 14)? as usize * 4;
        let names = l5::decompress(
            buf.get(name_ofs..name_ofs + name_size)
                .ok_or("XPCK: nomes truncados")?,
        )?;
        let mut files = Vec::with_capacity(count);
        for i in 0..count {
            let e = info + 12 * i;
            let name = cstr(&names, u16le(buf, e + 4)? as usize)?;
            let off_lo = u16le(buf, e + 6)? as usize;
            let size_lo = u16le(buf, e + 8)? as usize;
            let off_hi = *buf.get(e + 10).ok_or("XPCK: tabela truncada")? as usize;
            let size_hi = *buf.get(e + 11).ok_or("XPCK: tabela truncada")? as usize;
            let off = data_ofs + (((off_hi << 16) | off_lo) << 2);
            let size = (size_hi << 16) | size_lo;
            let data = buf
                .get(off..off + size)
                .ok_or_else(|| format!("XPCK: {name} truncado"))?;
            files.push((name, data.to_vec()));
        }
        Ok(Pack { files })
    }

    pub fn get(&self, name: &str) -> Result<&[u8]> {
        self.files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_slice())
            .ok_or_else(|| format!("XPCK: {name} ausente"))
    }
}
