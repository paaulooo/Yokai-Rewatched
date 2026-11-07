//! `fs:USER` — arquivos do jogo.
//!
//! - Arquivo 3 (SelfNCCH) com caminho binário vazio = imagem RomFS (nível 3 do IVFC): lida direto do `.3ds` do
//!   usuário (NCSD → partição 0 → NCCH → RomFS → IVFC). O próprio jogo interpreta a imagem.
//! - SaveData (4) / ExtSaveData (6) / SDMC (9): pastas no host em `%APPDATA%/yw1/recomp/<arquivo>/`.
//!
//! Comandos e layouts: 3dbrew "Filesystem services" / "FS:OpenFile" etc.

use super::{unknown, Ipc};
use crate::hle::kernel::{Obj, K, RESULT_OK};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

const ERR_NOT_FOUND: u32 = 0xc880_4478;
const ERR_NOT_FORMATTED: u32 = 0xc8a0_4554;
const ERR_EXT_NOT_FOUND: u32 = 0xc8a0_4478;

enum Node {
    /// imagem RomFS: arquivo do host + deslocamento do nível 3 + tamanho
    Rom { file: File, base: u64, size: u64 },
    Host { file: File },
    Dir,
}

struct Fs {
    archives: HashMap<u64, (u32, PathBuf)>,
    next_archive: u64,
    nodes: HashMap<u32, Node>,
    next_node: u32,
}

static FS: Mutex<Option<Fs>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Fs) -> R) -> R {
    let mut g = FS.lock().unwrap();
    f(g.get_or_insert_with(|| Fs {
        archives: HashMap::new(),
        next_archive: 1,
        nodes: HashMap::new(),
        next_node: 1,
    }))
}

/// Caminho do jogo (`.3ds`): `YW_GAME` ou `BASE/Yo-Kai Watch (USA).3ds`.
fn game_file() -> PathBuf {
    if let Some(p) = std::env::var_os("YW_GAME") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Yo-Kai Watch (USA).3ds")
}

/// Localiza o nível 3 do RomFS dentro do `.3ds`: (deslocamento, tamanho).
fn romfs_level3(f: &mut File) -> std::io::Result<(u64, u64)> {
    let mut h = [0u8; 0x200];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut h)?;
    if &h[0x100..0x104] != b"NCSD" {
        return Err(std::io::Error::other("não é NCSD (.3ds)"));
    }
    let part0 = u32::from_le_bytes(h[0x120..0x124].try_into().unwrap()) as u64 * 0x200;
    f.seek(SeekFrom::Start(part0))?;
    f.read_exact(&mut h)?;
    if &h[0x100..0x104] != b"NCCH" {
        return Err(std::io::Error::other("partição 0 não é NCCH"));
    }
    let romfs = part0 + u32::from_le_bytes(h[0x1b0..0x1b4].try_into().unwrap()) as u64 * 0x200;
    let mut iv = [0u8; 0x60];
    f.seek(SeekFrom::Start(romfs))?;
    f.read_exact(&mut iv)?;
    if &iv[0..4] != b"IVFC" {
        return Err(std::io::Error::other("RomFS sem IVFC (criptografado?)"));
    }
    let master = u32::from_le_bytes(iv[0x08..0x0c].try_into().unwrap()) as u64;
    let l3_size = u64::from_le_bytes(iv[0x44..0x4c].try_into().unwrap());
    let l3_block = 1u64 << u32::from_le_bytes(iv[0x4c..0x50].try_into().unwrap());
    let off = (0x60 + master).div_ceil(l3_block) * l3_block;
    Ok((romfs + off, l3_size))
}

fn save_root() -> PathBuf {
    if let Some(p) = std::env::var_os("YW_SAVES") {
        return PathBuf::from(p).join("recomp");
    }
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("yw1").join("recomp")
}

/// Lê um caminho FS (tipo 2 binário, 3 ASCII, 4 UTF-16).
fn read_path(ty: u32, size: u32, ptr: u32) -> (String, Vec<u8>) {
    let mut raw = vec![0u8; size as usize];
    yw1_rt::read_bytes(ptr, &mut raw);
    let text = match ty {
        3 => String::from_utf8_lossy(raw.split(|&b| b == 0).next().unwrap_or(&[])).into_owned(),
        4 => {
            let u: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
            String::from_utf16_lossy(&u)
        }
        _ => String::new(),
    };
    (text, raw)
}

fn archive_dir(id: u32, path: &[u8]) -> PathBuf {
    let name = match id {
        4 => "savedata".to_string(),
        6 => format!("extdata_{}", hex(path)),
        7 => format!("shared_extdata_{}", hex(path)),
        9 => "sdmc".to_string(),
        _ => format!("archive_{id:x}"),
    };
    save_root().join(name)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn new_session(fs: &mut Fs, n: Node, kind: &str) -> u32 {
    let id = fs.next_node;
    fs.next_node += 1;
    fs.nodes.insert(id, n);
    K.lock().create(Obj::Session {
        service: format!("fs:{kind}:{id}"),
    })
}

fn open_rom(fs: &mut Fs) -> Result<u32, u32> {
    let path = game_file();
    let mut file = File::open(&path).map_err(|e| {
        eprintln!("[fs] não consegui abrir {}: {e} (defina YW_GAME)", path.display());
        ERR_NOT_FOUND
    })?;
    let (base, size) = romfs_level3(&mut file).map_err(|e| {
        eprintln!("[fs] RomFS de {}: {e}", path.display());
        ERR_NOT_FOUND
    })?;
    eprintln!("[fs] RomFS: {} @ {base:#x} ({size} bytes)", path.display());
    Ok(new_session(fs, Node::Rom { file, base, size }, "file"))
}

fn open_host(fs: &mut Fs, dir: PathBuf, path: &str, flags: u32) -> Result<u32, u32> {
    let p = dir.join(path.trim_start_matches('/'));
    let create = flags & 4 != 0;
    let write = flags & 2 != 0;
    if create {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let file = OpenOptions::new().read(true).write(write || create).create(create).open(&p).map_err(|_| ERR_NOT_FOUND)?;
    Ok(new_session(fs, Node::Host { file }, "file"))
}

pub fn request(ipc: &Ipc, session: Option<&str>) {
    if let Some(s) = session {
        let id: u32 = s.rsplit(':').next().and_then(|x| x.parse().ok()).unwrap_or(0);
        return node_request(ipc, id, s.starts_with("fs:dir:"));
    }
    match ipc.cmd() {
        // Initialize / InitializeWithSdkVersion / SetPriority
        0x0801 | 0x0861 | 0x0862 => ipc.ok(1, 0),
        // GetPriority
        0x0863 => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        // OpenFile(transação, arquivo lo/hi, tipo, tamanho, flags, atributos, desc, ptr)
        0x0802 => {
            let ah = ipc.get(2) as u64 | (ipc.get(3) as u64) << 32;
            let (path, _) = read_path(ipc.get(4), ipc.get(5), ipc.get(9));
            let flags = ipc.get(6);
            let r = with(|fs| match fs.archives.get(&ah).cloned() {
                Some((3, _)) => open_rom(fs),
                Some((_, dir)) => open_host(fs, dir, &path, flags),
                None => Err(ERR_NOT_FOUND),
            });
            reply_handle(ipc, r);
        }
        // OpenFileDirectly(transação, id do arquivo, tipo/tamanho do caminho do arquivo, tipo/tamanho do arquivo, flags, atributos, desc, ptr, desc, ptr)
        0x0803 => {
            let id = ipc.get(2);
            let (_, apath) = read_path(ipc.get(3), ipc.get(4), ipc.get(10));
            let (path, _) = read_path(ipc.get(5), ipc.get(6), ipc.get(12));
            let flags = ipc.get(7);
            let r = with(|fs| match id {
                3 | 0x2345678a => open_rom(fs),
                _ => open_host(fs, archive_dir(id, &apath), &path, flags),
            });
            reply_handle(ipc, r);
        }
        // DeleteFile(transação, arquivo lo/hi, tipo, tamanho, desc, ptr)
        0x0804 => {
            let ah = ipc.get(2) as u64 | (ipc.get(3) as u64) << 32;
            let (path, _) = read_path(ipc.get(4), ipc.get(5), ipc.get(7));
            let r = with(|fs| fs.archives.get(&ah).map(|a| a.1.clone()));
            match r.map(|d| std::fs::remove_file(d.join(path.trim_start_matches('/')))) {
                Some(Ok(())) => ipc.ok(1, 0),
                _ => ipc.reply(1, 0, ERR_NOT_FOUND),
            }
        }
        // CreateFile(transação, arquivo lo/hi, tipo, tamanho, atributos, tamanho do arquivo lo/hi, desc, ptr)
        0x0808 => {
            let ah = ipc.get(2) as u64 | (ipc.get(3) as u64) << 32;
            let (path, _) = read_path(ipc.get(4), ipc.get(5), ipc.get(10));
            let size = ipc.get(7) as u64 | (ipc.get(8) as u64) << 32;
            let dir = with(|fs| fs.archives.get(&ah).map(|a| a.1.clone()));
            match dir {
                Some(d) => {
                    let p = d.join(path.trim_start_matches('/'));
                    if let Some(parent) = p.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let ok = File::create(&p).and_then(|f| f.set_len(size)).is_ok();
                    ipc.reply(1, 0, if ok { RESULT_OK } else { ERR_NOT_FOUND });
                }
                None => ipc.reply(1, 0, ERR_NOT_FOUND),
            }
        }
        // CreateDirectory(transação, arquivo lo/hi, tipo, tamanho, atributos, desc, ptr)
        0x0809 => {
            let ah = ipc.get(2) as u64 | (ipc.get(3) as u64) << 32;
            let (path, _) = read_path(ipc.get(4), ipc.get(5), ipc.get(8));
            if let Some(d) = with(|fs| fs.archives.get(&ah).map(|a| a.1.clone())) {
                let _ = std::fs::create_dir_all(d.join(path.trim_start_matches('/')));
            }
            ipc.ok(1, 0);
        }
        // OpenDirectory(arquivo lo/hi, tipo, tamanho, desc, ptr)
        0x080b => {
            let r = with(|fs| Ok(new_session(fs, Node::Dir, "dir")));
            reply_handle(ipc, r);
        }
        // OpenArchive(id, tipo do caminho, tamanho, desc, ptr) → handle u64
        0x080c => {
            let id = ipc.get(1);
            let (_, path) = read_path(ipc.get(2), ipc.get(3), ipc.get(5));
            let dir = archive_dir(id, &path);
            // 7 = ExtSaveData compartilhado (dados do sistema; sempre existe num console), 9 = SD
            if matches!(id, 7 | 9) {
                let _ = std::fs::create_dir_all(&dir);
            }
            let exists = matches!(id, 3 | 0x2345678a) || dir.exists();
            if !exists {
                eprintln!("[fs] OpenArchive({id:#x}): ainda não existe → o jogo deve formatar/criar");
                ipc.reply(1, 0, if id == 4 { ERR_NOT_FORMATTED } else { ERR_EXT_NOT_FOUND });
                return;
            }
            let h = with(|fs| {
                let h = fs.next_archive;
                fs.next_archive += 1;
                fs.archives.insert(h, (id, dir));
                h
            });
            ipc.ok(3, 0);
            ipc.set(2, h as u32);
            ipc.set(3, (h >> 32) as u32);
        }
        // ControlArchive (commit do save)
        0x080d => ipc.ok(1, 0),
        // CloseArchive
        0x080e => {
            let ah = ipc.get(1) as u64 | (ipc.get(2) as u64) << 32;
            with(|fs| fs.archives.remove(&ah));
            ipc.ok(1, 0);
        }
        // IsSdmcDetected / CardSlotIsInserted
        0x0817 | 0x0821 => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
        }
        // GetFormatInfo(id, …) → tamanho, diretórios, arquivos, duplicar
        0x0845 => {
            ipc.ok(5, 0);
            ipc.set(2, 0x80000);
            ipc.set(3, 10);
            ipc.set(4, 10);
            ipc.set(5, 0);
        }
        // FormatSaveData / CreateExtSaveData / FormatThisUserSaveData
        0x084c | 0x0851 | 0x080f => {
            let id = if ipc.cmd() == 0x0851 { 6 } else { 4 };
            let dir = if id == 6 {
                // CreateExtSaveData: ExtSaveDataInfo nas palavras 1–4 (u8 mídia, …, u64 id) → mesmo caminho binário
                // que o OpenArchive usa: {u32 mídia, u32 id lo, u32 id hi}
                let mut p = Vec::new();
                p.extend_from_slice(&(ipc.get(1) & 0xff).to_le_bytes());
                p.extend_from_slice(&ipc.get(2).to_le_bytes());
                p.extend_from_slice(&ipc.get(3).to_le_bytes());
                archive_dir(6, &p)
            } else {
                archive_dir(4, &[])
            };
            let _ = std::fs::create_dir_all(dir);
            ipc.ok(1, 0);
        }
        _ => unknown("fs:USER", ipc),
    }
}

fn reply_handle(ipc: &Ipc, r: Result<u32, u32>) {
    match r {
        Ok(h) => {
            ipc.ok(1, 2);
            ipc.set(2, 0x10);
            ipc.set(3, h);
        }
        Err(e) => ipc.reply(1, 0, e),
    }
}

/// Comandos de uma sessão de arquivo/diretório.
fn node_request(ipc: &Ipc, id: u32, dir: bool) {
    if dir {
        match ipc.cmd() {
            // Read(n, desc, ptr) → nenhuma entrada [H]
            0x0801 => {
                ipc.ok(2, 0);
                ipc.set(2, 0);
            }
            _ => ipc.ok(1, 0),
        }
        return;
    }
    match ipc.cmd() {
        // Read(off lo/hi, tamanho, desc, ptr) → lidos
        0x0802 => {
            let off = ipc.get(1) as u64 | (ipc.get(2) as u64) << 32;
            let size = ipc.get(3);
            let ptr = ipc.get(5);
            let n = with(|fs| {
                let mut buf = vec![0u8; size as usize];
                let n = match fs.nodes.get_mut(&id) {
                    Some(Node::Rom { file, base, size: total }) => {
                        let len = (size as u64).min(total.saturating_sub(off)) as usize;
                        file.seek(SeekFrom::Start(*base + off)).and_then(|_| file.read_exact(&mut buf[..len])).map(|_| len).unwrap_or(0)
                    }
                    Some(Node::Host { file }) => file.seek(SeekFrom::Start(off)).and_then(|_| read_up_to(file, &mut buf)).unwrap_or(0),
                    _ => 0,
                };
                yw1_rt::write_bytes(ptr, &buf[..n]);
                n
            });
            ipc.ok(2, 2);
            ipc.set(2, n as u32);
            ipc.set(3, (size << 4) | 0xc);
            ipc.set(4, ptr);
        }
        // Write(off lo/hi, tamanho, flush, desc, ptr)
        0x0803 => {
            let off = ipc.get(1) as u64 | (ipc.get(2) as u64) << 32;
            let size = ipc.get(3);
            let ptr = ipc.get(6);
            let mut buf = vec![0u8; size as usize];
            yw1_rt::read_bytes(ptr, &mut buf);
            let n = with(|fs| match fs.nodes.get_mut(&id) {
                Some(Node::Host { file }) => file.seek(SeekFrom::Start(off)).and_then(|_| file.write_all(&buf)).map(|_| buf.len()).unwrap_or(0),
                _ => 0,
            });
            ipc.ok(2, 2);
            ipc.set(2, n as u32);
            ipc.set(3, (size << 4) | 0xa);
            ipc.set(4, ptr);
        }
        // GetSize
        0x0804 => {
            let s = with(|fs| match fs.nodes.get_mut(&id) {
                Some(Node::Rom { size, .. }) => *size,
                Some(Node::Host { file }) => file.metadata().map(|m| m.len()).unwrap_or(0),
                _ => 0,
            });
            ipc.ok(3, 0);
            ipc.set(2, s as u32);
            ipc.set(3, (s >> 32) as u32);
        }
        // SetSize
        0x0805 => {
            let s = ipc.get(1) as u64 | (ipc.get(2) as u64) << 32;
            with(|fs| {
                if let Some(Node::Host { file }) = fs.nodes.get_mut(&id) {
                    let _ = file.set_len(s);
                }
            });
            ipc.ok(1, 0);
        }
        // GetAttributes / SetAttributes / SetPriority / GetPriority
        0x0806 | 0x080b => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        0x0807 | 0x080a => ipc.ok(1, 0),
        // OpenLinkFile → outro handle para o mesmo arquivo
        0x080c => {
            let r = with(|fs| {
                let n = match fs.nodes.get(&id) {
                    Some(Node::Rom { file, base, size }) => file.try_clone().ok().map(|f| Node::Rom { file: f, base: *base, size: *size }),
                    Some(Node::Host { file }) => file.try_clone().ok().map(|f| Node::Host { file: f }),
                    _ => None,
                };
                n.map(|n| new_session(fs, n, "file")).ok_or(ERR_NOT_FOUND)
            });
            reply_handle(ipc, r);
        }
        // Close
        0x0808 => {
            with(|fs| fs.nodes.remove(&id));
            ipc.ok(1, 0);
        }
        // Flush
        0x0809 => {
            with(|fs| {
                if let Some(Node::Host { file }) = fs.nodes.get_mut(&id) {
                    let _ = file.flush();
                }
            });
            ipc.ok(1, 0);
        }
        _ => unknown("fs:file", ipc),
    }
}

fn read_up_to(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    Ok(n)
}
