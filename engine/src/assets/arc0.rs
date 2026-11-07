//! `yw1_a.fa` (ARC0): índice de diretórios/arquivos comprimido em L5; os arquivos ficam crus.
//! Porte de `tools/fa_list.py` (20.835 arquivos no RomFS do jogo).

use super::{cstr, l5, u16le, u32le, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct Archive {
    path: PathBuf,
    files: HashMap<String, (u64, u32)>,
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive> {
        let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut hd = [0u8; 72];
        f.read_exact(&mut hd).map_err(|e| e.to_string())?;
        if &hd[0..4] != b"ARC0" {
            return Err(format!("{}: não é um ARC0", path.display()));
        }
        let w = |i: usize| u32le(&hd, 4 + i * 4);
        let (dir_ofs, hash_ofs, ent_ofs, name_ofs, data_ofs) = (w(0)?, w(1)?, w(2)?, w(3)?, w(4)?);
        let dir_count = u16le(&hd, 24)? as usize;
        let ent_count = u32le(&hd, 28)? as usize;
        let mut seg = |a: u32, b: u32| -> Result<Vec<u8>> {
            let mut raw = vec![0u8; (b - a) as usize];
            f.seek(SeekFrom::Start(a as u64))
                .map_err(|e| e.to_string())?;
            f.read_exact(&mut raw).map_err(|e| e.to_string())?;
            l5::decompress(&raw)
        };
        let dirs = seg(dir_ofs, hash_ofs)?;
        let ents = seg(ent_ofs, name_ofs)?;
        let names = seg(name_ofs, data_ofs)?;

        let mut files = HashMap::with_capacity(ent_count);
        for d in 0..dir_count {
            let o = d * 20;
            let first = u16le(&dirs, o + 8)? as usize;
            let count = u16le(&dirs, o + 10)? as usize;
            let file_names = u32le(&dirs, o + 12)? as usize;
            let dir_name = cstr(&names, u32le(&dirs, o + 16)? as usize)?;
            for e in first..first + count {
                let eo = e * 16;
                let name = cstr(&names, file_names + u32le(&ents, eo + 4)? as usize)?;
                let offset = data_ofs as u64 + u32le(&ents, eo + 8)? as u64;
                files.insert(
                    format!("{dir_name}{name}"),
                    (offset, u32le(&ents, eo + 12)?),
                );
            }
        }
        Ok(Archive {
            path: path.to_path_buf(),
            files,
        })
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let &(offset, size) = self
            .files
            .get(name)
            .ok_or_else(|| format!("{name}: não está no .fa"))?;
        let mut f = File::open(&self.path).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        let mut data = vec![0u8; size as usize];
        f.read_exact(&mut data)
            .map_err(|e| format!("{name}: {e}"))?;
        Ok(data)
    }
}
