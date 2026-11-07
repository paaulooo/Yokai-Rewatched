//! Compila os shaders GLSL do backend Vulkan (`src/render/shaders/*.{vert,frag}`) para SPIR-V em `OUT_DIR`, com o
//! `glslangValidator` (variável `YW_GLSLANG`, o do MSYS2 UCRT64 ou o do PATH). Sem ele, usa os `.spv` pré-gerados
//! de `src/render/spv/` (gerados por este mesmo script; são nossos, não contêm nada do jogo).

use std::path::{Path, PathBuf};
use std::process::Command;

const SHADERS: [&str; 2] = ["pica.vert", "pica.frag"];

fn glslang() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("YW_GLSLANG") {
        cands.push(p.into());
    }
    cands.push("C:/msys64/ucrt64/bin/glslangValidator.exe".into());
    cands.push("glslangValidator".into());
    cands.into_iter().find(|p| Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success()))
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let src = manifest.join("src/render/shaders");
    let pre = manifest.join("src/render/spv");
    println!("cargo:rerun-if-env-changed=YW_GLSLANG");
    let tool = glslang();
    for name in SHADERS {
        let input = src.join(name);
        println!("cargo:rerun-if-changed={}", input.display());
        let spv = format!("{}.spv", name.replace('.', "_"));
        let dst = out.join(&spv);
        match &tool {
            Some(t) => {
                let o = Command::new(t).arg("-V").arg(&input).arg("-o").arg(&dst).output().expect("glslangValidator");
                if !o.status.success() {
                    panic!(
                        "falha ao compilar {name}:\n{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
                // mantém a cópia pré-gerada em dia (para quem compila sem o glslang)
                let _ = std::fs::create_dir_all(&pre);
                let _ = std::fs::copy(&dst, pre.join(&spv));
            }
            None => copy_prebuilt(&pre.join(&spv), &dst),
        }
    }
}

fn copy_prebuilt(from: &Path, to: &Path) {
    println!("cargo:rerun-if-changed={}", from.display());
    if std::fs::copy(from, to).is_err() {
        panic!(
            "glslangValidator não encontrado e não há {} — instale o glslang (MSYS2: pacman -S mingw-w64-ucrt-x86_64-glslang) ou defina YW_GLSLANG",
            from.display()
        );
    }
}
