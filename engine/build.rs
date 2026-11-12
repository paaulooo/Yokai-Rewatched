//! Compila os shaders GLSL do backend Vulkan (`src/render/shaders/*.{vert,frag}`) para SPIR-V em `OUT_DIR`, com o
//! `glslangValidator` (variável `YW_GLSLANG`, o do MSYS2 UCRT64 ou o do PATH). Sem ele, usa os `.spv` pré-gerados
//! de `src/render/spv/` (gerados por este mesmo script; são nossos, não contêm nada do jogo).
//! Uma fonte pode gerar mais de um módulo com `#define`s diferentes (ex.: `pica_frag_fastz.spv` = `pica.frag` com
//! `-DFAST_Z`, sem escrita de `gl_FragDepth`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// (fonte em `src/render/shaders/`, módulo SPIR-V gerado em `OUT_DIR` e copiado para `src/render/spv/`, defines)
const SHADERS: [(&str, &str, &[&str]); 6] = [
    ("pica.vert", "pica_vert.spv", &[]),
    ("pica_vs.vert", "pica_vs_vert.spv", &[]),
    ("pica.frag", "pica_frag.spv", &[]),
    // F4: fragmento sem gl_FragDepth (profundidade rápida na sombra N× e nos intermediários da interpolação)
    ("pica.frag", "pica_frag_fastz.spv", &["FAST_Z"]),
    ("present.comp", "present_comp.spv", &[]),
    ("fxaa.comp", "fxaa_comp.spv", &[]),
];

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
    println!("cargo:rerun-if-env-changed=YW_UPDATE_SPV");
    let tool = glslang();
    for (name, spv, defines) in SHADERS {
        let input = src.join(name);
        println!("cargo:rerun-if-changed={}", input.display());
        let dst = out.join(spv);
        match &tool {
            Some(t) => {
                let mut cmd = Command::new(t);
                cmd.arg("-V");
                for d in defines {
                    cmd.arg(format!("-D{d}"));
                }
                let o = cmd.arg(&input).arg("-o").arg(&dst).output().expect("glslangValidator");
                if !o.status.success() {
                    panic!(
                        "falha ao compilar {name} → {spv}:\n{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                }
                // Cópia pré-gerada (para quem compila sem o glslang): fica versionada em src/, então só é regravada
                // quando o shader mudou depois dela (ou com YW_UPDATE_SPV=1) e o conteúdo é diferente. Uma versão
                // diferente do glslang sozinha não suja a árvore do git.
                update_prebuilt(&dst, &pre.join(spv), &input);
            }
            None => copy_prebuilt(&pre.join(spv), &dst),
        }
    }
}

fn update_prebuilt(fresh: &Path, prebuilt: &Path, source: &Path) {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let forced = std::env::var_os("YW_UPDATE_SPV").is_some_and(|v| v == "1");
    let stale = match (mtime(source), mtime(prebuilt)) {
        (Some(s), Some(p)) => s > p,
        _ => true, // cópia ausente
    };
    if !(forced || stale) {
        return;
    }
    let Ok(new) = std::fs::read(fresh) else { return };
    if std::fs::read(prebuilt).is_ok_and(|old| old == new) {
        return;
    }
    if let Some(dir) = prebuilt.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(prebuilt, new);
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
