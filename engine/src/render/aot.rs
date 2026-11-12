//! Shaders de vértice AOT: os programas do PICA do próprio jogo traduzidos para GLSL pelo gerador
//! (`recomp/src/pica_glsl.rs`, a partir dos DVLB de `fix.xr`) e compilados para SPIR-V no build do executável.
//! O executável gerado registra aqui (`register_aot`) a lista de (registro, SPIR-V) antes de iniciar o jogo; o
//! backend Vulkan, a cada draw com o shader de vértice na GPU, procura um AOT para o programa carregado e, se achar,
//! usa o pipeline dele em vez do ubershader (`shaders/pica_vs.vert`).
//!
//! Registro (uma linha de texto por programa, escrita pelo gerador em `work/recomp/shaders/aot.txt`):
//! `name=SHD001.shdc#3 entry=0x69 vs=<hash do pica_vs.vert> mode=struct code=0069:4c000000,... desc=00:...,...`
//! - `code`/`desc`: TODAS as palavras de código (endereço:palavra) e descritores (índice:valor) de que a tradução
//!   depende — o que a execução pode alcançar a partir da entrada. O AOT só é usado se essas palavras forem iguais às
//!   que estão carregadas no PICA, então o resultado é o mesmo do ubershader por construção (o resto da memória de
//!   código, sobras de programas anteriores, não importa). Isso também confere se o que o jogo carrega bate com o
//!   `fix.xr`: programa sem AOT é registrado no log (uma vez cada).
//! - `vs`: hash do `pica_vs.vert` usado pelo gerador. Os AOT são esse arquivo com o interpretador trocado pelo
//!   programa; se o motor foi compilado com outra versão do arquivo, os AOT são recusados (regere).
//!
//! `YW_VK_NO_AOT=1` desliga (só ubershader).

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};

/// O ubershader de vértice com que este motor foi compilado (os AOT têm de ter vindo do mesmo texto).
const VS_SOURCE: &str = include_str!("shaders/pica_vs.vert");

/// FNV-1a de 64 bits (o gerador calcula o mesmo sobre o mesmo arquivo).
pub fn fnv1a64(b: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &x in b {
        h = (h ^ x as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

pub fn vs_source_hash() -> u64 {
    fnv1a64(VS_SOURCE.as_bytes())
}

/// Um programa traduzido.
pub struct AotProg {
    pub name: String,
    pub entry: u32,
    /// `straight` (sem fluxo), `struct` (IF/CALL viram if/else e blocos) ou `dispatch` (blocos + pilha)
    pub mode: String,
    code: Vec<(u16, u32)>,
    desc: Vec<(u8, u32)>,
    pub spirv: Vec<u32>,
}

impl AotProg {
    /// Quantas palavras de que a tradução depende diferem do estado carregado (0 = serve).
    fn diff(&self, code: &[u32], desc: &[u32]) -> usize {
        let c = self.code.iter().filter(|&&(a, w)| code.get(a as usize) != Some(&w)).count();
        let d = self.desc.iter().filter(|&&(i, w)| desc.get(i as usize) != Some(&w)).count();
        c + d
    }
}

/// Tabela registrada. `gen` muda a cada `register_aot` (o backend descarta módulos/pipelines da tabela anterior).
/// `hash` identifica o CONTEÚDO (os programas, na ordem): `PipeKey::aot` do Vulkan é um índice nesta tabela, então a
/// lista de pipelines salva (`vk_pipekeys_<hash>.bin`) só vale com a mesma tabela.
pub struct Table {
    pub gen: u64,
    pub progs: Vec<AotProg>,
    pub hash: u64,
}

/// Hash do conteúdo da tabela: tudo o que define cada programa (registro e SPIR-V), na ordem dos índices.
pub fn table_hash(progs: &[AotProg]) -> u64 {
    let mut b: Vec<u8> = Vec::new();
    for p in progs {
        b.extend_from_slice(p.name.as_bytes());
        b.push(0);
        b.extend_from_slice(&p.entry.to_le_bytes());
        b.extend_from_slice(p.mode.as_bytes());
        b.push(0);
        for &(a, w) in &p.code {
            b.extend_from_slice(&a.to_le_bytes());
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.push(0xff);
        for &(i, w) in &p.desc {
            b.push(i);
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.push(0xff);
        for w in &p.spirv {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b.extend_from_slice(&(p.spirv.len() as u64).to_le_bytes());
    }
    fnv1a64(&b)
}

static TABLE: RwLock<Option<Arc<Table>>> = RwLock::new(None);
static GEN: AtomicU64 = AtomicU64::new(0);
/// 0 = ainda não lido do ambiente, 1 = ligado, 2 = desligado
static ENABLED: AtomicU8 = AtomicU8::new(0);

/// Registra os shaders AOT: (registro, SPIR-V) por programa. Devolve quantos ficaram valendo. Substitui a tabela
/// anterior (testes registram mais de uma vez).
pub fn register_aot(list: &[(&str, &[u8])]) -> usize {
    let ours = vs_source_hash();
    let mut progs = Vec::new();
    let mut stale = 0usize;
    for (rec, spv) in list {
        match parse(rec, spv) {
            Ok((p, vs)) if vs == ours => progs.push(p),
            Ok(_) => stale += 1,
            Err(e) => eprintln!("[render] registro AOT inválido ({e}): {}", &rec[..rec.len().min(80)]),
        }
    }
    if stale > 0 {
        eprintln!(
            "[render] {stale} shaders AOT recusados: gerados de outra versão do pica_vs.vert (regere: cd recomp && cargo run --release, e recompile o executável)"
        );
    }
    let n = progs.len();
    let mut modes: Vec<(String, usize)> = Vec::new();
    for p in &progs {
        match modes.iter_mut().find(|(m, _)| *m == p.mode) {
            Some((_, k)) => *k += 1,
            None => modes.push((p.mode.clone(), 1)),
        }
    }
    let modes: Vec<String> = modes.iter().map(|(m, k)| format!("{k} {m}")).collect();
    eprintln!(
        "[render] {n} shaders de vértice AOT registrados ({}){}",
        if modes.is_empty() { "nenhum".to_string() } else { modes.join(", ") },
        if enabled() { "" } else { " — desligados por YW_VK_NO_AOT" }
    );
    if list.is_empty() {
        eprintln!(
            "[render] executável sem shaders AOT (gerador sem o jogo ou build sem o glslangValidator): regere (cd recomp && cargo run --release) e recompile com o glslang — até lá, só o ubershader"
        );
    }
    let gen = GEN.fetch_add(1, Ordering::Relaxed) + 1;
    let hash = table_hash(&progs);
    *TABLE.write().unwrap() = Some(Arc::new(Table { gen, progs, hash }));
    n
}

/// Tabela atual (None = nada registrado).
pub fn table() -> Option<Arc<Table>> {
    TABLE.read().unwrap().clone()
}

/// Liga/desliga os AOT (o mesmo que `YW_VK_NO_AOT`; testes comparam com o ubershader).
pub fn set_enabled(on: bool) {
    ENABLED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = !std::env::var("YW_VK_NO_AOT").is_ok_and(|v| v != "0");
            set_enabled(on);
            on
        }
    }
}

/// Resultado da procura: o índice do AOT, ou o mais próximo (para o log) quando nenhum serve.
pub enum Found {
    Aot(usize),
    /// nenhum com esta entrada / o mais próximo com esta entrada e quantas palavras diferem
    Miss(Option<(usize, usize)>),
}

/// Procura um AOT cujas palavras de código/descritores batem com o estado carregado.
pub fn find(t: &Table, code: &[u32], desc: &[u32], entry: u32) -> Found {
    let mut best: Option<(usize, usize)> = None;
    for (i, p) in t.progs.iter().enumerate() {
        if p.entry != entry {
            continue;
        }
        let d = p.diff(code, desc);
        if d == 0 {
            return Found::Aot(i);
        }
        if best.is_none_or(|(_, b)| d < b) {
            best = Some((i, d));
        }
    }
    Found::Miss(best)
}

fn hex(s: &str) -> Result<u32, String> {
    u32::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| format!("{s}: {e}"))
}

fn pairs(s: &str) -> Result<Vec<(u32, u32)>, String> {
    s.split(',')
        .filter(|x| !x.is_empty())
        .map(|x| {
            let (a, w) = x.split_once(':').ok_or_else(|| format!("par sem ':' ({x})"))?;
            Ok((hex(a)?, hex(w)?))
        })
        .collect()
}

fn parse(rec: &str, spv: &[u8]) -> Result<(AotProg, u64), String> {
    let mut name = None;
    let mut entry = None;
    let mut vs = None;
    let mut mode = String::from("?");
    let mut code = Vec::new();
    let mut desc = Vec::new();
    for f in rec.split_whitespace() {
        let (k, v) = f.split_once('=').ok_or_else(|| format!("campo sem '=' ({f})"))?;
        match k {
            "name" => name = Some(v.to_string()),
            "entry" => entry = Some(hex(v)?),
            "vs" => vs = Some(u64::from_str_radix(v, 16).map_err(|e| e.to_string())?),
            "mode" => mode = v.to_string(),
            "code" => {
                code = pairs(v)?
                    .into_iter()
                    .map(|(a, w)| u16::try_from(a).ok().filter(|&a| a < 4096).map(|a| (a, w)).ok_or("endereço ≥ 4096"))
                    .collect::<Result<_, _>>()?
            }
            "desc" => {
                desc = pairs(v)?
                    .into_iter()
                    .map(|(i, w)| u8::try_from(i).ok().filter(|&i| i < 128).map(|i| (i, w)).ok_or("descritor ≥ 128"))
                    .collect::<Result<_, _>>()?
            }
            _ => {}
        }
    }
    if spv.len() < 20 || spv.len() % 4 != 0 {
        return Err("SPIR-V vazio ou desalinhado".into());
    }
    let spirv: Vec<u32> = spv.chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    if spirv[0] != 0x0723_0203 {
        return Err("SPIR-V sem o número mágico".into());
    }
    if code.is_empty() {
        return Err("sem palavras de código".into());
    }
    let p = AotProg {
        name: name.ok_or("sem name")?,
        entry: entry.ok_or("sem entry")?,
        mode,
        code,
        desc,
        spirv,
    };
    Ok((p, vs.ok_or("sem vs")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spv() -> Vec<u8> {
        let mut v = 0x0723_0203u32.to_le_bytes().to_vec();
        v.resize(20, 0);
        v
    }

    #[test]
    fn parse_and_find() {
        let rec = format!(
            "name=T#0 entry=0x2 vs={:016x} mode=straight code=0002:88000000,0003:88000000 desc=05:0000000f",
            vs_source_hash()
        );
        let (p, vs) = parse(&rec, &spv()).unwrap();
        assert_eq!(vs, vs_source_hash());
        assert_eq!((p.entry, p.code.len(), p.desc.len()), (2, 2, 1));
        let t = Table { gen: 1, hash: 0, progs: vec![p] };
        let mut code = vec![0u32; 4096];
        let mut desc = vec![0u32; 128];
        code[2] = 0x8800_0000;
        code[3] = 0x8800_0000;
        code[100] = 0xdead_beef; // sobra de outro programa: não importa
        desc[5] = 0xf;
        assert!(matches!(find(&t, &code, &desc, 2), Found::Aot(0)));
        assert!(matches!(find(&t, &code, &desc, 3), Found::Miss(None)));
        desc[5] = 0xe;
        assert!(matches!(find(&t, &code, &desc, 2), Found::Miss(Some((0, 1)))));
    }
}
