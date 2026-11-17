//! Etiquetas de draw (fase G1): cada draw da cena 3D ganha uma identidade vinda do JOGO — o nó (matriz de modelo) de
//! origem, a malha/primitiva, a ocorrência e o caminho de desenho — para a interpolação de quadros parear draws por
//! identidade em vez da heurística estrutural (`render/vulkan/interp.rs`), e o corte de câmera vem de um gancho na câmera.
//!
//! **O que se confirmou** (fase G1, amostras com `YW_ETIQ_LOG=2` na rota novo jogo → quarto):
//! - `FUN_00317030(ctx, malha, prim, n)` = o draw do jogo (quase todos os draws do PICA na rota). Escreve o bloco do
//!   draw (`FUN_0012f868` estado + `FUN_0012fb34` 0x1c palavras com o disparo 0x22F) no CURSOR da lista de comandos do
//!   nngx, a variável global 0x5c7a28 (`DAT_005c7a28`, literal em 0x317128; o bind da lista põe nela a base do buffer e
//!   o flush grava `cursor − base` como tamanho usado, `code.bin.c` ~34488/34644). Ganchos "antes"/"depois" leem o
//!   cursor: a faixa `[antes, depois)` (0x70–0xc8 bytes) contém o disparo. Retorno 0 = não desenhou.
//! - O laço de objetos `FUN_00325cf8` só ENFILEIRA: nenhum draw acontece entre o `blx` do desenho do objeto e o passo
//!   seguinte (medido: 37 mil objetos, 0 draws dentro). Os draws saem depois, da fila de comandos do dispositivo
//!   (`FUN_001c9508`, "lxpCOpenGLES"), sem o objeto. A identidade vem do estado do dispositivo no draw: a matriz de
//!   modelo do último `FUN_003166d4(ctx, matriz)`. Nos cenários (86 % dos draws amostrados), `matriz` aponta para a
//!   matriz do nó no heap — estável e única por instância. Nos personagens com esqueleto a matriz é calculada na pilha
//!   (`FUN_001c9648(ctx, M)` = modelo × M): usa-se o `M` dele quando não está na pilha; senão o nó fica 0 e a
//!   identidade é (malha, primitiva, caminho) + ordem.
//! - `FUN_00325c18(_, câmera, olho)` = envio da câmera ao renderizador, 1 por quadro (view 3×4 em câmera+0x44 no modo
//!   0/1, +0x130 no 2, +0x21c no 3 — `FUN_003140c0` → ctx+0x184). Compara a view com o envio anterior da MESMA
//!   câmera; salto acima do limiar ou câmera nova incrementa a GERAÇÃO de corte, que vai em cada etiqueta.
//!
//! **Transporte** (sem atômico entre threads): as etiquetas ficam num anel do lado do jogo com a faixa de endereços do
//! bloco; ao despachar a lista (`gsp`, `GxCmd::List(addr, size)`, na thread do jogo) as da faixa `[addr, addr+size)`
//! saem do anel e viajam junto com as palavras copiadas. A thread "gpu" (`pica::execute_words_etiq`) acha os disparos
//! de draw da lista, confere que cada etiqueta cai em exatamente um disparo (senão descarta as da lista e avisa uma
//! vez) e deixa a etiqueta do draw corrente em [`draw_atual`] (thread-local da própria thread "gpu") enquanto ele é
//! desenhado; `Vk::draw` a repassa ao `record` da interpolação.
//!
//! Draws sem matriz de modelo desde o envio da câmera ficam sem etiqueta (heurística).
//! `YW_INTERP_TAGS=0` desliga tudo (nenhum gancho; heurística antiga). `YW_ETIQ_LOG=1|2`: diagnóstico no log.

use super::hooks::{self, IdGancho, Tipo};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use yw1_rt::r32;

/// Endereço da variável do cursor de escrita da lista de comandos ativa (`DAT_005c7a28`).
const CURSOR: u32 = 0x005c_7a28;
/// Draw do jogo, matriz de modelo, modelo × M (esqueleto), envio da câmera.
const F_DRAW: u32 = 0x0031_7030;
const F_MATRIZ: u32 = 0x0031_66d4;
/// `FUN_001c9648` (gancho em instrução, `recomp/hooks.toml`)
const AT_MULT: u32 = 0x001c_9648;
const F_CAMERA: u32 = 0x0032_5c18;
/// Matriz na pilha (a menos disto acima do sp na entrada de `FUN_003166d4`): calculada, não identifica nada.
const PILHA: u32 = 0x4000;
/// Tamanho do bloco que `FUN_0012fb34` escreve (0x1c palavras): a faixa de um draw nunca é menor.
const BLOCO_DRAW: u32 = 0x70;

/// Identidade de um draw dada pelo jogo. Iguais em dois quadros = o mesmo desenho (o par da interpolação; repetidas
/// no mesmo quadro pareiam em ordem).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Etiqueta {
    /// nó de origem: ponteiro guest da matriz de modelo (`FUN_003166d4`, ou o `M` de `FUN_001c9648`); 0 = calculada
    /// na pilha (sem identidade de instância)
    pub obj: u32,
    /// r1 de `FUN_00317030` (cabeçalho da malha: `malha+0x6c` = primitiva nos cenários)
    pub malha: u32,
    /// primitiva desenhada (r2 de `FUN_00317030`; `prim+0x10` = índices)
    pub prim: u32,
    /// draws desde a última matriz de modelo (0, 1, …: primitivas do mesmo nó)
    pub sub: u16,
    /// caminho de desenho: lr de quem pôs a matriz (cenário 0x32547c, esqueleto 0x1c9674…)
    pub origem: u32,
    /// reservado para a G5 (HUD ancorada)
    pub ancora: u8,
}

/// Etiqueta + geração de corte de câmera no momento do draw (não faz parte da identidade).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EtiqDraw {
    pub et: Etiqueta,
    pub corte: u32,
    /// grupo de HUD do draw (G5, `hle/hud.rs`; 0 = sem grupo). Não faz parte da identidade.
    pub grupo: u32,
    /// draw sem identidade (sem matriz de modelo no quadro): viaja só pelo `grupo` da HUD; a interpolação o trata
    /// como sem etiqueta
    pub sem_id: bool,
}

/// Etiquetas de uma lista despachada: faixas `[ini, fim)` (endereços guest) em ordem de endereço.
#[derive(Clone, Debug, Default)]
pub struct EtiqLista {
    pub itens: Vec<(u32, u32, EtiqDraw)>,
}

/// `YW_INTERP_TAGS=0`: sem etiquetas (heurística antiga, nenhum gancho).
pub fn ligadas() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_INTERP_TAGS").map_or(true, |v| v != "0"))
}

pub fn log_on() -> bool {
    nivel_log() > 0
}

/// `YW_ETIQ_LOG=2`: além do diagnóstico, amostra todos os draws de um quadro a cada 600 VBlanks (de onde vieram).
fn nivel_log() -> u32 {
    static E: OnceLock<u32> = OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_ETIQ_LOG").ok().and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// Limiares do corte de câmera entre dois envios seguidos da mesma câmera: o olho pode andar `base + vel × VBlanks`
/// (unidades do mundo; o jogo é por quadro e, lento, avança vários VBlanks de lógica por desenho) e a view girar até
/// `graus`. Medido (G1): caminhada em Uptown, olho até 2,0 unidades/VBlank e giro
/// 0 (câmera do campo de ângulo fixo; ruído 0,02–0,05°); voo da cena de abertura do prólogo até 11,1 unidades e
/// 0,42° por quadro (2 VBlanks; limite 20 unidades). Um corte falso só deixa aquele quadro sem intermediários (o
/// comportamento antigo); um corte perdido interpola entre duas tomadas. `YW_ETIQ_CORTE=base,vel,graus`.
fn limiares() -> (f32, f32, f32) {
    static L: OnceLock<(f32, f32, f32)> = OnceLock::new();
    *L.get_or_init(|| {
        std::env::var("YW_ETIQ_CORTE")
            .ok()
            .and_then(|v| {
                let p: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (p.len() == 3).then(|| (p[0], p[1], p[2]))
            })
            .unwrap_or((LIMIAR_BASE, LIMIAR_VEL, LIMIAR_GRAUS))
    })
}
const LIMIAR_BASE: f32 = 12.0;
const LIMIAR_VEL: f32 = 4.0;
const LIMIAR_GRAUS: f32 = 20.0;
/// Câmera que não estava entre os últimos envios (de qualquer câmera) e volta = câmera nova (troca de cena/evento) →
/// corte. Contado em envios, não em tempo: com o jogo lento (carga, máquina ocupada) os envios se espaçam (medido: até
/// 25 VBlanks entre dois com a CPU disputada) e um limite em VBlanks cortaria à toa. Medido: 1 envio por quadro.
const CAMERA_NOVA_ENVIOS: u64 = 4;

/// Estado do lado do jogo.
#[derive(Default)]
struct Jogo {
    /// nó e caminho da última matriz de modelo (None até a 1ª do quadro) e draws desde ela
    no: Option<(u32, u32)>,
    sub: u16,
    /// `M` do último `FUN_001c9648` (modelo × M), para a matriz seguinte que vier da pilha
    mult: u32,
    /// cursor da lista na entrada do draw em andamento
    cursor_antes: u32,
    /// faixas etiquetadas ainda não despachadas, em ordem de escrita
    anel: VecDeque<(u32, u32, EtiqDraw)>,
    /// (câmera, deslocamento da view do olho do estéreo) → (nº do último envio, VBlank dele, olho, eixos da view)
    cams: HashMap<(u32, u32), (u64, u64, [f32; 3], [f32; 9])>,
    /// envios de câmera até agora (qualquer câmera)
    envios: u64,
    /// diagnóstico (`YW_ETIQ_LOG=2`): última matriz de modelo crua (r1 de FUN_003166d4); quadros a amostrar
    mtx: u32,
    amostra: u32,
}

fn jogo() -> MutexGuard<'static, Jogo> {
    static J: OnceLock<Mutex<Jogo>> = OnceLock::new();
    J.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Geração de corte de câmera (incrementa a cada corte detectado).
static CORTE: AtomicU32 = AtomicU32::new(0);

/// Contadores de diagnóstico (lidos pela linha `[interp]`).
pub static N_ETIQ: AtomicU64 = AtomicU64::new(0); // etiquetas gravadas pelo jogo
pub static N_SEM_OBJ: AtomicU64 = AtomicU64::new(0); // draws do jogo sem etiqueta (sem matriz de modelo no quadro)
pub static N_PILHA: AtomicU64 = AtomicU64::new(0); // etiquetas com nó 0 (matriz calculada na pilha)
pub static N_DESCARTADAS: AtomicU64 = AtomicU64::new(0); // etiquetas que saíram do anel sem lista (velhas/sobrescritas)
pub static N_CASADAS: AtomicU64 = AtomicU64::new(0); // etiquetas que caíram em exatamente 1 disparo de draw
pub static N_LISTAS_RUINS: AtomicU64 = AtomicU64::new(0); // listas cujas etiquetas foram ignoradas
pub static N_CORTES_CAM: AtomicU64 = AtomicU64::new(0); // cortes de câmera detectados (lado do jogo)

/// Ganchos registrados (vazio = desligado).
static GANCHOS: Mutex<Vec<IdGancho>> = Mutex::new(Vec::new());
static ATIVO: AtomicBool = AtomicBool::new(false);

/// Liga/desliga os ganchos conforme a captura da interpolação (chamado a cada lista despachada, na thread do jogo):
/// sem interpolação nem estatística, o jogo roda sem ganchos (custo zero).
pub fn sincronizar() {
    // a HUD ancorada (G5) também usa o transporte (o grupo de cada draw)
    let quer = (ligadas() && crate::render::vulkan::interp_capturing()) || super::widescreen::hud::quer();
    if quer == ATIVO.load(Relaxed) {
        return;
    }
    let mut g = GANCHOS.lock().unwrap_or_else(|e| e.into_inner());
    if quer {
        if super::widescreen::hud::quer() {
            super::widescreen::hud::registrar();
        }
        match registrar() {
            Ok(v) => *g = v,
            Err(e) => {
                eprintln!("[etiquetas] ganchos não registrados ({e}): interpolação fica na heurística");
                for id in g.drain(..) {
                    hooks::remover(id);
                }
                // não tenta de novo a cada lista
                ATIVO.store(true, Relaxed);
                return;
            }
        }
    } else {
        for id in g.drain(..) {
            hooks::remover(id);
        }
        let mut j = jogo();
        j.anel.clear();
        j.no = None;
    }
    ATIVO.store(quer, Relaxed);
    if log_on() {
        eprintln!("[etiquetas] ganchos {}", if quer { "ligados" } else { "desligados" });
    }
}

fn registrar() -> Result<Vec<IdGancho>, String> {
    let mut ids = Vec::new();
    let mut r = |x: Result<IdGancho, String>| -> Result<(), String> {
        ids.push(x?);
        Ok(())
    };
    // matriz de modelo: FUN_003166d4(ctx, matriz)
    r(hooks::registrar(
        F_MATRIZ,
        Tipo::antes(|c| {
            let (m, sp) = (c.r[1], c.r[13]);
            let mut j = jogo();
            j.mtx = m;
            let mult = std::mem::take(&mut j.mult);
            let no = if m.wrapping_sub(sp) >= PILHA {
                m
            } else if mult != 0 && mult.wrapping_sub(sp) >= PILHA {
                mult
            } else {
                0
            };
            j.no = Some((no, c.r[14]));
            j.sub = 0;
        }),
        "etiq: matriz de modelo (FUN_003166d4)",
        0,
        false,
    ))?;
    // FUN_001c9648 fica numa região densa (não é entrada de função recompilada): gancho em instrução (hooks.toml)
    r(hooks::registrar_em(AT_MULT, "etiq: modelo × M (0x1c9648)", 0, false, |c| jogo().mult = c.r[1]))?;
    // o draw: cursor antes e depois
    r(hooks::registrar(F_DRAW, Tipo::antes(|_| jogo().cursor_antes = r32(CURSOR)), "etiq: draw (cursor antes)", 0, false))?;
    r(hooks::registrar(F_DRAW, Tipo::depois(depois_draw), "etiq: draw (FUN_00317030)", 0, false))?;
    // câmera enviada ao renderizador
    r(hooks::registrar(F_CAMERA, Tipo::antes(camera), "etiq: câmera (FUN_00325c18)", 0, false))?;
    Ok(ids)
}

fn depois_draw(c: &mut yw1_rt::Cpu, e: &hooks::Entrada) {
    if c.r[0] == 0 {
        return; // não desenhou
    }
    let fim = r32(CURSOR);
    let mut j = jogo();
    let ini = j.cursor_antes;
    if j.amostra > 0 {
        eprintln!(
            "[etiq-amostra] draw lr={:#x} r1={:#x} r2={:#x} n={} · matriz {:#x} → nó {:x?} sub {} · faixa {:#x}+{:#x}",
            e.lr, e.r[1], e.r[2], e.r[3], j.mtx, j.no, j.sub, ini, fim.wrapping_sub(ini)
        );
    }
    let hud = super::widescreen::hud::quer();
    if fim.wrapping_sub(ini) < BLOCO_DRAW || fim.wrapping_sub(ini) > 0x1_0000 {
        // cursor não andou como o bloco de draw (outro buffer?): sem etiqueta [não visto]
        N_SEM_OBJ.fetch_add(1, Relaxed);
        return;
    }
    // grupo de HUD (G5): item da camada em desenho + matriz de modelo (`hle/hud.rs`)
    let grupo = if hud { super::widescreen::hud::grupo(e, j.no.map_or(0, |n| n.0)) } else { 0 };
    let (obj, origem, sem_id) = match j.no {
        Some((o, g)) => (o, g, false),
        None => {
            N_SEM_OBJ.fetch_add(1, Relaxed);
            if grupo == 0 {
                return;
            }
            (0, 0, true)
        }
    };
    let sub = j.sub;
    if !sem_id {
        j.sub = j.sub.saturating_add(1);
    }
    if obj == 0 && !sem_id {
        N_PILHA.fetch_add(1, Relaxed);
    }
    let et = Etiqueta { obj, malha: e.r[1], prim: e.r[2], sub, origem, ancora: 0 };
    // buffer reaproveitado (o cursor voltou para trás de uma faixa pendente): as pendentes que se sobrepõem são de uma
    // lista que nunca foi despachada
    if j.anel.back().is_some_and(|b| ini < b.1) {
        let antes = j.anel.len();
        j.anel.retain(|x| x.1 <= ini || x.0 >= fim);
        N_DESCARTADAS.fetch_add((antes - j.anel.len()) as u64, Relaxed);
    }
    if j.anel.len() >= 16384 {
        j.anel.pop_front();
        N_DESCARTADAS.fetch_add(1, Relaxed);
    }
    j.anel.push_back((ini, fim, EtiqDraw { et, corte: CORTE.load(Relaxed), grupo, sem_id }));
    N_ETIQ.fetch_add(1, Relaxed);
}

/// `FUN_00325c18(_, câmera, olho)`: compara a view enviada com o envio anterior da mesma câmera.
fn camera(c: &mut yw1_rt::Cpu) {
    let cam = c.r[1];
    if cam == 0 || !super::mem::is_mapped(cam) || yw1_rt::r8(cam + 0x2d0) > 1 {
        return; // a função não envia nada nesses casos
    }
    let off = match c.r[2] {
        2 => 0x130,
        3 => 0x21c,
        _ => 0x44,
    };
    let v = cam + off;
    // view 3×4 (12 floats, copiada por FUN_00320404 para ctx+0x184): os três eixos da câmera (3 floats cada, montados
    // assim em FUN_002cc0bc) e a translação. Visto: [1,0,0 | 0,.707,−.707 | 0,.707,.707 | 611.9, 81.3, −451.6].
    // Giro = ângulo de R₀ᵀR₁ (não depende de os eixos serem linhas ou colunas); olho = −Σ tₖ·eixoₖ [H: convenção]
    let f = |i: u32| f32::from_bits(r32(v + 4 * i));
    let eixos: [f32; 9] = std::array::from_fn(|i| f(i as u32));
    let t = [f(9), f(10), f(11)];
    let olho: [f32; 3] = std::array::from_fn(|c| -(t[0] * eixos[c] + t[1] * eixos[3 + c] + t[2] * eixos[6 + c]));
    if !olho.iter().chain(eixos.iter()).all(|x| x.is_finite()) {
        return;
    }
    let q = super::VBLANKS.load(Relaxed);
    let mut j = jogo();
    j.envios += 1;
    // começo do quadro (do olho): draws antes da 1ª matriz de modelo ficam sem etiqueta
    j.no = None;
    if nivel_log() >= 2 {
        j.amostra = j.amostra.saturating_sub(1);
        if q % 600 < 3 && j.amostra == 0 && q > 60 {
            j.amostra = 2; // o próximo quadro inteiro (até o envio seguinte de câmera)
            eprintln!("[etiq-amostra] ---- quadro q={q} câmera {cam:#x}");
        }
    }
    let e = j.envios;
    let ant = j.cams.insert((cam, off), (e, q, olho, eixos));
    let (lb, lv, lg) = limiares();
    let motivo = match ant {
        None => Some("câmera nova".to_string()),
        Some((e0, _, _, _)) if e - e0 > CAMERA_NOVA_ENVIOS => Some(format!("câmera volta depois de {} envios de outras", e - e0 - 1)),
        Some((_, q0, o0, e0)) => {
            let lp = lb + lv * q.saturating_sub(q0).max(1) as f32;
            let dp = ((olho[0] - o0[0]).powi(2) + (olho[1] - o0[1]).powi(2) + (olho[2] - o0[2]).powi(2)).sqrt();
            // escala (eixos não unitários): normaliza pelo tamanho médio dos eixos
            let n0 = (e0.iter().map(|x| x * x).sum::<f32>() / 3.0).sqrt();
            let n1 = (eixos.iter().map(|x| x * x).sum::<f32>() / 3.0).sqrt();
            let tr = e0.iter().zip(eixos.iter()).map(|(a, b)| a * b).sum::<f32>() / (n0 * n1).max(1e-12);
            let graus = ((tr - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees();
            if log_on() && (dp > 0.01 || graus > 0.05) {
                eprintln!("[etiquetas] câmera {cam:#x} q={q} (+{}) olho Δ {dp:.3} giro {graus:.3}°", q.saturating_sub(q0));
            }
            (!(dp <= lp && graus <= lg)).then(|| format!("salto do olho {dp:.1} / giro {graus:.1}°"))
        }
    };
    if j.cams.len() > 64 {
        j.cams.retain(|_, x| e - x.0 <= CAMERA_NOVA_ENVIOS);
    }
    drop(j);
    if let Some(m) = motivo {
        CORTE.fetch_add(1, Relaxed);
        N_CORTES_CAM.fetch_add(1, Relaxed);
        if log_on() {
            eprintln!("[etiquetas] corte de câmera {cam:#x} q={q}: {m}");
        }
    }
}

/// Lista despachada (`gsp`, thread do jogo): tira do anel as etiquetas da faixa `[addr, addr+size)`.
pub fn extrair(addr: u32, size: u32) -> Option<Arc<EtiqLista>> {
    sincronizar();
    if !ATIVO.load(Relaxed) {
        return None;
    }
    let fim = addr.wrapping_add(size);
    let mut j = jogo();
    if j.anel.is_empty() {
        return None;
    }
    let mut itens = Vec::new();
    j.anel.retain(|x| {
        if x.0 >= addr && x.1 <= fim {
            itens.push(*x);
            false
        } else {
            true
        }
    });
    drop(j);
    if log_on() {
        static N: AtomicU32 = AtomicU32::new(0);
        if N.fetch_add(1, Relaxed) < 40 {
            eprintln!(
                "[etiquetas] lista {addr:#x}+{size:#x}: {} etiquetas{}",
                itens.len(),
                itens.first().map_or(String::new(), |p| format!(" (1ª faixa {:#x}..{:#x}, última até {:#x})", p.0, p.1, itens.last().unwrap().1))
            );
        }
    }
    if itens.is_empty() {
        return None;
    }
    itens.sort_unstable_by_key(|x| x.0);
    Some(Arc::new(EtiqLista { itens }))
}

/// Endereços (guest) das palavras de cabeçalho que disparam um draw (escrita em 0x22E/0x22F), na ordem de execução —
/// a mesma decodificação de `pica::execute_inner`.
pub fn disparos(base: u32, words: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 2 <= words.len() {
        let hdr = words[p + 1];
        let id = hdr & 0xffff;
        let extra = ((hdr >> 20) & 0xff) as usize;
        let consecutive = hdr >> 31 != 0;
        for k in 0..=extra {
            let reg = if consecutive { id + k as u32 } else { id };
            if reg == 0x22e || reg == 0x22f {
                // endereço do cabeçalho (o valor de k = 0 vem antes dele; os extras depois)
                out.push(base.wrapping_add(4 * (p as u32 + 1)));
            }
        }
        p += 2 + extra;
        if extra % 2 == 1 {
            p += 1;
        }
    }
    out
}

/// Etiqueta de cada disparo de draw da lista (`None` = draw sem etiqueta). Se alguma etiqueta não cair em exatamente
/// um disparo, a lista inteira fica sem etiquetas (`Err` com o motivo).
pub fn casar(base: u32, words: &[u32], et: &EtiqLista) -> Result<Vec<Option<EtiqDraw>>, String> {
    let d = disparos(base, words);
    let mut out = vec![None; d.len()];
    let mut i = 0usize;
    for &(ini, fim, e) in &et.itens {
        while i < d.len() && d[i] < ini {
            i += 1;
        }
        let mut k = 0;
        while i + k < d.len() && d[i + k] < fim {
            k += 1;
        }
        if k != 1 {
            return Err(format!("faixa {ini:#x}..{fim:#x} com {k} disparos de draw"));
        }
        out[i] = Some(e);
        i += 1;
    }
    Ok(out)
}

/// Etiqueta do draw que a thread "gpu" está desenhando agora: (etiqueta, lista com etiquetas descartadas).
#[derive(Clone, Copy, Debug, Default)]
pub struct Atual {
    pub et: Option<EtiqDraw>,
    /// a lista deste draw tinha etiquetas, mas foram descartadas (inconsistentes): o quadro não usa etiquetas
    pub ruim: bool,
}

thread_local! {
    static ATUAL: Cell<Atual> = const { Cell::new(Atual { et: None, ruim: false }) };
}

pub fn set_draw_atual(a: Atual) {
    ATUAL.with(|c| c.set(a));
}

pub fn draw_atual() -> Atual {
    ATUAL.with(|c| c.get())
}

/// Lista com etiquetas inconsistentes (avisa uma vez).
pub fn lista_ruim(base: u32, motivo: &str) {
    N_LISTAS_RUINS.fetch_add(1, Relaxed);
    static AVISOU: AtomicBool = AtomicBool::new(false);
    if !AVISOU.swap(true, Relaxed) {
        eprintln!("[etiquetas] lista {base:#x}: etiquetas ignoradas ({motivo}) — a interpolação usa a heurística nesses quadros (aviso único)");
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn bloco_draw(n: u32) -> Vec<u32> {
        // como FUN_0012fb34: 0x228 = n, …, disparo 0x22F (cabeçalho 0xf022f) e mais estado
        vec![
            n, 0xf0228, 0, 0x20229, 0, 0x30253, 0x300, 0x2025e, 1, 0xf025f, 0x8000_0000, 0xf0227, 0, 0x10245, 1, 0xf022f,
            1, 0xf0245, 0, 0xf0231, 0, 0x8025e, 0, 0x8025e, 1, 0xf0111, 0, 0x30253,
        ]
    }

    #[test]
    fn etiq_disparo_cai_na_faixa_do_bloco() {
        let base = 0x1440_0000u32;
        let mut w = vec![0x10, 0x000f_0041, 0x20, 0x0010_0042]; // estado qualquer
        let ini = base + 4 * w.len() as u32;
        w.extend(bloco_draw(36));
        let fim = base + 4 * w.len() as u32;
        assert_eq!(fim - ini, BLOCO_DRAW);
        w.extend(bloco_draw(12)); // draw sem etiqueta (fora do laço de objetos)
        let d = disparos(base, &w);
        assert_eq!(d, vec![ini + 0x3c, fim + 0x3c]);
        let e = EtiqDraw { et: Etiqueta { obj: 0x1494_4e88, malha: 0x14de_aa50, prim: 0x14de_aabc, sub: 0, origem: 0x32547c, ancora: 0 }, corte: 7, grupo: 0, sem_id: false };
        let l = EtiqLista { itens: vec![(ini, fim, e)] };
        assert_eq!(casar(base, &w, &l).unwrap(), vec![Some(e), None]);
        // faixa que pega os dois draws: inconsistente
        let l = EtiqLista { itens: vec![(ini, fim + 0x70, e)] };
        assert!(casar(base, &w, &l).is_err());
        // faixa sem disparo
        let l = EtiqLista { itens: vec![(base, base + 8, e)] };
        assert!(casar(base, &w, &l).is_err());
    }

    #[test]
    fn etiq_cabecalho_com_extras() {
        // escrita consecutiva 0x22D..0x22F (3 valores) — o disparo conta no endereço do cabeçalho
        let base = 0x100u32;
        let w = vec![1, 0x802f_022d, 2, 3];
        assert_eq!(disparos(base, &w), vec![base + 4, base + 4]);
    }
}
