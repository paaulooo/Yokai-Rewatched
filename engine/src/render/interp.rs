//! Interpolação de quadros por ESTADO DE DESENHO (sem artefatos de imagem).
//!
//! O jogo gera imagem nova a 30 Hz na jogabilidade. Em vez de misturar duas imagens (fantasma, borrão, buracos onde algo
//! se move), este módulo re-renderiza o quadro intermediário de verdade: os desenhos da tela de cima de cada quadro são
//! capturados por inteiro (estado, programa, vértices, índices, uniforms); no quadro N+1 eles são pareados com os do
//! quadro N pela estrutura (estado de pipeline, programa, trechos de vértice, nº de índices) e, para cada α entre 0 e 1,
//! os uniforms float do shader de vértice — onde moram as matrizes de câmera e de ossos — são interpolados e o quadro é
//! desenhado de novo (oclusão, sombras, transparência e iluminação corretas), pelo mesmo caminho de desenho do jogo.
//!
//! Matrizes (como o RT64 do Zelda 64 Recompiled faz com `gEXMatrixGroupDecomposed`): três registradores seguidos cuja
//! parte 3×3 é rotação × escala uniforme nos dois quadros são decompostos em translação (lerp), escala (lerp) e rotação
//! (slerp de quatérnio) — o lerp elemento a elemento encolhe o objeto no meio de uma rotação. O resto é lerp.
//!
//! Medido (diagnóstico `YW_INTERP_STATS=1`, cenas de jogo): 99–100 % dos desenhos pareiam, 100 % deles com a MESMA geometria
//! (o movimento é GPU skinning/uniforms, não vértices reescritos) e 0 % com salto grande entre quadros seguidos.
//! `YW_INTERP_VALIDATE=1` mede a qualidade: prevê o quadro N a partir de N−1 e N+1 e compara com o N real (ver RECOMP.md).
//!
//! Onde NÃO interpola (cai no quadro real, sem artefato): corte de cena (poucos desenhos pareados), desenho com salto de
//! uniform (teleporte, passagem do fim para o começo de uma animação), quadro cujo alvo não começou com um clear, e
//! geometria reescrita pela CPU (não pareia). Desenho sem par no quadro anterior é desenhado na pose do quadro novo.
//!
//! Texturas: cada desenho capturado guarda as VERSÕES (`TexKey`, com o hash do conteúdo) que usou; o replay desenha com
//! exatamente essas, do cache (`Vk::replay_tex`), sem reler a memória do jogo — a thread da GPU é assíncrona ao jogo, e
//! o texto da HUD (atlas reescrito pela CPU) saía embaralhado com a versão do quadro seguinte. As versões dos quadros
//! capturados ficam pinadas (`Vk::tex_pinned`); se mesmo assim faltar alguma, o quadro não é interpolado.
//! Desenhos sem teste de profundidade (HUD/2D) usam o shading do quadro novo, sem lerp (`YW_INTERP_HUD=1` volta ao lerp).

use super::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Captura ligada? (interpolação pedida ou estatística de diagnóstico)
pub fn capturing() -> bool {
    stats_enabled() || crate::render::interp_target() > 0
}

pub fn stats_enabled() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_INTERP_STATS").is_ok_and(|v| v != "0"))
}

/// `YW_INTERP_HUD=1`: desenhos sem teste de profundidade (HUD/2D) também interpolam os uniforms (comportamento antigo).
fn hud_lerp() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_INTERP_HUD").is_ok_and(|v| v != "0"))
}

fn h64(parts: &[u64]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &p in parts {
        h = (h ^ p).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(23);
    }
    h
}

/// Salto de uniform acima do qual o desenho deixa de interpolar (unidades do mundo).
const JUMP_LIMIT: f32 = 50.0;

/// Um desenho capturado da tela de cima (dono de tudo que precisa para ser desenhado de novo).
pub(super) struct Cap {
    pub ctx: Ctx,
    pub shading: Box<VsShading>,
    pub spans: Vec<(u32, u32)>,
    pub data: Vec<Arc<[u8]>>,
    pub indices: Vec<u32>,
    pub prog_gen: u64,
    pub code: Arc<Vec<u32>>,
    pub desc: Arc<Vec<u32>>,
    /// identidade estrutural (ver o módulo)
    pub skey: u64,
    /// hash dos bytes de vértice (diferente entre quadros = a CPU reescreveu a geometria)
    pub vhash: u64,
    /// versões das texturas (por unidade) que o desenho usou: o replay usa estas, do cache
    pub tex: [Option<TexKey>; 3],
    /// teste de profundidade ligado (`PipeKey::dtest`): sem ele (HUD/2D) o replay não interpola os uniforms
    pub dtest: bool,
}

/// Os desenhos de um quadro da tela de cima, do clear ao DisplayTransfer.
#[derive(Default)]
pub(super) struct FrameCap {
    pub draws: Vec<Cap>,
    /// conteúdo inicial do alvo apresentado e da profundidade dele (resolvidos por endereço em `resolve`)
    pub init_color: Option<[f32; 4]>,
    pub init_depth: Option<(f32, u32)>,
    /// clear no meio do quadro, ou captura grande demais: não interpola
    pub complex: bool,
    /// clears de cor/profundidade dos alvos de tela antes de qualquer desenho neles, por endereço
    clears_c: Vec<(u32, [f32; 4])>,
    clears_d: Vec<(u32, (f32, u32))>,
    /// endereços limpos depois de já terem recebido desenhos (clear no meio do quadro)
    complex_at: Vec<u32>,
}

impl FrameCap {
    /// Versões de textura usadas pelos desenhos.
    fn tex_keys(&self) -> impl Iterator<Item = TexKey> + '_ {
        self.draws.iter().flat_map(|c| c.tex.iter().flatten().copied())
    }
}

/// Só o alvo apresentado (`inp`) conta (H4): desenhos em outros alvos de tela do mesmo quadro (outro olho, alvo
/// intermediário) ficam de fora, e o conteúdo inicial vem dos clears do alvo e da profundidade DELE.
fn resolve(mut cur: FrameCap, inp: u32) -> FrameCap {
    cur.draws.retain(|d| d.ctx.color_addr == inp);
    let da = cur.draws.first().map_or(0, |d| d.ctx.depth_addr);
    cur.init_color = cur.clears_c.iter().find(|(a, _)| *a == inp).map(|x| x.1);
    cur.init_depth = if da == 0 { None } else { cur.clears_d.iter().find(|(a, _)| *a == da).map(|x| x.1) };
    if cur.complex_at.iter().any(|&a| a == inp || (da != 0 && a == da)) {
        cur.complex = true;
    }
    cur
}

#[derive(Default)]
struct Stats {
    frames: u32,
    draws: u64,
    matched: u64,
    static_geo: u64,
    dyn_geo: u64,
    jumpy: u64,
    cuts: u32,
    mids: u32,
    cost_ms: f64,
    /// soma do tempo de GPU do resto do trabalho por quadro (ms) e quadros somados
    gpu_frame_ms: f64,
    /// diagnóstico da corrida jogo × thread da GPU: texturas (versões distintas por quadro re-renderizado) cuja
    /// memória já não tem o conteúdo capturado na hora do replay (antes, o replay usaria o conteúdo novo)
    tex_changed: u64,
    /// quadros não interpolados porque alguma textura capturada saiu do cache
    no_tex: u32,
}

#[derive(Default)]
pub(super) struct Interp {
    cur: FrameCap,
    prev: Option<FrameCap>,
    /// versões de textura do quadro entre `present_check` e `present_finish` (fora de `cur` e de `prev`): pinadas
    inflight: Vec<TexKey>,
    last_end: Option<std::time::Instant>,
    period: f32,
    /// custo (ms) de CPU da thread da GPU por quadro intermediário (gravar + enviar; no modo síncrono inclui a espera
    /// da GPU) — média móvel: limita quantos cabem por período
    cost_ms: f32,
    /// tempo de GPU pelos timestamps (`Vk::gpu_work_ns`/`gpu_mid_ns`): do trabalho que não é intermediário desde o
    /// último quadro (cena, sombra N×, present) e de um intermediário (média móvel); marca = (trabalho, intermediários,
    /// quantos) no último quadro
    gpu_frame_ms: f32,
    gpu_mid_ms: f32,
    gpu_mark: Option<(u64, u64, u64)>,
    /// duração dos quadros recentes em VBlanks (o menor = o ritmo nominal do jogo agora; 2 na jogabilidade)
    vb_hist: std::collections::VecDeque<u8>,
    nominal_vb: u8,
    /// rede de segurança: teto de intermediários = 5 − `corte`. Cai quando um quadro com intermediários atrasa um
    /// VBlank (eles rodam na thread da GPU antes do PPF e roubaram tempo do jogo); volta ao orçamento depois de `espera`
    /// quadros no ritmo, e `espera` dobra a cada atraso (30 → 240) para não virar um tranco periódico.
    corte: usize,
    no_ritmo: u32,
    espera: u32,
    last_m: usize,
    /// orçamento: tempo ocupado da thread da GPU no último período (`gsp::GPU_BUSY_NS`) sem os intermediários
    busy_mark: u64,
    busy_ms: f32,
    mids_ms: f32,
    geo: HashMap<u64, Arc<[u8]>>,
    progs: HashMap<u64, (Arc<Vec<u32>>, Arc<Vec<u32>>)>,
    st: Stats,
    // validação (`YW_INTERP_VALIDATE=1`): capturas do quadro N−1 e as imagens reais de N−1 e N
    prev2: Option<FrameCap>,
    real_prev: Option<Arc<Vec<u8>>>,
    real_prev2: Option<Arc<Vec<u8>>>,
    v: ValStats,
}

/// Soma das diferenças entre a previsão e o quadro real (e entre a mistura de imagens e o real).
#[derive(Default)]
struct ValStats {
    n: u32,
    pred_mean: f64,
    pred_big: f64,
    blend_mean: f64,
    blend_big: f64,
    still_mean: f64,
    still_big: f64,
    /// fidelidade do re-render: α = 1 tem de reproduzir o quadro real novo
    faith_mean: f64,
    faith_big: f64,
    /// a mesma previsão com as matrizes interpoladas elemento a elemento (sem decompor): o ganho da decomposição
    elem_mean: f64,
    elem_big: f64,
    /// previsões em que alguma matriz foi decomposta (nas outras as duas formas dão o mesmo)
    decomp: u32,
}

fn validating() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_INTERP_VALIDATE").is_ok_and(|v| v != "0"))
}

/// (diferença média por canal RGB, fração de pixels com algum canal > 24) entre duas imagens RGBA do mesmo tamanho.
fn diff(a: &[u8], b: &[u8]) -> (f64, f64) {
    let n = a.len().min(b.len()) / 4;
    let (mut sum, mut big) = (0u64, 0u64);
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let d = (0..3).map(|c| (x[c] as i32 - y[c] as i32).unsigned_abs()).collect::<Vec<_>>();
        sum += d.iter().map(|&v| v as u64).sum::<u64>();
        big += (d.iter().any(|&v| v > 24)) as u64;
    }
    (sum as f64 / (3 * n.max(1)) as f64, big as f64 / n.max(1) as f64)
}

/// Duração de um VBlank no relógio do host (ms).
fn vblank_ms() -> f32 {
    (1000.0 / (crate::hle::vblank_hz() * crate::hle::speed())) as f32
}

impl Interp {
    pub fn reset(&mut self) {
        self.cur = FrameCap::default();
        self.prev = None;
        self.inflight.clear();
        self.last_end = None;
    }

    /// Período nominal (ms): o ritmo do jogo em VBlanks inteiros (2 × 16,7 ms na jogabilidade), não o medido — o medido
    /// sobe quando a interpolação atrasa a GPU, e calcular os intermediários por ele os faria aumentar, atrasando mais.
    pub fn nominal_ms(&self) -> f32 {
        self.nominal_vb.max(1) as f32 * vblank_ms()
    }

    /// Um alvo de cor/profundidade da tela de cima foi limpo (MemoryFill): é o conteúdo inicial do quadro nele. Por
    /// endereço: só um clear no MESMO alvo depois de desenhos nele é "clear no meio do quadro".
    pub fn note_clear(&mut self, k: SurfKey, c: Clear) {
        if !capturing() || k.h != 400 || !(k.w == 480 || k.w == 240) {
            return;
        }
        let cur = &mut self.cur;
        match c {
            Clear::Color(v) if !k.depth => {
                if cur.draws.iter().any(|d| d.ctx.color_addr == k.addr) {
                    cur.complex_at.push(k.addr);
                } else {
                    cur.clears_c.retain(|(a, _)| *a != k.addr);
                    cur.clears_c.push((k.addr, v));
                }
            }
            Clear::Depth(z, st) if k.depth => {
                if cur.draws.iter().any(|d| d.ctx.depth_addr == k.addr) {
                    cur.complex_at.push(k.addr);
                } else {
                    cur.clears_d.retain(|(a, _)| *a != k.addr);
                    cur.clears_d.push((k.addr, (z, st)));
                }
            }
            _ => {}
        }
    }

    /// Captura um desenho com o shader de vértice na GPU; `tex` = as versões das texturas que ele usou. Falso = não
    /// capturou (quadro já descartado como complexo).
    pub fn record(&mut self, ctx: &Ctx, key: PipeKey, aot: Option<usize>, dr: &VsDraw, tex: [Option<TexKey>; 3]) -> bool {
        if self.cur.complex {
            return false;
        }
        if self.cur.draws.len() >= 1500 {
            self.cur.complex = true;
            return false;
        }
        let mut data = Vec::with_capacity(dr.spans.len());
        let (mut spans, mut vh) = (0u64, 0u64);
        for &(a, n) in dr.spans {
            spans = h64(&[spans, a as u64, n as u64]);
            let bytes: &[u8] = guest_slice(a, n).unwrap_or(&[]);
            let hv = hash_bytes(bytes);
            vh = h64(&[vh, hv]);
            let arc = self.geo.entry(hv ^ ((n as u64) << 40)).or_insert_with(|| Arc::from(bytes)).clone();
            data.push(arc);
        }
        if self.geo.len() > 1024 {
            self.geo.clear();
        }
        let (code, desc) = self
            .progs
            .entry(dr.prog_gen)
            .or_insert_with(|| (Arc::new(dr.code.to_vec()), Arc::new(dr.desc.to_vec())))
            .clone();
        if self.progs.len() > 256 {
            self.progs.clear();
        }
        let kh = {
            // SipHash de chaves fixas: determinístico entre quadros e execuções
            let mut hs = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut hs);
            hs.finish()
        };
        // conteúdo das texturas (o hash da versão): vídeo, textura animada ou renderizada muda de um quadro para o
        // outro e tira o desenho do pareamento
        let thash = tex.iter().enumerate().fold(0u64, |h, (i, k)| match k {
            Some(k) => h64(&[h, i as u64, k.addr as u64, k.hash]),
            None => h64(&[h, i as u64]),
        });
        let skey = h64(&[kh, aot.map_or(u64::MAX, |a| a as u64), dr.shading.misc[1] as u64, spans, dr.indices.len() as u64, thash]);
        self.cur.draws.push(Cap {
            ctx: ctx.clone(),
            shading: Box::new(*dr.shading),
            spans: dr.spans.to_vec(),
            data,
            indices: dr.indices.to_vec(),
            prog_gen: dr.prog_gen,
            code,
            desc,
            skey,
            vhash: vh,
            tex,
            dtest: key.dtest,
        });
        true
    }
}

/// Componente que é uma CHAVE (exatamente 0 ou 1 nos dois quadros e trocou): não se interpola (0,5 não existe) e não é
/// movimento — usa o valor do quadro novo. Visto no jogo: `c7.3` alterna 0 ↔ 1 a cada quadro.
fn is_switch(x: f32, y: f32) -> bool {
    x != y && (x == 0.0 || x == 1.0) && (y == 0.0 || y == 1.0)
}

/// Linha `i` dos uniforms com salto grande entre os dois quadros? (componentes tipo rotação/escala [−1, 1] não passam
/// de 0,8 por quadro; translações não passam de `JUMP_LIMIT`)
fn jump_row(a: &VsShading, b: &VsShading, i: usize) -> bool {
    (0..4).any(|c| {
        let (x, y) = (a.f[i][c], b.f[i][c]);
        let d = (x - y).abs();
        !d.is_finite() || (!is_switch(x, y) && (d > JUMP_LIMIT || (x.abs().max(y.abs()) <= 1.0 && d > 0.8)))
    })
}

/// Desenho com algum uniform saltando (conta para o corte de cena em `usable`).
fn jumpy(a: &VsShading, b: &VsShading) -> bool {
    (0..96).any(|i| jump_row(a, b, i))
}

/// Linhas que saltam: no replay ficam com o valor do quadro novo, e só elas — o resto do desenho (a geometria)
/// continua interpolando junto com a cena.
fn jump_rows(a: &VsShading, b: &VsShading) -> [bool; 96] {
    std::array::from_fn(|i| jump_row(a, b, i))
}

/// `YW_INTERP_JUMP_DRAW=1`: um desenho com salto vai inteiro para o quadro novo (comportamento antigo). Fazia o vidro e
/// o reflexo das janelas (linha de geração de coordenada de textura que acompanha a câmera, `c41.3` −0,34 → 0,62)
/// pularem para o quadro novo enquanto a parede e a moldura interpolavam: camadas coplanares desencontradas, o teste
/// de profundidade derrubava o reflexo e o vidro piscava escuro nos intermediários.
fn jump_whole_draw() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("YW_INTERP_JUMP_DRAW").is_ok_and(|v| v != "0"))
}

type M3 = [[f64; 3]; 3];

/// Linhas `r..r+3` como matriz afim 3×4 (parte 3×3 nas componentes xyz, translação no .w): se a parte 3×3 é rotação ×
/// escala uniforme (linhas ortogonais de mesmo tamanho, sem espelho), devolve (escala, rotação). Uma matriz guardada
/// por colunas também passa (a transposta de uma rotação é rotação) e continua certa, porque o slerp comuta com a
/// transposição. Projeção, luz, cor e afins não passam e ficam no lerp.
fn rigid(f: &[[f32; 4]; 96], r: usize) -> Option<(f64, M3)> {
    if !f[r..r + 3].iter().flatten().all(|v| v.is_finite()) {
        return None;
    }
    let row = |i: usize| [f[r + i][0] as f64, f[r + i][1] as f64, f[r + i][2] as f64];
    let m = [row(0), row(1), row(2)];
    let dot = |x: [f64; 3], y: [f64; 3]| x[0] * y[0] + x[1] * y[1] + x[2] * y[2];
    let n = [dot(m[0], m[0]), dot(m[1], m[1]), dot(m[2], m[2])];
    let s2 = (n[0] + n[1] + n[2]) / 3.0;
    if !(s2 > 1e-10) {
        return None;
    }
    let tol = 1e-3 * s2;
    if n.iter().any(|&v| (v - s2).abs() > tol) || [dot(m[0], m[1]), dot(m[0], m[2]), dot(m[1], m[2])].iter().any(|v| v.abs() > tol) {
        return None;
    }
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det <= 0.0 {
        return None;
    }
    let s = s2.sqrt();
    Some((s, m.map(|l| l.map(|v| v / s))))
}

/// Quatérnio (x, y, z, w) de uma rotação.
fn quat(m: &M3) -> [f64; 4] {
    let t = m[0][0] + m[1][1] + m[2][2];
    let q = if t > 0.0 {
        let s = (t + 1.0).sqrt() * 2.0;
        [(m[2][1] - m[1][2]) / s, (m[0][2] - m[2][0]) / s, (m[1][0] - m[0][1]) / s, 0.25 * s]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [0.25 * s, (m[0][1] + m[1][0]) / s, (m[0][2] + m[2][0]) / s, (m[2][1] - m[1][2]) / s]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [(m[0][1] + m[1][0]) / s, 0.25 * s, (m[1][2] + m[2][1]) / s, (m[0][2] - m[2][0]) / s]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [(m[0][2] + m[2][0]) / s, (m[1][2] + m[2][1]) / s, 0.25 * s, (m[1][0] - m[0][1]) / s]
    };
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    q.map(|v| v / n)
}

fn rot(q: [f64; 4]) -> M3 {
    let [x, y, z, w] = q;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

/// Interpolação esférica pelo caminho curto.
fn slerp(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] {
    let mut d = a.iter().zip(&b).map(|(x, y)| x * y).sum::<f64>();
    let b = if d < 0.0 {
        d = -d;
        b.map(|v| -v)
    } else {
        b
    };
    let (wa, wb) = if d > 0.9995 {
        (1.0 - t, t)
    } else {
        let th = d.acos();
        (((1.0 - t) * th).sin() / th.sin(), (t * th).sin() / th.sin())
    };
    let q = [0, 1, 2, 3].map(|i| wa * a[i] + wb * b[i]);
    let n = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    q.map(|v| v / n)
}

/// Uniforms em α entre `a` e `b`. `decompor = false` só na validação (comparação com o lerp elemento a elemento).
fn lerp_shading(a: &VsShading, b: &VsShading, t: f32, decompor: bool) -> Box<VsShading> {
    lerp_shading_snap(a, b, t, decompor, &[false; 96])
}

/// `lerp_shading` com as linhas `snap` fixas no quadro novo (as que saltam, `jump_rows`).
fn lerp_shading_snap(a: &VsShading, b: &VsShading, t: f32, decompor: bool, snap: &[bool; 96]) -> Box<VsShading> {
    let mut o = Box::new(*b);
    if t >= 1.0 {
        return o;
    }
    // matrizes rígidas (as mesmas linhas nos dois quadros): decompostas; o resto, componente a componente
    let mut feito = *snap;
    let mut r = 0;
    while decompor && r + 3 <= 96 {
        if snap[r..r + 3].iter().any(|&j| j) {
            r += 1;
            continue;
        }
        let (Some((sa, ra)), Some((sb, rb))) = (rigid(&a.f, r), rigid(&b.f, r)) else {
            r += 1;
            continue;
        };
        if a.f[r..r + 3] != b.f[r..r + 3] {
            let td = t as f64;
            let s = sa + (sb - sa) * td;
            let m = rot(slerp(quat(&ra), quat(&rb), td));
            for i in 0..3 {
                for c in 0..3 {
                    o.f[r + i][c] = (m[i][c] * s) as f32;
                }
                let (x, y) = (a.f[r + i][3], b.f[r + i][3]);
                o.f[r + i][3] = x + (y - x) * t;
            }
        }
        feito[r..r + 3].fill(true);
        r += 3;
    }
    for i in (0..96).filter(|&i| !feito[i]) {
        for c in 0..4 {
            let (x, y) = (a.f[i][c], b.f[i][c]);
            o.f[i][c] = if is_switch(x, y) { y } else { x + (y - x) * t };
        }
    }
    o
}

pub(super) struct Pairing {
    /// por desenho do quadro novo: índice do par no anterior
    pub pair: Vec<Option<usize>>,
    pub jumpy: Vec<bool>,
    pub matched: usize,
    pub njumpy: usize,
    pub static_geo: usize,
}

/// Distância entre os uniforms float de dois desenhos (soma de |Δ|; chaves 0/1 e valores não finitos não contam).
fn shading_dist(a: &VsShading, b: &VsShading) -> f32 {
    let mut d = 0.0f32;
    for i in 0..96 {
        for c in 0..4 {
            let (x, y) = (a.f[i][c], b.f[i][c]);
            if x.is_finite() && y.is_finite() && !is_switch(x, y) {
                d += (x - y).abs();
            }
        }
    }
    d
}

/// Pareamento em ordem por identidade estrutural (janela de 48 desenhos à frente do primeiro ainda livre). Entre
/// candidatos de mesma identidade (vários personagens com o mesmo modelo e textura) fica o de uniforms mais próximos:
/// parear o personagem errado interpolaria a matriz de um com a do outro.
pub(super) fn pair(prev: &FrameCap, cur: &FrameCap) -> Pairing {
    let mut used = vec![false; prev.draws.len()];
    let mut first = 0usize;
    let mut pr = Pairing { pair: vec![None; cur.draws.len()], jumpy: vec![false; cur.draws.len()], matched: 0, njumpy: 0, static_geo: 0 };
    for (i, c) in cur.draws.iter().enumerate() {
        while first < used.len() && used[first] {
            first += 1;
        }
        let hi = prev.draws.len().min(first + 48);
        let mut best: Option<(usize, f32)> = None;
        for j in first..hi {
            if used[j] || prev.draws[j].skey != c.skey {
                continue;
            }
            let d = shading_dist(&prev.draws[j].shading, &c.shading);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((j, d));
            }
        }
        if let Some((j, _)) = best {
            used[j] = true;
            pr.pair[i] = Some(j);
            pr.matched += 1;
            if prev.draws[j].vhash == c.vhash {
                pr.static_geo += 1;
            }
            if jumpy(&prev.draws[j].shading, &c.shading) {
                pr.jumpy[i] = true;
                pr.njumpy += 1;
            }
        }
    }
    pr
}

/// O par de quadros pode ser interpolado? (sem clear no meio, começa com clear de cor e de profundidade, ≥ 70 % dos
/// desenhos pareados e ≤ 20 % deles com salto — senão é corte de cena e vale o quadro real)
fn usable(prev: &FrameCap, cur: &FrameCap, pr: &Pairing) -> bool {
    !cur.complex
        && !prev.complex
        && cur.init_color.is_some()
        && cur.init_depth.is_some()
        && pr.matched * 10 >= cur.draws.len() * 7
        && pr.njumpy * 5 <= pr.matched
}

impl Vk {
    /// Recalcula as versões de textura pinadas: quadro anterior ∪ quadro em curso ∪ quadro entre os dois passos da
    /// apresentação (`evict` não as remove).
    pub(super) fn interp_repin(&mut self) {
        self.tex_pinned.clear();
        let ip = &self.ip;
        let it = ip.prev.iter().flat_map(|p| p.tex_keys()).chain(ip.cur.tex_keys()).chain(ip.inflight.iter().copied());
        self.tex_pinned.extend(it);
    }

    /// Tira os desenhos capturados do quadro que está sendo apresentado (`present_check`); as texturas deles continuam
    /// pinadas até `interp_frame_done`.
    pub(super) fn interp_take(&mut self) -> FrameCap {
        let cap = std::mem::take(&mut self.ip.cur);
        self.ip.inflight = cap.tex_keys().collect();
        cap
    }

    /// Fim de um quadro da tela de cima (DisplayTransfer): atualiza o período, pareia com o quadro anterior e, se a
    /// interpolação está ligada e o par é seguro, devolve os quadros intermediários re-renderizados (saídas do anel, uma
    /// por α, enviadas à GPU num lote só). `cur` = os desenhos do quadro (`interp_take`); `real` = a saída do quadro real
    /// (o hash dela, do framebuffer nativo, vale para os intermediários também).
    pub(super) unsafe fn interp_frame_done(&mut self, req: PresentReq, real: &OutRef, cur: FrameCap) -> Result<Option<Vec<OutRef>>, String> {
        let r = self.interp_frame_done_in(req, real, cur);
        self.ip.inflight.clear();
        self.interp_repin();
        r
    }

    unsafe fn interp_frame_done_in(&mut self, req: PresentReq, real: &OutRef, cur: FrameCap) -> Result<Option<Vec<OutRef>>, String> {
        if !capturing() {
            return Ok(None);
        }
        // só o framebuffer que o LCD de cima mostra como olho esquerdo (o da janela) é interpolado: o outro olho do 3D
        // estéreo é outro render, a poucos ms deste, e pareá-lo com o quadro anterior misturaria olhos
        if !crate::hle::gpu::top_shown(req.out) {
            return Ok(None);
        }
        let now = std::time::Instant::now();
        if let Some(t) = self.ip.last_end {
            let dt = now.duration_since(t).as_secs_f32() * 1000.0;
            if (6.0..250.0).contains(&dt) {
                self.ip.period = if self.ip.period == 0.0 { dt } else { 0.9 * self.ip.period + 0.1 * dt };
                let n = (dt / vblank_ms()).round().clamp(1.0, 8.0) as u8;
                let h = &mut self.ip.vb_hist;
                h.push_back(n);
                if h.len() > 60 {
                    h.pop_front();
                }
                // ritmo nominal = 10º percentil dos últimos 60 quadros (um quadro curto isolado, de fila da GPU, não
                // conta; uma queda que dura também não engana: os quadros no ritmo ainda estão na janela)
                let mut ord: Vec<u8> = h.iter().copied().collect();
                ord.sort_unstable();
                self.ip.nominal_vb = ord[ord.len() / 10];
                let espera = self.ip.espera.max(30);
                if n > self.ip.nominal_vb {
                    // quadro atrasado: se havia intermediários, um a menos daqui para a frente, e espera mais para tentar
                    if self.ip.last_m > 0 {
                        self.ip.corte = self.ip.corte.max(6 - self.ip.last_m.min(5));
                        self.ip.espera = (espera * 2).min(240);
                    }
                    self.ip.no_ritmo = 0;
                } else {
                    self.ip.no_ritmo += 1;
                    // passada a espera, volta ao orçamento previsto (o limitador principal); a espera só encurta de novo
                    // depois de um período inteiro sem atraso
                    if self.ip.no_ritmo >= espera {
                        if self.ip.corte > 0 {
                            self.ip.corte = 0;
                        } else {
                            self.ip.espera = 30;
                        }
                        self.ip.no_ritmo = 0;
                    }
                }
            }
        }
        self.ip.last_end = Some(now);
        self.ip.last_m = 0;
        // tempo ocupado da thread da GPU desde o último quadro, sem os intermediários dele
        let b = crate::hle::services::gsp::GPU_BUSY_NS.load(std::sync::atomic::Ordering::Relaxed);
        if self.ip.busy_mark != 0 {
            self.ip.busy_ms = ((b - self.ip.busy_mark) as f32 / 1e6 - self.ip.mids_ms).max(0.0);
        }
        self.ip.busy_mark = b;
        self.ip.mids_ms = 0.0;
        self.interp_gpu_budget();
        let cur = resolve(cur, req.inp);
        if cur.draws.is_empty() {
            return Ok(None);
        }
        let prev = self.ip.prev.take();
        if validating() {
            self.validate(req, &cur, real)?;
        }
        let out = self.interp_make(req, prev.as_ref(), &cur, real.hash);
        if validating() {
            self.ip.prev2 = prev; // no próximo quadro este é o N−1
        }
        self.ip.prev = Some(cur);
        out
    }

    /// Descarta os desenhos capturados do quadro em curso (olho que a janela não mostra).
    pub(super) fn interp_discard(&mut self) {
        self.ip.cur = FrameCap::default();
        self.interp_repin();
    }

    /// Todas as versões de textura dos desenhos de `cap` estão no cache?
    fn interp_tex_ok(&self, cap: &FrameCap) -> bool {
        cap.tex_keys().all(|k| self.textures.contains_key(&k))
    }

    /// DIAGNÓSTICO (`YW_INTERP_STATS=1`): quantas versões distintas de textura de `cap` já não batem com a memória
    /// agora (o jogo reescreveu a textura depois da captura). Só conta; o replay não usa a memória.
    fn interp_tex_changed(&self, cap: &FrameCap) -> u64 {
        let mut ks: Vec<TexKey> = cap.tex_keys().collect();
        ks.sort_unstable_by_key(|k| (k.addr, k.w, k.h, k.fmt, k.hash));
        ks.dedup();
        ks.iter()
            .filter(|k| {
                let n = k.w * k.h * texture::bits_per_texel(k.fmt) / 8;
                guest_slice(k.addr, n).is_some_and(|m| hash_bytes(m) != k.hash)
            })
            .count() as u64
    }

    /// TESTE (`vulkan::test_interp_replay`): re-renderiza o quadro capturado no alvo `req.inp` em α = 0,5 contra si mesmo.
    #[cfg(test)]
    pub(super) unsafe fn interp_test_replay(&mut self, req: PresentReq) -> Result<Vec<(u32, u32)>, String> {
        if !self.hr.on || !self.hr.ready {
            return Err("resolução interna desligada".into());
        }
        if self.recording {
            self.flush()?;
        }
        let mut cur = resolve(self.interp_take(), req.inp);
        if cur.draws.is_empty() {
            self.ip.inflight.clear();
            return Err(format!("nenhum desenho capturado no alvo {:#x}", req.inp));
        }
        cur.init_color.get_or_insert([0.0; 4]);
        cur.init_depth.get_or_insert((1.0, 0));
        let texs = cur.tex_keys().map(|k| (k.addr, k.w * k.h * texture::bits_per_texel(k.fmt) / 8)).collect();
        self.interp_repin();
        let pr = pair(&cur, &cur);
        let r = self.interp_render(req, &cur, &cur, &pr, 0.5, 0);
        self.ip.inflight.clear();
        self.interp_repin();
        r.map(|_| texs)
    }

    /// VALIDAÇÃO (`YW_INTERP_VALIDATE=1`): prevê o quadro N (o anterior) a partir de N−1 e N+1 (este, `cur`) com α = 0,5
    /// e compara com o N real; compara também com a mistura das imagens e com repetir o quadro, e mede a fidelidade do
    /// re-render (α = 1 contra o quadro real). Uma linha `[interp-validar]` a cada 60 previsões.
    unsafe fn validate(&mut self, req: PresentReq, cur: &FrameCap, real: &OutRef) -> Result<(), String> {
        let real = HiRes { w: 0, h: 0, rgba: self.out_pixels(real)?, hash: real.hash, bytes: 0 };
        let real = &real;
        let p2opt = self.ip.prev2.take();
        if let (Some(p2), Some(rn), Some(rn1)) = (p2opt.as_ref(), self.ip.real_prev.clone(), self.ip.real_prev2.clone()) {
            let pr = pair(p2, cur);
            if usable(p2, cur, &pr) && self.interp_tex_ok(cur) {
                let h = real.hash;
                let pred = self.interp_render(req, p2, cur, &pr, 0.5, h)?;
                let elem = self.interp_render_as(req, p2, cur, &pr, 0.5, false, h)?;
                let p1 = self.interp_render(req, p2, cur, &pr, 1.0, h)?;
                let (fm, fb) = diff(&p1.rgba, &real.rgba);
                let blend: Vec<u8> = rn1.iter().zip(real.rgba.iter()).map(|(a, b)| ((*a as u16 + *b as u16) / 2) as u8).collect();
                let (pm, pb) = diff(&pred.rgba, &rn);
                let (bm, bb) = diff(&blend, &rn);
                let (sm, sb) = diff(&real.rgba, &rn);
                let v = &mut self.ip.v;
                v.n += 1;
                v.pred_mean += pm;
                v.pred_big += pb;
                v.blend_mean += bm;
                v.blend_big += bb;
                v.still_mean += sm;
                v.still_big += sb;
                v.faith_mean += fm;
                v.faith_big += fb;
                if elem.rgba != pred.rgba {
                    v.decomp += 1;
                }
                let (em, eb) = diff(&elem.rgba, &rn);
                v.elem_mean += em;
                v.elem_big += eb;
                if v.n >= 60 {
                    let k = v.n as f64;
                    eprintln!(
                        "[interp-validar] {} previsões de N a partir de N−1 e N+1 · interpolação por desenho: diferença média {:.3}, {:.2}% dos pixels > 24 · (matrizes elemento a elemento: {:.3}, {:.2}%; decomposição mudou a imagem em {} previsões) · mistura de imagens: {:.2}, {:.2}% · repetir o quadro seguinte: {:.2}, {:.2}% · FIDELIDADE do re-render (α=1 vs real): {:.3}, {:.3}%",
                        v.n, v.pred_mean / k, 100.0 * v.pred_big / k, v.elem_mean / k, 100.0 * v.elem_big / k, v.decomp, v.blend_mean / k, 100.0 * v.blend_big / k, v.still_mean / k, 100.0 * v.still_big / k, v.faith_mean / k, 100.0 * v.faith_big / k
                    );
                    self.ip.v = ValStats::default();
                }
            }
        }
        self.ip.real_prev2 = self.ip.real_prev.take();
        self.ip.real_prev = Some(Arc::new(real.rgba.clone()));
        Ok(())
    }

    unsafe fn interp_make(&mut self, req: PresentReq, prev: Option<&FrameCap>, cur: &FrameCap, hash: u64) -> Result<Option<Vec<OutRef>>, String> {
        let target = crate::render::interp_target();
        let Some(prev) = prev else { return Ok(None) };
        let pr = pair(prev, cur);
        // estatística de diagnóstico
        {
            let st = &mut self.ip.st;
            st.frames += 1;
            st.draws += cur.draws.len() as u64;
            st.matched += pr.matched as u64;
            st.static_geo += pr.static_geo as u64;
            st.dyn_geo += (pr.matched - pr.static_geo) as u64;
            st.jumpy += pr.njumpy as u64;
            st.gpu_frame_ms += self.ip.gpu_frame_ms as f64;
        }
        let usable = usable(prev, cur, &pr);
        if !usable {
            self.ip.st.cuts += 1;
        }
        if stats_enabled() && self.ip.st.frames >= 90 {
            let s = &self.ip.st;
            let m = s.matched.max(1) as f64;
            eprintln!(
                "[interp] {} quadros, {:.0} desenhos/quadro · pareados {:.1}% (geometria igual {:.1}%) · saltos {:.1}% · quadros sem interpolar (corte/clear/pouco par) {} · intermediários feitos {} ({:.1} ms de CPU cada; GPU {:.1} ms cada) · período medido {:.1} ms, nominal {:.1} ms · thread da GPU ocupada {:.1} ms/quadro · GPU (resto do quadro) {:.1} ms · teto {} · texturas mudaram desde a captura {} · sem textura {}",
                s.frames,
                s.draws as f64 / s.frames as f64,
                100.0 * m / s.draws.max(1) as f64,
                100.0 * s.static_geo as f64 / m,
                100.0 * s.jumpy as f64 / m,
                s.cuts,
                s.mids,
                if s.mids > 0 { s.cost_ms / s.mids as f64 } else { 0.0 },
                self.ip.gpu_mid_ms,
                self.ip.period,
                self.ip.nominal_ms(),
                self.ip.busy_ms,
                s.gpu_frame_ms / s.frames.max(1) as f64,
                5 - self.ip.corte.min(5),
                s.tex_changed,
                s.no_tex
            );
            self.ip.st = Stats::default();
        }
        if target == 0 || !usable || self.ip.period <= 0.0 {
            return Ok(None);
        }
        // quantos intermediários: alvo (Hz) × período nominal (s) − 1, limitado pelo que a thread da GPU e a própria GPU
        // dão conta e pelo teto adaptativo (quadros do jogo atrasados por causa deles)
        let nominal = self.ip.nominal_ms();
        let mut m = ((target as f32 * nominal / 1000.0).round() as i32 - 1).clamp(0, 5) as usize;
        if self.ip.cost_ms > 0.0 {
            // CPU: o que a thread da GPU deixou livre no último período, menos 15 % de folga
            let livre = nominal - self.ip.busy_ms - 0.15 * nominal;
            m = m.min((livre / self.ip.cost_ms).floor().max(0.0) as usize);
        }
        if self.ip.gpu_mid_ms > 0.0 {
            // GPU (timestamps): o período menos o resto do trabalho dela (cena, sombra, present), com a mesma folga — no
            // modo assíncrono os intermediários de um quadro rodam enquanto a thread já grava o seguinte
            let livre = nominal - self.ip.gpu_frame_ms - 0.15 * nominal;
            m = m.min((livre / self.ip.gpu_mid_ms).floor().max(0.0) as usize);
        }
        m = m.min(5 - self.ip.corte.min(5));
        self.ip.last_m = m;
        if m == 0 {
            return Ok(None);
        }
        // o replay desenha com as versões de textura capturadas: se alguma saiu do cache, vale só o quadro real
        if !self.interp_tex_ok(cur) {
            self.ip.st.no_tex += 1;
            self.ip.last_m = 0;
            return Ok(None);
        }
        if stats_enabled() {
            self.ip.st.tex_changed += self.interp_tex_changed(cur);
        }
        // os m re-renders num lote só (um envio; no modo assíncrono sem esperar a GPU)
        let alphas: Vec<f32> = (1..=m).map(|i| i as f32 / (m + 1) as f32).collect();
        let t0 = std::time::Instant::now();
        let mids = self.interp_render_batch(req, prev, cur, &pr, &alphas, true, hash)?;
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        self.ip.mids_ms += ms;
        let each = ms / m as f32;
        self.ip.cost_ms = if self.ip.cost_ms == 0.0 { each } else { 0.8 * self.ip.cost_ms + 0.2 * each };
        self.ip.st.mids += m as u32;
        self.ip.st.cost_ms += ms as f64;
        Ok(Some(mids))
    }

    /// Tempo de GPU desde o último quadro (timestamps já recolhidos): o do trabalho que não é intermediário vira
    /// `gpu_frame_ms`; o dos intermediários terminados entra na média de `gpu_mid_ms`. Atrasa até a GPU terminar os
    /// envios (lotes em voo), o que numa média de quadros não importa.
    unsafe fn interp_gpu_budget(&mut self) {
        self.poll();
        let now = (self.gpu_work_ns, self.gpu_mid_ns, self.gpu_mid_n);
        if let Some((w0, m0, n0)) = self.ip.gpu_mark {
            self.ip.gpu_frame_ms = (now.0 - w0) as f32 / 1e6;
            if now.2 > n0 {
                let each = (now.1 - m0) as f32 / 1e6 / (now.2 - n0) as f32;
                self.ip.gpu_mid_ms = if self.ip.gpu_mid_ms == 0.0 { each } else { 0.8 * self.ip.gpu_mid_ms + 0.2 * each };
            }
        }
        self.ip.gpu_mark = Some(now);
    }

    /// Re-renderiza o quadro em α e espera a imagem (validação e teste): o clear inicial do quadro novo e os desenhos
    /// dele com os uniforms interpolados. `hash` = hash do framebuffer nativo do quadro real (o que a imagem carrega).
    unsafe fn interp_render(&mut self, req: PresentReq, prev: &FrameCap, cur: &FrameCap, pr: &Pairing, alpha: f32, hash: u64) -> Result<HiRes, String> {
        self.interp_render_as(req, prev, cur, pr, alpha, true, hash)
    }

    /// Como `interp_render`; `decompor = false` interpola as matrizes elemento a elemento (só a validação usa).
    #[allow(clippy::too_many_arguments)]
    unsafe fn interp_render_as(&mut self, req: PresentReq, prev: &FrameCap, cur: &FrameCap, pr: &Pairing, alpha: f32, decompor: bool, hash: u64) -> Result<HiRes, String> {
        let mut outs = self.interp_render_batch(req, prev, cur, pr, &[alpha], decompor, hash)?;
        let o = outs.pop().ok_or("interpolação: nenhuma saída")?;
        self.out_take(&o)
    }

    /// Re-renderiza o quadro em cada α de `alphas` num lote só (um envio, sem esperar a GPU no modo assíncrono): para
    /// cada α, o clear inicial do quadro novo no alvo de re-renderização, os desenhos com os uniforms interpolados e o
    /// `present.comp` para uma saída própria do anel — em sequência no buffer da sombra, com barreiras entre um e outro
    /// (o mesmo alvo é reaproveitado). Devolve as saídas, na ordem de `alphas`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn interp_render_batch(
        &mut self,
        req: PresentReq,
        prev: &FrameCap,
        cur: &FrameCap,
        pr: &Pairing,
        alphas: &[f32],
        decompor: bool,
        hash: u64,
    ) -> Result<Vec<OutRef>, String> {
        let rt = self.interp_target_ready(cur)?;
        let mut outs: Vec<OutRef> = Vec::with_capacity(alphas.len());
        let mut res = Ok(());
        for &alpha in alphas {
            match self.interp_record_mid(req, prev, cur, pr, alpha, decompor, hash, rt) {
                Ok(o) => outs.push(o),
                Err(e) => {
                    res = Err(e);
                    break;
                }
            }
        }
        self.replay_tex = None;
        self.replay = None;
        self.hr.aa.remove(&rt.ck);
        let res = res.and_then(|_| self.flush());
        if let Err(e) = res {
            // o que já foi gravado pode ter ido (ou ir) para a GPU: as saídas só são soltas depois que ela terminar
            let _ = self.flush();
            let _ = self.wait_all();
            for o in &outs {
                self.out_release(o);
            }
            return Err(e);
        }
        Ok(outs)
    }

    /// Abre o lote dos re-renders e garante o alvo de re-renderização (N× do alvo do quadro; recriado se mudou de
    /// tamanho, o antigo destruído depois dos envios em voo).
    unsafe fn interp_target_ready(&mut self, cur: &FrameCap) -> Result<hires::ReplayTarget, String> {
        let n = self.hr.scale;
        let c0 = &cur.draws[0].ctx;
        let (w, h) = (c0.width, c0.height_m1 + 1);
        let (iw, ih) = (w * n, h * n);
        self.begin()?;
        if self.hr.rimg.as_ref().is_none_or(|r| (r.0, r.1) != (iw, ih)) {
            if let Some((_, _, c, d)) = self.hr.rimg.take() {
                self.defer(Res::Img(c));
                self.defer(Res::Img(d));
            }
            let cimg = self.image(
                iw,
                ih,
                COLOR_FMT,
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST,
                vk::ImageAspectFlags::COLOR,
            )?;
            let dimg = self.image(
                iw,
                ih,
                DEPTH_FMT,
                vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_DST,
                vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
            )?;
            self.init_layout(cimg.image, vk::ImageAspectFlags::COLOR);
            self.init_layout(dimg.image, vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL);
            self.hr.rimg = Some((iw, ih, cimg, dimg));
        }
        let r = self.hr.rimg.as_ref().unwrap();
        let ck = SurfKey { addr: 0xFFFE_0000, w, h, fmt: c0.color_fmt, depth: false };
        let dk = SurfKey { addr: 0xFFFE_8000, w, h, fmt: c0.depth_fmt, depth: true };
        Ok(hires::ReplayTarget { ck, dk, cview: r.2.view, dview: r.3.view, cimage: r.2.image, dimage: r.3.image, n })
    }

    /// Grava um re-render (α) no lote aberto: fecha a passada da sombra, barreira (o present anterior lê o alvo), clear
    /// inicial, os desenhos, e o present para uma saída do anel. Timestamps nas fronteiras (`TS_MID..`): o tempo de
    /// GPU de cada intermediário (orçamento da interpolação).
    #[allow(clippy::too_many_arguments)]
    unsafe fn interp_record_mid(
        &mut self,
        req: PresentReq,
        prev: &FrameCap,
        cur: &FrameCap,
        pr: &Pairing,
        alpha: f32,
        decompor: bool,
        hash: u64,
        rt: hires::ReplayTarget,
    ) -> Result<OutRef, String> {
        // um flush no meio do re-render anterior (buffers cheios) pode ter fechado o lote
        self.begin()?;
        let d = self.device.clone();
        let cs = self.cmd_shadow;
        self.end_pass_s();
        self.full_barrier(cs);
        if self.mid_b == 0 {
            d.cmd_write_timestamp(cs, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.ts_pool, self.ts_base + TS_MID);
            self.mid_b = 1;
        }
        // conteúdo inicial: o clear que o jogo fez no começo do quadro
        let col = cur.init_color.unwrap();
        let (z, st) = cur.init_depth.unwrap();
        d.cmd_clear_color_image(cs, rt.cimage, vk::ImageLayout::GENERAL, &vk::ClearColorValue { float32: col }, &[range(vk::ImageAspectFlags::COLOR)]);
        // profundidade: com o layout ótimo na passada (`dopt`) o clear vai no loadOp da 1ª passada do replay (`draw`);
        // sem desenho nenhum ela não é lida (só a cor vai para o present)
        if dopt() {
            self.replay_dclear = Some((z, st));
        } else {
            self.replay_dclear = None;
            d.cmd_clear_depth_stencil_image(
                cs,
                rt.dimage,
                vk::ImageLayout::GENERAL,
                &vk::ClearDepthStencilValue { depth: z, stencil: st },
                &[range(vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL)],
            );
        }
        self.full_barrier(cs);
        self.hr.aa.remove(&rt.ck);
        self.replay = Some(rt);
        let hud_lerp = hud_lerp();
        for (i, cap) in cur.draws.iter().enumerate() {
            // HUD/2D (sem teste de profundidade): o texto e os ícones não se movem com a câmera — interpolar a matriz
            // deles só borra/desloca; usa o shading do quadro novo
            let scene = cap.dtest && cap.ctx.depth_test;
            let sh = match pr.pair[i] {
                Some(j) if !pr.jumpy[i] && (scene || hud_lerp) => lerp_shading(&prev.draws[j].shading, &cap.shading, alpha, decompor),
                // salto em parte dos uniforms: só essas linhas vão para o quadro novo (`jump_rows`)
                Some(j) if pr.jumpy[i] && (scene || hud_lerp) && !jump_whole_draw() => {
                    let (a, b) = (&prev.draws[j].shading, &cap.shading);
                    lerp_shading_snap(a, b, alpha, decompor, &jump_rows(a, b))
                }
                _ => cap.shading.clone(),
            };
            let slices: Vec<&[u8]> = cap.data.iter().map(|a| &a[..]).collect();
            let dr = VsDraw { indices: &cap.indices, spans: &cap.spans, shading: &sh, prog_gen: cap.prog_gen, code: &cap.code, desc: &cap.desc, data: Some(&slices) };
            // as versões de textura capturadas com o desenho (o replay não relê a memória do jogo)
            self.replay_tex = Some(cap.tex);
            self.draw(&cap.ctx, Geo::Shaded(&dr))?;
        }
        self.replay_tex = None;
        self.replay_dclear = None;
        // o present vai no lote aberto agora (um flush no meio dos desenhos abre outro)
        self.begin()?;
        let cs = self.cmd_shadow;
        self.end_pass_s();
        self.full_barrier(cs);
        let o = self.present_record(cs, req, rt.ck, hash)?;
        if self.mid_b > 0 && TS_MID + self.mid_b < TS_MID_END {
            d.cmd_write_timestamp(cs, vk::PipelineStageFlags::BOTTOM_OF_PIPE, self.ts_pool, self.ts_base + TS_MID + self.mid_b);
            self.mid_b += 1;
        }
        Ok(o)
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn rot_z(graus: f64, escala: f64, t: [f32; 3]) -> VsShading {
        let (s, c) = graus.to_radians().sin_cos();
        let mut v: VsShading = unsafe { std::mem::zeroed() };
        v.f[4] = [(c * escala) as f32, (-s * escala) as f32, 0.0, t[0]];
        v.f[5] = [(s * escala) as f32, (c * escala) as f32, 0.0, t[1]];
        v.f[6] = [0.0, 0.0, escala as f32, t[2]];
        // uma "projeção" (linhas ortogonais de tamanhos diferentes): não pode ser decomposta
        v.f[0] = [0.0, 1.2, 0.0, 0.0];
        v.f[1] = [2.0, 0.0, 0.0, 0.0];
        v.f[2] = [0.0, 0.0, -1.0, 3.0];
        v.f[3] = [0.0, 0.0, -1.0, 0.0];
        v
    }

    #[test]
    fn rotacao_no_meio_nao_encolhe() {
        let (a, b) = (rot_z(0.0, 2.0, [0.0, 0.0, 0.0]), rot_z(90.0, 2.0, [4.0, 0.0, -2.0]));
        let m = lerp_shading(&a, &b, 0.5, true);
        let r = 45f64.to_radians();
        let esperado = [(r.cos() * 2.0) as f32, (-r.sin() * 2.0) as f32, 0.0, 2.0];
        for c in 0..4 {
            assert!((m.f[4][c] - esperado[c]).abs() < 1e-5, "c4.{c}: {} vs {}", m.f[4][c], esperado[c]);
        }
        assert!((m.f[6][2] - 2.0).abs() < 1e-6 && (m.f[6][3] + 1.0).abs() < 1e-6);
        // o lerp elemento a elemento daria escala 2·cos(45°) ≈ 1,41 na diagonal
        let n = (m.f[4][0].powi(2) + m.f[4][1].powi(2)).sqrt();
        assert!((n - 2.0).abs() < 1e-5, "escala {n}");
    }

    #[test]
    fn projecao_e_iguais_ficam_como_estao() {
        let (a, b) = (rot_z(10.0, 1.0, [1.0, 2.0, 3.0]), rot_z(10.0, 1.0, [1.0, 2.0, 3.0]));
        let m = lerp_shading(&a, &b, 0.3, true);
        assert_eq!(m.f, b.f);
        assert!(rigid(&a.f, 0).is_none(), "projeção não é rígida");
        assert!(rigid(&a.f, 4).is_some());
        // espelho (determinante negativo) não é decomposto
        let mut e = a;
        e.f[6][2] = -1.0;
        assert!(rigid(&e.f, 4).is_none());
    }

    #[test]
    fn alfa_um_e_o_quadro_novo_exato() {
        let (a, b) = (rot_z(5.0, 1.5, [0.0, 0.0, 0.0]), rot_z(37.0, 1.5, [9.0, 1.0, 0.0]));
        assert_eq!(lerp_shading(&a, &b, 1.0, true).f, b.f);
    }

    /// Casa com vidro (visto no jogo, Uptown): uma linha de coordenada de textura do reflexo dá a volta (`c41.3`
    /// −0,34 → 0,62) e marca o desenho como "pulo". Só essa linha pode ir para o quadro novo; a geometria (matriz do
    /// modelo) tem de sair igual à de um desenho vizinho sem salto (a parede), senão as camadas coplanares se
    /// desencontram no intermediário e o vidro pisca.
    #[test]
    fn salto_so_na_linha_que_salta() {
        let (a, b) = (rot_z(0.0, 1.0, [0.0, 0.0, 0.0]), rot_z(20.0, 1.0, [3.0, 1.0, 0.0]));
        let (mut va, mut vb) = (a, b);
        va.f[41] = [-0.113571, 0.09470232, 0.9890061, -0.3429414];
        vb.f[41] = [-0.103440285, -0.082323305, 0.9912229, 0.6247641];
        assert!(!jumpy(&a, &b) && jumpy(&va, &vb));
        let rows = jump_rows(&va, &vb);
        assert_eq!(rows.iter().filter(|&&j| j).count(), 1);
        assert!(rows[41]);
        for t in [0.25f32, 0.5, 0.75] {
            let parede = lerp_shading(&a, &b, t, true);
            let vidro = lerp_shading_snap(&va, &vb, t, true, &rows);
            assert_eq!(vidro.f[41], vb.f[41], "a linha que salta vai para o quadro novo");
            for r in (0..96).filter(|&r| r != 41) {
                assert_eq!(vidro.f[r], parede.f[r], "linha {r} (α {t}): a geometria interpola como a da parede");
            }
        }
    }
}
