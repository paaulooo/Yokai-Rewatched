//! Gerado por yw1-recomp (modelo: recomp/src/app_build.rs): compila os shaders de vértice AOT
//! (`../shaders/aot_NN.vert`, listados em `../shaders/aot.txt`) para SPIR-V com o `glslangValidator` — `YW_GLSLANG`,
//! o do MSYS2 UCRT64 ou o do PATH, como o `engine/build.rs` — e gera `OUT_DIR/aot.rs` com a lista
//! (registro, SPIR-V) que o `main` registra no motor. Sem o glslang ou com erro num shader: aviso e o programa fica
//! sem AOT (o motor usa o ubershader). Compila em paralelo; só roda de novo quando os shaders mudam.

use std::path::{Path, PathBuf};
use std::process::Command;

fn glslang() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("YW_GLSLANG") {
        cands.push(p.into());
    }
    cands.push("C:/msys64/ucrt64/bin/glslangValidator.exe".into());
    cands.push("glslangValidator".into());
    cands.into_iter().find(|p| Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success()))
}

fn compile(tool: &Path, src: &Path, dst: &Path) -> Result<(), String> {
    let o = Command::new(tool).arg("-V").arg(src).arg("-o").arg(dst).output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../shaders");
    let manifest = dir.join("aot.txt");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-env-changed=YW_GLSLANG");
    let text = std::fs::read_to_string(&manifest).unwrap_or_default();
    let recs: Vec<(String, String)> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let file = l.split_whitespace().find_map(|f| f.strip_prefix("file="))?;
            Some((l.to_string(), file.to_string()))
        })
        .collect();
    let tool = if recs.is_empty() { None } else { glslang() };
    if !recs.is_empty() && tool.is_none() {
        println!("cargo:warning=glslangValidator não encontrado: executável sem shaders AOT (só o ubershader)");
        // Para o próximo build achar o glslang instalado depois: o cargo roda este script de novo quando o caminho
        // padrão do MSYS2 passa a existir (arquivo vigiado que não existe = script roda a cada build, barato aqui), o
        // PATH muda ou algum shader muda.
        println!("cargo:rerun-if-changed=C:/msys64/ucrt64/bin/glslangValidator.exe");
        println!("cargo:rerun-if-env-changed=PATH");
        for (_, file) in &recs {
            println!("cargo:rerun-if-changed={}", dir.join(format!("{file}.vert")).display());
        }
    }
    let t0 = std::time::Instant::now();
    let mut ok: Vec<(String, PathBuf)> = Vec::new();
    if let Some(tool) = &tool {
        let results: Vec<(String, PathBuf, Result<(), String>)> = std::thread::scope(|s| {
            let hs: Vec<_> = recs
                .iter()
                .map(|(rec, file)| {
                    let src = dir.join(format!("{file}.vert"));
                    let dst = out.join(format!("{file}.spv"));
                    println!("cargo:rerun-if-changed={}", src.display());
                    s.spawn(move || {
                        let r = compile(tool, &src, &dst);
                        (rec.clone(), dst, r)
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (rec, dst, r) in results {
            match r {
                Ok(()) => ok.push((rec, dst)),
                Err(e) => {
                    let name = rec.split_whitespace().next().unwrap_or("?");
                    println!("cargo:warning=shader AOT {name} não compilou (fica com o ubershader): {}", e.lines().take(3).collect::<Vec<_>>().join(" | "));
                }
            }
        }
    }
    let mut src = String::from("/// (registro, SPIR-V) dos shaders de vértice AOT — gerado pelo build.rs (derivado do jogo).\n");
    src.push_str("pub static AOT: &[(&str, &[u8])] = &[\n");
    for (rec, dst) in &ok {
        src.push_str(&format!("    ({rec:?}, include_bytes!({:?})),\n", dst.display().to_string()));
    }
    src.push_str("];\n");
    std::fs::write(out.join("aot.rs"), src).unwrap();
    if !ok.is_empty() {
        println!("cargo:warning={} shaders AOT compilados em {:.1}s", ok.len(), t0.elapsed().as_secs_f64());
    }
}
