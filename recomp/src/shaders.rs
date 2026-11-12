//! Shaders de vértice AOT: extrai os programas do PICA do jogo (`fix.xr` do `yw1_a.fa`: `SHD000..003.shdc`, um DVLB
//! cada, 21 pontos de entrada), traduz cada (código, descritores, entrada) para GLSL (`pica_glsl.rs`) e escreve em
//! `work/recomp/shaders/`:
//! - `aot_NN.vert` — um por programa; o `build.rs` do executável compila para SPIR-V e embute;
//! - `aot.txt` — o registro de cada programa (`render::aot` no motor): entrada, modo, arquivo e as palavras de código
//!   e descritores de que a tradução depende;
//! - `aot_NN_d.vert` + `aot_dispatch.txt` — a mesma tradução no modo `dispatch` (blocos + pilha) para os programas
//!   com fluxo: não entram no executável; o teste `pica_frame_vulkan` compara os dois modos.
//!
//! Tudo isso é derivado do jogo do usuário → só em `work/`. Sem o jogo, escreve registros vazios e o executável fica
//! só com o ubershader.
//!
//! Formato (DVLB/DVLP/DVLE do SDK, como o 3dbrew descreve "SHBIN"), conferido contra dados reais: o código e os
//! descritores do DVLP de `SHD001.shdc` são idênticos aos do programa capturado no Azahar (`data/gpu/`).
//! - DVLB: "DVLB", nº de DVLE, deslocamentos dos DVLE; o DVLP vem logo depois.
//! - DVLP: +0x08 deslocamento do código (palavras em +0x0C), +0x10 dos descritores (+0x14 entradas de 8 bytes; a
//!   1ª palavra é o descritor).
//! - DVLE: +0x06 tipo (0 = vértice), +0x08 entrada, +0x0C fim, +0x18/+0x1C constantes (20 bytes), +0x28/+0x2C mapa de
//!   saídas (8 bytes: tipo, registrador, máscara).

use crate::fa;
use crate::pica_glsl::{self, Mode};
use crate::write_if_changed;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// caminho sem `..` para o log (e sem o prefixo `\\?\` do Windows)
fn clean(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .map(|c| PathBuf::from(c.display().to_string().trim_start_matches(r"\\?\")))
        .unwrap_or(p.to_path_buf())
}

/// o ubershader do motor: cada AOT é ele com o interpretador trocado pelo programa
const TEMPLATE: &str = include_str!("../../engine/src/render/shaders/pica_vs.vert");
const DVLBS: [&str; 4] = ["SHD000.shdc", "SHD001.shdc", "SHD002.shdc", "SHD003.shdc"];

/// FNV-1a de 64 bits (o motor calcula o mesmo em `render::aot::fnv1a64`)
fn fnv1a64(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn rd(d: &[u8], o: usize) -> Result<u32, String> {
    fa::u32le(d, o)
}

fn f24(v: u32) -> f32 {
    let v = v & 0xff_ffff;
    if v & 0x7f_ffff == 0 {
        return if v >> 23 != 0 { -0.0 } else { 0.0 };
    }
    f32::from_bits((v >> 23 & 1) << 31 | (((v >> 16) & 0x7f) + 64) << 23 | (v & 0xffff) << 7)
}

struct Dvle {
    entry: u32,
    end: u32,
    /// descrição das constantes e do mapa de saídas (só para o cabeçalho do .vert)
    consts: Vec<String>,
    outs: Vec<String>,
}

struct Dvlb {
    code: Vec<u32>,
    desc: Vec<u32>,
    dvles: Vec<Dvle>,
}

fn parse_dvlb(d: &[u8]) -> Result<Dvlb, String> {
    if d.get(0..4) != Some(b"DVLB".as_slice()) {
        return Err("não é DVLB".into());
    }
    let n = rd(d, 4)? as usize;
    if n == 0 || n > 64 {
        return Err(format!("{n} DVLE?"));
    }
    let p = 8 + 4 * n;
    if d.get(p..p + 4) != Some(b"DVLP".as_slice()) {
        return Err("DVLP ausente".into());
    }
    let (co, cn, oo, on) = (rd(d, p + 8)? as usize, rd(d, p + 12)? as usize, rd(d, p + 16)? as usize, rd(d, p + 20)? as usize);
    if cn > pica_glsl::CODE_WORDS as usize || on > 128 {
        return Err(format!("DVLP com {cn} palavras / {on} descritores"));
    }
    let code = (0..cn).map(|i| rd(d, p + co + 4 * i)).collect::<Result<Vec<_>, _>>()?;
    let desc = (0..on).map(|i| rd(d, p + oo + 8 * i)).collect::<Result<Vec<_>, _>>()?;
    let mut dvles = Vec::new();
    for i in 0..n {
        let o = rd(d, 8 + 4 * i)? as usize;
        if d.get(o..o + 4) != Some(b"DVLE".as_slice()) {
            return Err(format!("DVLE {i} sem assinatura"));
        }
        if d[o + 6] != 0 {
            continue; // shader de geometria: o jogo não usa
        }
        let (k_off, k_n) = (rd(d, o + 0x18)? as usize, rd(d, o + 0x1c)? as usize);
        let (s_off, s_n) = (rd(d, o + 0x28)? as usize, rd(d, o + 0x2c)? as usize);
        let mut consts = Vec::new();
        for j in 0..k_n.min(96) {
            let c = o + k_off + 20 * j;
            let (ty, id) = (*d.get(c).ok_or("constante truncada")?, d[c + 2]);
            consts.push(match ty {
                0 => format!("b{id} = {}", d[c + 4]),
                1 => format!("i{id} = ({}, {}, {}, {})", d[c + 4], d[c + 5], d[c + 6], d[c + 7]),
                _ => {
                    let v: Vec<String> = (0..4).map(|k| rd(d, c + 4 + 4 * k).map(|x| format!("{}", f24(x)))).collect::<Result<_, _>>()?;
                    format!("c{id} = ({})", v.join(", "))
                }
            });
        }
        let names = ["pos", "quat", "cor", "tc0", "tc0w", "tc1", "tc2", "?7", "vista"];
        let mut outs = Vec::new();
        for j in 0..s_n.min(16) {
            let e = o + s_off + 8 * j;
            let (ty, reg, mask) = (fa::u16le(d, e)?, fa::u16le(d, e + 2)?, fa::u16le(d, e + 4)?);
            let comps: String = (0..4).filter(|k| mask >> k & 1 != 0).map(|k| b"xyzw"[k] as char).collect();
            outs.push(format!("o{reg} {} ({comps})", names.get(ty as usize).unwrap_or(&"?")));
        }
        dvles.push(Dvle { entry: rd(d, o + 8)?, end: rd(d, o + 0xc)?, consts, outs });
    }
    Ok(Dvlb { code, desc, dvles })
}

/// Gera os shaders AOT em `out/shaders`. `root` = `yokai-port/`. Nunca falha o gerador: sem o jogo (ou com erro),
/// os registros ficam vazios e o executável usa só o ubershader.
pub fn generate(root: &Path, out: &Path) {
    let dir = out.join("shaders");
    std::fs::create_dir_all(&dir).unwrap();
    let vs_hash = fnv1a64(TEMPLATE.as_bytes());
    let mut main_recs = String::from("# Gerado por yw1-recomp (recomp/src/shaders.rs): shaders de vértice AOT do executável. Derivado do jogo: não versionar.\n");
    let mut alt_recs = String::from("# Gerado por yw1-recomp: variantes no modo dispatch (só para o teste pica_frame_vulkan).\n");
    let t0 = std::time::Instant::now();
    match build(root, &dir, vs_hash, &mut main_recs, &mut alt_recs) {
        Ok(msg) => println!("shaders AOT: {msg} em {:.1}s → {}", t0.elapsed().as_secs_f64(), clean(&dir).display()),
        Err(e) => {
            println!("shaders AOT: nenhum ({e}); o executável fica só com o ubershader de vértice");
            main_recs.truncate(main_recs.find('\n').unwrap() + 1);
            alt_recs.truncate(alt_recs.find('\n').unwrap() + 1);
        }
    }
    write_if_changed(dir.join("aot.txt"), main_recs).unwrap();
    write_if_changed(dir.join("aot_dispatch.txt"), alt_recs).unwrap();
}

fn record(name: &str, entry: u32, vs: u64, mode: Mode, file: &str, t: &pica_glsl::Translation) -> String {
    let code: Vec<String> = t.code_deps.iter().map(|(a, w)| format!("{a:03x}:{w:08x}")).collect();
    let desc: Vec<String> = t.desc_deps.iter().map(|(i, w)| format!("{i:02x}:{w:08x}")).collect();
    format!(
        "name={name} entry={entry:#x} vs={vs:016x} mode={} file={file} code={} desc={}\n",
        mode.name(),
        code.join(","),
        desc.join(",")
    )
}

fn build(root: &Path, dir: &Path, vs: u64, recs: &mut String, alts: &mut String) -> Result<String, String> {
    let base = clean(&root.join(".."));
    let (xr, src) = fa::read_game_file(&base, "fix.xr")?;
    let pack = fa::xpck::Pack::parse(&xr)?;
    // capturas do Azahar (data/gpu): o programa do quadro do quarto, para conferir o formato contra dado real
    let cap_code = std::fs::read(root.join("data/gpu/shader_code.bin")).ok();
    let cap_desc = std::fs::read(root.join("data/gpu/shader_opdescs.bin")).ok();
    let words = |b: &[u8]| b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect::<Vec<u32>>();
    let mut seen: Vec<(u32, String)> = Vec::new();
    let (mut n, mut n_mode, mut n_ins, mut dups) = (0usize, [0usize; 3], 0usize, 0usize);
    let mut notes = Vec::new();
    for f in DVLBS {
        let raw = pack.get(f)?;
        let data = if raw.starts_with(b"DVLB") { raw.to_vec() } else { fa::l5::decompress(raw)? };
        let b = parse_dvlb(&data).map_err(|e| format!("{f}: {e}"))?;
        if let (Some(c), Some(d)) = (&cap_code, &cap_desc) {
            if words(c) == b.code && words(d) == b.desc {
                notes.push(format!("{f} = programa capturado no Azahar (data/gpu)"));
            }
        }
        let mut code = b.code.clone();
        code.resize(pica_glsl::CODE_WORDS as usize, 0);
        let mut desc = b.desc.clone();
        desc.resize(128, 0);
        for (i, e) in b.dvles.iter().enumerate() {
            let name = format!("{f}#{i}");
            let t = pica_glsl::translate(&code, &desc, e.entry, false).map_err(|x| format!("{name}: {x}"))?;
            let key = format!("{:?}{:?}", t.code_deps, t.desc_deps);
            if seen.iter().any(|(en, k)| *en == e.entry && *k == key) {
                dups += 1;
                continue;
            }
            seen.push((e.entry, key));
            let stem = format!("aot_{n:02}");
            let mut hdr = String::new();
            let _ = writeln!(
                hdr,
                "// GERADO por yw1-recomp a partir de fix.xr/{f}, DVLE {i} (entrada {:#05x}, fim {:#05x}). Derivado do jogo do\n// usuário: fica em work/, NUNCA versionar.",
                e.entry, e.end
            );
            let _ = writeln!(hdr, "// modo {} · {} instruções traduzidas", t.mode.name(), t.instructions);
            hdr.push_str("// O resto é o ubershader engine/src/render/shaders/pica_vs.vert com o interpretador (@@AOT) trocado pelo programa.
");
            let _ = writeln!(hdr, "// saídas do DVLE: {}", e.outs.join(" · "));
            if !e.consts.is_empty() {
                let _ = writeln!(hdr, "// constantes do DVLE (o jogo as envia como uniforms): {}", e.consts.join(" · "));
            }
            let glsl = pica_glsl::splice(TEMPLATE, &t.region, &hdr)?;
            write_if_changed(dir.join(format!("{stem}.vert")), glsl).unwrap();
            recs.push_str(&record(&name, e.entry, vs, t.mode, &stem, &t));
            n_mode[t.mode as usize] += 1;
            n_ins += t.instructions;
            if t.mode != Mode::Straight {
                let td = pica_glsl::translate(&code, &desc, e.entry, true).map_err(|x| format!("{name} (dispatch): {x}"))?;
                let dstem = format!("{stem}_d");
                let glsl = pica_glsl::splice(TEMPLATE, &td.region, &hdr.replacen("// modo", "// VARIANTE DE TESTE no modo dispatch; o executável usa o modo", 1))?;
                write_if_changed(dir.join(format!("{dstem}.vert")), glsl).unwrap();
                alts.push_str(&record(&name, e.entry, vs, td.mode, &dstem, &td));
            }
            n += 1;
        }
    }
    let mut msg = format!(
        "{n} programas ({} retos, {} com blocos if/else, {} com despacho; {n_ins} instruções) de {} DVLB em {src}",
        n_mode[0],
        n_mode[1],
        n_mode[2],
        DVLBS.len()
    );
    if dups > 0 {
        let _ = write!(msg, ", {dups} repetidos omitidos");
    }
    for x in notes {
        let _ = write!(msg, "; {x}");
    }
    Ok(msg)
}
