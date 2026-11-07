//! Localiza o RomFS extraído do CIA do usuário (nunca embutido no projeto).
//! Ordem: argumento explícito → `YW_ROMFS` → `BASE/romfs/0004000000167700` → `BASE/romfs`,
//! onde BASE é a pasta que contém `yokai-port/` (mesma convenção de tools/paths.py).

use std::path::{Path, PathBuf};

pub fn locate(explicit: Option<&Path>) -> Option<PathBuf> {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let candidates = explicit
        .map(Path::to_path_buf)
        .into_iter()
        .chain(std::env::var_os("YW_ROMFS").map(PathBuf::from))
        .chain([
            base.join("romfs").join("0004000000167700"),
            base.join("romfs"),
        ]);
    for dir in candidates {
        if dir.join("mov").is_dir() || dir.join("yw1_a.fa").is_file() {
            return Some(dir.canonicalize().map(strip_verbatim).unwrap_or(dir));
        }
    }
    None
}

/// `canonicalize` no Windows devolve `\\?\C:\...`; tira o prefixo quando é um caminho de disco comum.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(s) if !s.starts_with("UNC") => PathBuf::from(s),
        _ => p,
    }
}
