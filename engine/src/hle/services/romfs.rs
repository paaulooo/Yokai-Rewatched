//! Nível 3 da RomFS (formato: 3dbrew "RomFS") e a RomFS VIRTUAL dos mods no formato LayeredFS (Luma3DS/Azahar:
//! `<mod>/romfs/<caminho na RomFS>` troca o arquivo inteiro — ex.: `romfs/yw1_a.fa`, `romfs/mov/yw1_op.moflex`).
//!
//! O jogo lê a imagem do nível 3 sozinho (`fs.rs`, arquivo SelfNCCH). Para trocar arquivos de tamanho diferente sem
//! copiar a imagem (≈ 1 GB), a imagem virtual é:
//! - `[0, tamanho original)`: a imagem do `.3ds`, com a TABELA DE METADADOS DOS ARQUIVOS substituída por uma cópia em
//!   que cada arquivo trocado aponta (deslocamento/tamanho) para…
//! - `[alinhado(tamanho original), …)`: os arquivos do mod, um depois do outro (alinhados a 16), lidos direto do PC.
//! Hashes das tabelas, nomes e diretórios ficam iguais; o jogo só vê deslocamentos e tamanhos novos.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;

const NADA: u32 = 0xFFFF_FFFF;

/// Cabeçalho do nível 3 (0x28 bytes; todos os deslocamentos relativos ao início do nível 3).
#[derive(Clone, Copy, Debug)]
struct Cabecalho {
    dir_meta: (u32, u32),
    arq_meta: (u32, u32),
    dados: u32,
}

/// Um arquivo da RomFS.
#[derive(Clone, Debug)]
pub struct Entrada {
    /// caminho normalizado (minúsculas, `/`, sem `/` inicial) — o mesmo de `mods::indice::normalizar`
    pub caminho: String,
    /// posição da entrada dentro da tabela de metadados dos arquivos
    pub meta: u32,
    /// início dos dados (absoluto no nível 3) e tamanho
    pub ini: u64,
    pub tam: u64,
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn nome_utf16(b: &[u8], o: usize, len: usize) -> Option<String> {
    let s = b.get(o..o + len)?;
    let u: Vec<u16> = s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Some(String::from_utf16_lossy(&u))
}

fn ler_cab(r: &mut (impl Read + Seek), base: u64) -> std::io::Result<Cabecalho> {
    let mut h = [0u8; 0x28];
    r.seek(SeekFrom::Start(base))?;
    r.read_exact(&mut h)?;
    if u32le(&h, 0) != 0x28 {
        return Err(std::io::Error::other(format!("cabeçalho do nível 3 com tamanho {:#x} (esperado 0x28)", u32le(&h, 0))));
    }
    Ok(Cabecalho { dir_meta: (u32le(&h, 0xc), u32le(&h, 0x10)), arq_meta: (u32le(&h, 0x1c), u32le(&h, 0x20)), dados: u32le(&h, 0x24) })
}

fn ler_tabela(r: &mut (impl Read + Seek), base: u64, (off, len): (u32, u32)) -> std::io::Result<Vec<u8>> {
    let mut v = vec![0u8; len as usize];
    r.seek(SeekFrom::Start(base + off as u64))?;
    r.read_exact(&mut v)?;
    Ok(v)
}

/// Todos os arquivos da RomFS cujo nível 3 começa em `base`.
pub fn listar(r: &mut (impl Read + Seek), base: u64) -> std::io::Result<Vec<Entrada>> {
    let cab = ler_cab(r, base)?;
    let dirs = ler_tabela(r, base, cab.dir_meta)?;
    let arqs = ler_tabela(r, base, cab.arq_meta)?;
    let mut out = Vec::new();
    // (diretório, prefixo); a raiz é a entrada 0 e não tem nome
    let mut pilha: Vec<(u32, String)> = vec![(0, String::new())];
    let mut visitados = 0usize;
    while let Some((d, pref)) = pilha.pop() {
        visitados += 1;
        if visitados > 1_000_000 || d as usize + 0x18 > dirs.len() {
            return Err(std::io::Error::other("tabela de diretórios inválida"));
        }
        let mut f = u32le(&dirs, d as usize + 0xc);
        while f != NADA {
            let o = f as usize;
            if o + 0x20 > arqs.len() {
                return Err(std::io::Error::other("tabela de arquivos inválida"));
            }
            let nome = nome_utf16(&arqs, o + 0x20, u32le(&arqs, o + 0x1c) as usize)
                .ok_or_else(|| std::io::Error::other("nome de arquivo fora da tabela"))?;
            out.push(Entrada {
                caminho: format!("{pref}{nome}").to_ascii_lowercase(),
                meta: f,
                ini: cab.dados as u64 + u64le(&arqs, o + 8),
                tam: u64le(&arqs, o + 0x10),
            });
            f = u32le(&arqs, o + 4);
            if out.len() > 1_000_000 {
                return Err(std::io::Error::other("tabela de arquivos em ciclo"));
            }
        }
        let mut c = u32le(&dirs, d as usize + 8);
        while c != NADA {
            let o = c as usize;
            if o + 0x18 > dirs.len() {
                return Err(std::io::Error::other("tabela de diretórios inválida"));
            }
            let nome = nome_utf16(&dirs, o + 0x18, u32le(&dirs, o + 0x14) as usize)
                .ok_or_else(|| std::io::Error::other("nome de diretório fora da tabela"))?;
            pilha.push((c, format!("{pref}{nome}/")));
            c = u32le(&dirs, o + 4);
        }
    }
    Ok(out)
}

/// Arquivo do mod anexado ao fim da imagem virtual.
struct Extra {
    ini: u64,
    tam: u64,
    caminho: PathBuf,
    arq: Mutex<Option<File>>,
}

/// A RomFS virtual: imagem original + tabela de arquivos corrigida + arquivos dos mods no fim.
pub struct Camadas {
    tam_original: u64,
    tam_total: u64,
    /// posição (no nível 3) e conteúdo da tabela de metadados dos arquivos, já com os deslocamentos novos
    meta_ini: u64,
    meta: Vec<u8>,
    extras: Vec<Extra>,
}

/// Resultado de [`montar`] para o log/menu.
pub struct Montagem {
    pub camadas: Camadas,
    /// (caminho, tamanho original, tamanho novo)
    pub trocados: Vec<(String, u64, u64)>,
}

const ALINHA: u64 = 16;

/// Monta a RomFS virtual: `trocas` = (caminho normalizado, arquivo no PC). Caminhos que não existem na RomFS são um
/// erro de quem chamou (filtre antes com [`listar`]).
pub fn montar(r: &mut (impl Read + Seek), base: u64, tam_original: u64, trocas: &[(String, PathBuf)]) -> std::io::Result<Montagem> {
    let cab = ler_cab(r, base)?;
    let mut meta = ler_tabela(r, base, cab.arq_meta)?;
    let por_caminho: std::collections::HashMap<String, Entrada> =
        listar(r, base)?.into_iter().map(|e| (e.caminho.clone(), e)).collect();
    let mut pos = tam_original.div_ceil(ALINHA) * ALINHA;
    let mut extras = Vec::new();
    let mut trocados = Vec::new();
    for (c, p) in trocas {
        let Some(e) = por_caminho.get(c) else {
            return Err(std::io::Error::other(format!("{c}: não existe na RomFS")));
        };
        let tam = std::fs::metadata(p)?.len();
        let o = e.meta as usize;
        meta[o + 8..o + 0x10].copy_from_slice(&(pos - cab.dados as u64).to_le_bytes());
        meta[o + 0x10..o + 0x18].copy_from_slice(&tam.to_le_bytes());
        extras.push(Extra { ini: pos, tam, caminho: p.clone(), arq: Mutex::new(None) });
        trocados.push((c.clone(), e.tam, tam));
        pos = (pos + tam).div_ceil(ALINHA) * ALINHA;
    }
    let tam_total = if extras.is_empty() { tam_original } else { pos };
    Ok(Montagem {
        camadas: Camadas { tam_original, tam_total, meta_ini: cab.arq_meta.0 as u64, meta, extras },
        trocados,
    })
}

impl Camadas {
    pub fn tamanho(&self) -> u64 {
        self.tam_total
    }

    /// Lê `buf.len()` bytes a partir de `off` da imagem virtual (`orig` = o `.3ds`, nível 3 em `base`). Devolve quantos
    /// bytes valem (menos no fim da imagem ou se um arquivo do mod sumiu do PC).
    pub fn ler(&self, orig: &mut File, base: u64, off: u64, buf: &mut [u8]) -> usize {
        let n = (buf.len() as u64).min(self.tam_total.saturating_sub(off)) as usize;
        let buf = &mut buf[..n];
        buf.fill(0);
        let fim = off + n as u64;
        // 1) imagem original
        if off < self.tam_original {
            let k = (fim.min(self.tam_original) - off) as usize;
            if orig.seek(SeekFrom::Start(base + off)).and_then(|_| orig.read_exact(&mut buf[..k])).is_err() {
                return 0;
            }
        }
        // 2) tabela de arquivos corrigida por cima
        let (mi, mf) = (self.meta_ini, self.meta_ini + self.meta.len() as u64);
        if off < mf && fim > mi {
            let a = off.max(mi);
            let b = fim.min(mf);
            buf[(a - off) as usize..(b - off) as usize].copy_from_slice(&self.meta[(a - mi) as usize..(b - mi) as usize]);
        }
        // 3) arquivos dos mods
        for e in &self.extras {
            let (ei, ef) = (e.ini, e.ini + e.tam);
            if off >= ef || fim <= ei {
                continue;
            }
            let a = off.max(ei);
            let b = fim.min(ef);
            let mut g = e.arq.lock().unwrap_or_else(|x| x.into_inner());
            if g.is_none() {
                match File::open(&e.caminho) {
                    Ok(f) => *g = Some(f),
                    Err(err) => {
                        eprintln!("[fs] mod: {}: {err}", e.caminho.display());
                        return 0;
                    }
                }
            }
            let f = g.as_mut().unwrap();
            let dst = &mut buf[(a - off) as usize..(b - off) as usize];
            if f.seek(SeekFrom::Start(a - ei)).and_then(|_| f.read_exact(dst)).is_err() {
                return 0;
            }
        }
        n
    }
}

#[cfg(test)]
pub(crate) mod testes {
    use super::*;
    use std::io::Cursor;

    fn utf16(s: &str) -> Vec<u8> {
        let mut v: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
        while v.len() % 4 != 0 {
            v.push(0);
        }
        v
    }

    /// RomFS mínima: raiz { a.bin = "AAAA", d/ { b.bin = "BBBBBBBB" } } (sem tabelas de hash; o leitor não usa).
    pub(crate) fn romfs_minima() -> Vec<u8> {
        // diretórios: raiz (0) e "d" (0x18)
        let mut dirs = Vec::new();
        let d_nome = utf16("d");
        // raiz: pai 0, irmão -, 1º filho 0x18, 1º arquivo 0, hash -, nome 0
        for x in [0u32, NADA, 0x18, 0, NADA, 0] {
            dirs.extend_from_slice(&x.to_le_bytes());
        }
        // "d": pai 0, irmão -, filho -, 1º arquivo (depois de a.bin), hash -, nome
        let a_nome = utf16("a.bin");
        let b_off = 0x20 + a_nome.len() as u32;
        for x in [0u32, NADA, NADA, b_off, NADA, 2] {
            dirs.extend_from_slice(&x.to_le_bytes());
        }
        dirs.extend_from_slice(&d_nome);
        // arquivos
        let mut arqs = Vec::new();
        let ent = |v: &mut Vec<u8>, pai: u32, irmao: u32, off: u64, tam: u64, nome: &str| {
            v.extend_from_slice(&pai.to_le_bytes());
            v.extend_from_slice(&irmao.to_le_bytes());
            v.extend_from_slice(&off.to_le_bytes());
            v.extend_from_slice(&tam.to_le_bytes());
            v.extend_from_slice(&NADA.to_le_bytes());
            v.extend_from_slice(&((nome.len() * 2) as u32).to_le_bytes());
            v.extend_from_slice(&utf16(nome));
        };
        ent(&mut arqs, 0, NADA, 0, 4, "a.bin");
        ent(&mut arqs, 0x18, NADA, 16, 8, "b.bin");
        let dir_off = 0x28u32;
        let arq_off = dir_off + dirs.len() as u32;
        let dados = (arq_off + arqs.len() as u32).div_ceil(16) * 16;
        let mut img = Vec::new();
        for x in [0x28u32, dir_off, 0, dir_off, dirs.len() as u32, arq_off, 0, arq_off, arqs.len() as u32, dados] {
            img.extend_from_slice(&x.to_le_bytes());
        }
        img.extend_from_slice(&dirs);
        img.extend_from_slice(&arqs);
        img.resize(dados as usize, 0);
        img.extend_from_slice(b"AAAA");
        img.resize(dados as usize + 16, 0);
        img.extend_from_slice(b"BBBBBBBB");
        img
    }

    #[test]
    fn romfs_lista_arquivos() {
        let img = romfs_minima();
        let mut v = listar(&mut Cursor::new(&img), 0).unwrap();
        v.sort_by(|a, b| a.caminho.cmp(&b.caminho));
        let c: Vec<_> = v.iter().map(|e| (e.caminho.as_str(), e.tam)).collect();
        assert_eq!(c, [("a.bin", 4), ("d/b.bin", 8)]);
        let b = &v[1];
        assert_eq!(&img[b.ini as usize..(b.ini + b.tam) as usize], b"BBBBBBBB");
    }

    #[test]
    fn romfs_virtual_troca_arquivo_maior() {
        let img = romfs_minima();
        let dir = std::env::temp_dir().join(format!("yw1_romfs_teste_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let orig_p = dir.join("orig.bin");
        std::fs::write(&orig_p, &img).unwrap();
        let novo = dir.join("b.bin");
        std::fs::write(&novo, b"traducao pt-br, maior que o original").unwrap();
        let mut orig = File::open(&orig_p).unwrap();
        let m = montar(&mut orig, 0, img.len() as u64, &[("d/b.bin".into(), novo.clone())]).unwrap();
        assert_eq!(m.trocados, [("d/b.bin".to_string(), 8, 36)]);
        let cam = m.camadas;
        // a imagem virtual inteira, lida em pedaços tortos, tem a tabela nova e o arquivo novo no fim
        let mut virt = vec![0u8; cam.tamanho() as usize];
        let mut off = 0;
        while off < virt.len() {
            let k = 7.min(virt.len() - off);
            assert_eq!(cam.ler(&mut orig, 0, off as u64, &mut virt[off..off + k]), k);
            off += k;
        }
        let mut v = listar(&mut Cursor::new(&virt), 0).unwrap();
        v.sort_by(|a, b| a.caminho.cmp(&b.caminho));
        let a = &v[0];
        assert_eq!(&virt[a.ini as usize..(a.ini + a.tam) as usize], b"AAAA");
        let b = &v[1];
        assert_eq!(&virt[b.ini as usize..(b.ini + b.tam) as usize], b"traducao pt-br, maior que o original");
        // fora da imagem: 0 bytes
        assert_eq!(cam.ler(&mut orig, 0, cam.tamanho(), &mut [0u8; 4]), 0);
        // caminho inexistente é erro
        assert!(montar(&mut orig, 0, img.len() as u64, &[("x.bin".into(), novo)]).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
