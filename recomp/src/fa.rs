//! Acesso a um arquivo do `yw1_a.fa` (ARC0) do usuário, para o gerador extrair os shaders (`fix.xr`).
//!
//! Reaproveita os leitores do motor por `#[path]` (`engine/src/assets/l5.rs` e `xpck.rs`, que só dependem dos
//! auxiliares abaixo), sem o gerador depender do crate `yw1` inteiro. O índice do ARC0 é o mesmo de
//! `engine/src/assets/arc0.rs`, lido aqui de um `File` com deslocamento base para servir também ao `.fa` dentro do
//! RomFS do `.3ds`.
//!
//! Onde procura o `.fa`: `YW_FA` → `BASE/romfs/yw1_a.fa` → `BASE/romfs/0004000000167700/yw1_a.fa` → dentro do `.3ds`
//! (`YW_GAME` ou `BASE/Yo-Kai Watch (USA).3ds`, NCSD → NCCH → IVFC nível 3, como `hle/services/fs.rs`).
//! BASE = a pasta acima de `yokai-port/`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, String>;

#[path = "../../engine/src/assets/l5.rs"]
pub mod l5;
#[path = "../../engine/src/assets/xpck.rs"]
#[allow(dead_code)]
pub mod xpck;

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

pub(crate) fn cstr(d: &[u8], o: usize) -> Result<String> {
    let tail = d.get(o..).ok_or_else(|| format!("string fora dos limites em 0x{o:x}"))?;
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    Ok(String::from_utf8_lossy(&tail[..end]).into_owned())
}

fn read_at(f: &mut File, at: u64, n: usize) -> Result<Vec<u8>> {
    let mut b = vec![0u8; n];
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    f.read_exact(&mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

/// Lê `want` (caminho completo dentro do ARC0, ex. `fix.xr`) de um `.fa` que começa em `base` no arquivo `f`.
fn fa_read(f: &mut File, base: u64, want: &str) -> Result<Vec<u8>> {
    let hd = read_at(f, base, 72)?;
    if &hd[0..4] != b"ARC0" {
        return Err("não é um ARC0".into());
    }
    let w = |i: usize| u32le(&hd, 4 + i * 4);
    let (dir_ofs, hash_ofs, ent_ofs, name_ofs, data_ofs) = (w(0)?, w(1)?, w(2)?, w(3)?, w(4)?);
    let dir_count = u16le(&hd, 24)? as usize;
    let mut seg = |a: u32, b: u32| -> Result<Vec<u8>> {
        let raw = read_at(f, base + a as u64, b.checked_sub(a).ok_or("ARC0: segmento invertido")? as usize)?;
        l5::decompress(&raw)
    };
    let dirs = seg(dir_ofs, hash_ofs)?;
    let ents = seg(ent_ofs, name_ofs)?;
    let names = seg(name_ofs, data_ofs)?;
    for d in 0..dir_count {
        let o = d * 20;
        let first = u16le(&dirs, o + 8)? as usize;
        let count = u16le(&dirs, o + 10)? as usize;
        let file_names = u32le(&dirs, o + 12)? as usize;
        let dir_name = cstr(&names, u32le(&dirs, o + 16)? as usize)?;
        if !want.starts_with(&dir_name) {
            continue;
        }
        for e in first..first + count {
            let eo = e * 16;
            let name = cstr(&names, file_names + u32le(&ents, eo + 4)? as usize)?;
            if format!("{dir_name}{name}") == want {
                let offset = data_ofs as u64 + u32le(&ents, eo + 8)? as u64;
                return read_at(f, base + offset, u32le(&ents, eo + 12)? as usize);
            }
        }
    }
    Err(format!("{want}: não está no .fa"))
}

/// Deslocamento do `yw1_a.fa` dentro do `.3ds` (arquivo na raiz do RomFS).
fn fa_in_3ds(f: &mut File) -> Result<u64> {
    let h = read_at(f, 0, 0x200)?;
    if &h[0x100..0x104] != b"NCSD" {
        return Err("não é NCSD (.3ds)".into());
    }
    let part0 = u32le(&h, 0x120)? as u64 * 0x200;
    let h = read_at(f, part0, 0x200)?;
    if &h[0x100..0x104] != b"NCCH" {
        return Err("partição 0 não é NCCH".into());
    }
    let romfs = part0 + u32le(&h, 0x1b0)? as u64 * 0x200;
    let iv = read_at(f, romfs, 0x60)?;
    if &iv[0..4] != b"IVFC" {
        return Err("RomFS sem IVFC (criptografado?)".into());
    }
    let master = u32le(&iv, 0x08)? as u64;
    let l3_block = 1u64 << u32le(&iv, 0x4c)?;
    let l3 = romfs + (0x60 + master).div_ceil(l3_block) * l3_block;
    // nível 3: tabela de diretórios (raiz em 0: 1º arquivo em +0x0C) e de arquivos (irmão +0x04, dados +0x08,
    // tamanho do nome +0x1C, nome UTF-16 em +0x20)
    let lh = read_at(f, l3, 0x28)?;
    let (dmeta, fmeta, fdata) = (u32le(&lh, 0x0c)? as u64, u32le(&lh, 0x1c)? as u64, u32le(&lh, 0x24)? as u64);
    let flen = u32le(&lh, 0x20)? as usize;
    let root = read_at(f, l3 + dmeta, 0x18)?;
    let files = read_at(f, l3 + fmeta, flen)?;
    let want: Vec<u16> = "yw1_a.fa".encode_utf16().collect();
    let mut at = u32le(&root, 0x0c)? as usize;
    for _ in 0..100_000 {
        if at == 0xffff_ffff {
            break;
        }
        let nlen = u32le(&files, at + 0x1c)? as usize;
        let name: Vec<u16> = files
            .get(at + 0x20..at + 0x20 + nlen)
            .ok_or("RomFS: nome truncado")?
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        if name == want {
            let off = u64::from_le_bytes(files.get(at + 8..at + 16).ok_or("RomFS truncado")?.try_into().unwrap());
            return Ok(l3 + fdata + off);
        }
        at = u32le(&files, at + 4)? as usize;
    }
    Err("yw1_a.fa não está na raiz do RomFS".into())
}

/// Lê um arquivo do `.fa` do usuário. Devolve também de onde veio (para o log). Err = jogo não encontrado.
pub fn read_game_file(base: &Path, want: &str) -> Result<(Vec<u8>, String)> {
    let mut tried = Vec::new();
    let fas: Vec<PathBuf> = std::env::var_os("YW_FA")
        .map(PathBuf::from)
        .into_iter()
        .chain([base.join("romfs/yw1_a.fa"), base.join("romfs/0004000000167700/yw1_a.fa")])
        .collect();
    for p in fas {
        match File::open(&p) {
            Ok(mut f) => match fa_read(&mut f, 0, want) {
                Ok(d) => return Ok((d, p.display().to_string())),
                Err(e) => tried.push(format!("{}: {e}", p.display())),
            },
            Err(_) => tried.push(format!("{}: ausente", p.display())),
        }
    }
    let game = std::env::var_os("YW_GAME").map(PathBuf::from).unwrap_or_else(|| base.join("Yo-Kai Watch (USA).3ds"));
    match File::open(&game) {
        Ok(mut f) => match fa_in_3ds(&mut f).and_then(|at| fa_read(&mut f, at, want)) {
            Ok(d) => return Ok((d, format!("{} (RomFS)", game.display()))),
            Err(e) => tried.push(format!("{}: {e}", game.display())),
        },
        Err(_) => tried.push(format!("{}: ausente", game.display())),
    }
    Err(tried.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O `fix.xr` lido de dentro do `.3ds` (RomFS) é igual ao do `yw1_a.fa` extraído (precisa dos dois arquivos do
    /// usuário: `cargo test --release -- --ignored`).
    #[test]
    #[ignore]
    fn fix_xr_3ds_equals_extracted_fa() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut fa = File::open(base.join("romfs/yw1_a.fa")).expect("romfs/yw1_a.fa");
        let a = fa_read(&mut fa, 0, "fix.xr").unwrap();
        let mut g = File::open(base.join("Yo-Kai Watch (USA).3ds")).expect(".3ds");
        let at = fa_in_3ds(&mut g).unwrap();
        let b = fa_read(&mut g, at, "fix.xr").unwrap();
        assert_eq!(a.len(), 57884);
        assert_eq!(a, b);
    }
}
