//! Backends de GPU do renderizador PICA. O front-end (`pica`) mantém os registradores. Com o Vulkan há dois caminhos
//! de geometria:
//! - **vértices na GPU** (padrão): `pica::vs_gpu` traduz carregadores/uniforms/saídas para `VsShading` + `VsDraw`,
//!   e `shaders/pica_vs.vert` interpreta o shader de vértice do PICA na GPU (mesma semântica de `pica::shader`),
//!   encaixa em 12.4 e deixa recorte e descarte por orientação para o hardware;
//! - **vértices na CPU** (`YW_VK_CPU_VS=1`, reserva): o backend recebe os triângulos já sombreados; esta camada
//!   comum faz o que tem de ser bit a bit igual ao `soft` — recorte nos 7 planos (`raster::clip`), projeção com o
//!   viewport do PICA arredondada a 1/16 de pixel (12.4) e o descarte por orientação.
//! Nos dois, empacota os uniforms do "ubershader" de fragmento (TEV, alpha test, texturas, origem da profundidade).
//!
//! Seleção: `YW_RENDERER=vulkan|soft|none` (padrão `vulkan`, desde 08/11, depois da revisão). Se o Vulkan não inicializar, cai para `soft` com aviso.
//! O DX12 (alternativa futura) vai consumir os mesmos `GpuVtx` / `DrawUniforms`.
//!
//! Memória guest continua sendo a fonte da verdade: o backend carrega os render targets da memória quando mudaram e
//! devolve o resultado para ela no fim de cada lista de comandos com draws (`end_list`).

pub mod aot;
pub mod convert;
pub mod drawprof;
pub mod vulkan;

pub use aot::register_aot;

use crate::pica::raster::{self, Ctx, Vtx};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Soft,
    Vulkan,
    None,
}

/// 0 = ainda não escolhido, 1 soft, 2 vulkan, 3 none
static BACKEND: AtomicU8 = AtomicU8::new(0);

// `code` e `decode` são a MESMA tabela, nos dois sentidos. Já estiveram invertidas (gravava Vulkan = 1 e lia 1 como
// Soft): com o padrão soft, `backend()` passava a devolver Vulkan sem dispositivo e nada era desenhado. O teste
// `backend_code_roundtrip` trava isso.
fn code(b: Backend) -> u8 {
    match b {
        Backend::Soft => 1,
        Backend::Vulkan => 2,
        Backend::None => 3,
    }
}

fn decode(c: u8) -> Option<Backend> {
    match c {
        1 => Some(Backend::Soft),
        2 => Some(Backend::Vulkan),
        3 => Some(Backend::None),
        _ => None,
    }
}

/// Backend em uso (escolhido na primeira chamada por `YW_RENDERER`).
pub fn backend() -> Backend {
    match decode(BACKEND.load(Ordering::Relaxed)) {
        Some(b) => b,
        None => {
            let want = match std::env::var("YW_RENDERER").as_deref() {
                Ok("none") => Backend::None,
                Ok("vulkan") | Ok("vk") | Err(_) => Backend::Vulkan,
                Ok("soft") => Backend::Soft,
                Ok(o) => {
                    eprintln!("[render] YW_RENDERER={o} desconhecido; usando soft");
                    Backend::Soft
                }
            };
            set_backend(want)
        }
    }
}

/// Troca o backend (testes / opção do usuário). Devolve o que ficou valendo (Vulkan → Soft se falhar).
pub fn set_backend(b: Backend) -> Backend {
    let got = if b == Backend::Vulkan {
        match vulkan::init() {
            Ok(name) => {
                eprintln!("[render] backend Vulkan: {name}");
                Backend::Vulkan
            }
            Err(e) => {
                eprintln!("[render] Vulkan não inicializou ({e}); usando o backend soft");
                Backend::Soft
            }
        }
    } else {
        b
    };
    BACKEND.store(code(got), Ordering::Relaxed);
    got
}

/// Alguém (GSP) vai ler `[lo, hi)` da memória guest: o backend de GPU devolve antes o que só está nele.
pub fn before_read(lo: u32, hi: u32) {
    if backend() == Backend::Vulkan {
        crate::pica::timed(3, || vulkan::before_read(lo, hi));
    }
}

/// Como `before_read`, para o DisplayTransfer de um alvo para o LCD. `hidden` = a saída vai para um framebuffer que a
/// janela não mostra (olho direito do 3D, ou nenhuma tela): a sombra N× da cena desse lote não é executada.
pub fn before_read_lcd(lo: u32, hi: u32, hidden: bool) {
    if backend() == Backend::Vulkan {
        crate::pica::timed(3, || vulkan::before_read_lcd(lo, hi, hidden));
    }
}

/// Alguém (GSP) vai escrever em `[lo, hi)`.
pub fn before_write(lo: u32, hi: u32) {
    if backend() == Backend::Vulkan {
        crate::pica::timed(3, || vulkan::before_write(lo, hi));
    }
}

/// Um MemoryFill terminou em `[lo, hi)` com padrão de `fill_bpp` bytes.
pub fn after_fill(lo: u32, hi: u32, fill_bpp: u32) {
    if backend() == Backend::Vulkan {
        crate::pica::timed(3, || vulkan::after_fill(lo, hi, fill_bpp));
    }
}

/// Fim de uma lista de comandos que desenhou: o backend de GPU executa o lote (a devolução para a memória guest é
/// preguiçosa: acontece em `before_read`/`before_write`).
/// DisplayTransfer de um alvo de tela para o framebuffer do LCD terminou: com resolução interna/FXAA, o backend Vulkan
/// monta a imagem final em N× (ver `vulkan::hires`).
pub fn present_hires(req: vulkan::PresentReq) {
    if backend() == Backend::Vulkan && hires_on() {
        // PROF[6]: imagem final em N× + intermediários da interpolação (linha de desempenho do log)
        crate::pica::timed(6, || vulkan::present_hires(req));
    }
}

/// Primeiro passo da imagem N× (PPF adiantado): roda ANTES de o GSP sinalizar o fim do DisplayTransfer, enquanto o
/// alvo e as texturas do quadro ainda estão intactos. `None` = nada a fazer depois (sem Vulkan, sem sombra…).
pub fn present_hires_check(req: vulkan::PresentReq) -> Option<vulkan::PresentChecked> {
    if backend() == Backend::Vulkan && hires_on() {
        // PROF[6]: imagem final em N× + intermediários da interpolação (linha de desempenho do log)
        crate::pica::timed(6, || vulkan::present_hires_check(req))
    } else {
        None
    }
}

/// Segundo passo (depois do PPF): imagem N×, intermediários da interpolação e publicação para a janela.
pub fn present_hires_finish(c: vulkan::PresentChecked) {
    crate::pica::timed(6, || vulkan::present_hires_finish(c));
}

pub fn end_list() {
    if backend() == Backend::Vulkan {
        vulkan::end_list();
    }
}

/// Vértice pronto para a GPU (56 bytes, layout de `shaders/pica.vert`).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct GpuVtx {
    /// coordenadas de recorte do Vulkan: (x_ndc·w, y_ndc·w, profundidade·w, w)
    pub pos: [f32; 4],
    /// |cor| saturada em [0, 1] (como o soft faz por vértice antes de interpolar)
    pub color: [f32; 4],
    pub tc0: [f32; 2],
    pub tc1: [f32; 2],
    pub tc2: [f32; 2],
}

fn area(a: (i32, i32), b: (i32, i32), p: (i32, i32)) -> i64 {
    (b.0 - a.0) as i64 * (p.1 - a.1) as i64 - (b.1 - a.1) as i64 * (p.0 - a.0) as i64
}

/// Recorta, projeta e descarta por orientação um triângulo exatamente como `raster::triangle`, e acrescenta os
/// triângulos que sobram (lista) em `out`. A imagem da GPU usa linha = y do rasterizador do PICA; o viewport do
/// Vulkan cobre o render target inteiro e a posição já vem com o viewport do PICA aplicado.
pub fn prepare(c: &Ctx, a: &Vtx, b: &Vtx, d: &Vtx, out: &mut Vec<GpuVtx>) {
    let poly = raster::clip([*a, *b, *d]);
    if poly.len() < 3 {
        return;
    }
    let (fw, fh) = (c.width.max(1) as f32, (c.height_m1 + 1) as f32);
    let s: Vec<((i32, i32), GpuVtx)> = poly
        .iter()
        .map(|v| {
            let w = v.pos[3];
            let inv_w = 1.0 / w;
            let sx = (v.pos[0] * inv_w + 1.0) * c.vp[0] + c.vp[2];
            let sy = (v.pos[1] * inv_w + 1.0) * c.vp[1] + c.vp[3];
            // valor do z-buffer (z/w·zscale + zoffset), interpolado na tela pelo gl_FragCoord.z; com w-buffer o pica.frag
            // divide por gl_FragCoord.w (`DEPTH_WBUF`), o que dá z·zscale + w·zoffset com correção de perspectiva
            let depth = c.zoffset + v.pos[2] * inv_w * c.zscale;
            // 12.4 como o soft; a GPU (≥ 4 bits de subpixel, normalmente 8) reproduz a mesma grade
            let (x16, y16) = ((sx * 16.0).round() as i32, (sy * 16.0).round() as i32);
            let nx = (x16 as f32 / 16.0) / fw * 2.0 - 1.0;
            let ny = (y16 as f32 / 16.0) / fh * 2.0 - 1.0;
            let col = v.color.map(|k| k.abs().min(1.0));
            (
                (x16, y16),
                GpuVtx {
                    pos: [nx * w, ny * w, depth * w, w],
                    color: col,
                    tc0: v.tc0,
                    tc1: v.tc1,
                    tc2: v.tc2,
                },
            )
        })
        .collect();
    for i in 1..s.len() - 1 {
        let (v0, v1, v2) = (&s[0], &s[i], &s[i + 1]);
        let ar = area(v0.0, v1.0, v2.0);
        // raster.rs::raster: modo 0 desenha as duas faces; 1 só as de área negativa; 2/3 só as positivas.
        // Área zero não gera fragmentos.
        let keep = match c.cull {
            0 => ar != 0,
            1 => ar < 0,
            _ => ar > 0,
        };
        if keep {
            out.extend_from_slice(&[v0.1, v1.1, v2.1]);
        }
    }
}

/// `YW_VK_CPU_VS=1`: o backend Vulkan recebe vértices já sombreados pela CPU (`pica::shader`, o caminho antigo) em
/// vez de interpretar o shader de vértice na GPU (`shaders/pica_vs.vert`).
pub fn cpu_vs_forced() -> bool {
    match CPU_VS.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var("YW_VK_CPU_VS").is_ok_and(|v| v != "0");
            set_cpu_vs(on);
            on
        }
    }
}

/// 0 = ainda não lido do ambiente, 1 = vértices na GPU, 2 = vértices na CPU
static CPU_VS: AtomicU8 = AtomicU8::new(0);

/// Força (ou não) o shader de vértice na CPU com o Vulkan — o mesmo que `YW_VK_CPU_VS` (testes comparam os dois).
pub fn set_cpu_vs(on: bool) {
    CPU_VS.store(if on { 2 } else { 1 }, Ordering::Relaxed);
}

/// Shader de vértice na GPU? (backend Vulkan, dispositivo com o necessário e sem `YW_VK_CPU_VS`)
pub fn gpu_vs() -> bool {
    backend() == Backend::Vulkan && !cpu_vs_forced() && vulkan::vs_supported()
}

/// Semântica sem registrador de saída (`VsShading::sem`).
pub const SEM_NONE: u32 = u32::MAX;
pub const VS_F_WBUFFER: u32 = 1;
pub const VS_F_USER_CLIP: u32 = 2;

/// Parâmetros por draw do shader de vértice na GPU (std430, mesmo layout de `Shading` em `shaders/pica_vs.vert`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VsShading {
    /// uniforms float c0..c95
    pub f: [[f32; 4]; 96],
    /// uniforms int: x = contagem, y = início de aL, z = passo (com sinal)
    pub i: [[i32; 4]; 4],
    /// x = uniforms bool, y = ponto de entrada, z = flags `VS_F_*`
    pub misc: [u32; 4],
    /// semântica k (0..23) → registrador de saída·4 + componente, ou `SEM_NONE`
    pub sem: [[u32; 4]; 6],
    /// viewport do PICA: meia largura, meia altura, x, y
    pub vp: [f32; 4],
    /// x = zscale, y = zoffset
    pub zmap: [f32; 4],
    /// plano de recorte do usuário
    pub clip: [f32; 4],
    /// por registrador de entrada: x = byte do vértice 0 nos dados enviados, y = passo, z = tipo | componentes << 8
    pub attr: [[u32; 4]; 16],
    /// valor do registrador onde o atributo não dá componente
    pub defs: [[f32; 4]; 16],
}

/// Um draw com o shader de vértice na GPU: índices já em lista de triângulos (relativos ao 1º vértice enviado), os
/// trechos da memória guest com os arranjos de vértices (enviados um atrás do outro, cada um arredondado a 4
/// bytes — `VsShading::attr` já aponta para lá) e o programa (com a geração de `pica::State`, que muda a cada
/// escrita de código/descritor: o backend só recalcula o hash do conteúdo quando ela muda).
pub struct VsDraw<'a> {
    pub indices: &'a [u32],
    pub spans: &'a [(u32, u32)],
    pub shading: &'a VsShading,
    pub prog_gen: u64,
    pub code: &'a [u32],
    pub desc: &'a [u32],
    /// bytes dos trechos de `spans` (um por trecho) quando NÃO vêm da memória guest agora: a interpolação de quadros
    /// re-renderiza desenhos capturados antes, cujos vértices a memória já pode ter reescrito
    pub data: Option<&'a [&'a [u8]]>,
}

/// Uniforms do ubershader (std140, mesmo layout de `Draw` em `shaders/pica.frag`).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct DrawUniforms {
    pub tev: [[u32; 4]; 6],
    pub scale: [[u32; 4]; 2],
    pub misc: [u32; 4],
    pub scissor: [u32; 4],
    pub tex: [[u32; 4]; 3],
    pub depth: [u32; 4],
}

pub const F_LIGHTING: u32 = 1;
pub const F_TEX2_TC1: u32 = 2;
pub const F_SCISSOR_EXCLUDE: u32 = 4;
pub const F_TEX0: u32 = 16;
/// bits 8–11 de `misc.w`: n → até 2^n amostras por textura no filtro anisotrópico (0 = desligado)
pub const F_ANISO_SHIFT: u32 = 8;

static ANISO: AtomicU32 = AtomicU32::new(u32::MAX);
static SCALE: AtomicU32 = AtomicU32::new(u32::MAX);
static FXAA: AtomicU8 = AtomicU8::new(2);
static VK_ASYNC: AtomicU8 = AtomicU8::new(2);
static HIRES_FORCE: AtomicU8 = AtomicU8::new(0);
static INTERP: AtomicU32 = AtomicU32::new(u32::MAX);

/// Interpolação de quadros: taxa de saída desejada em Hz (0 = desligada; 60, 90, 120, 144, 180). `YW_INTERP` define o
/// valor inicial; o menu usa `set_interp`. Só no backend Vulkan (ver `vulkan::interp`).
pub fn interp_target() -> u32 {
    match INTERP.load(Ordering::Relaxed) {
        u32::MAX => {
            let n = std::env::var("YW_INTERP").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            set_interp(n);
            interp_target()
        }
        n => n,
    }
}

pub fn set_interp(hz: u32) {
    INTERP.store(if hz < 40 { 0 } else { hz.min(240) }, Ordering::Relaxed);
}

/// Resolução interna: fator N (1–6) da imagem final das telas. Com N > 1 o backend Vulkan desenha cada alvo de tela
/// uma segunda vez, em N× de resolução (a "sombra"), só para a imagem que vai à janela; os alvos nativos (o que o jogo
/// lê de volta) não mudam. `YW_SCALE` define o valor inicial; o menu o troca com `set_scale`.
pub fn scale() -> u32 {
    match SCALE.load(Ordering::Relaxed) {
        u32::MAX => {
            let n = std::env::var("YW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
            set_scale(n);
            scale()
        }
        n => n,
    }
}

pub fn set_scale(n: u32) {
    SCALE.store(n.clamp(1, 6), Ordering::Relaxed);
}

/// FXAA só na cena 3D da sombra (nunca no texto/HUD). `YW_FXAA=1` liga; o menu usa `set_fxaa`.
pub fn fxaa() -> bool {
    match FXAA.load(Ordering::Relaxed) {
        2 => {
            let on = std::env::var("YW_FXAA").is_ok_and(|v| v != "0");
            set_fxaa(on);
            on
        }
        v => v != 0,
    }
}

pub fn set_fxaa(on: bool) {
    FXAA.store(on as u8, Ordering::Relaxed);
}

/// Renderização assíncrona no Vulkan (a GPU do PC trabalha em paralelo com o jogo; `graficos.renderizacao`).
/// `YW_VK_ASYNC=0/1` define o valor inicial e vence o arquivo; a janela usa `set_async` no boot. (Motor da F3: por
/// enquanto ninguém lê.)
pub fn async_on() -> bool {
    match VK_ASYNC.load(Ordering::Relaxed) {
        2 => {
            let on = std::env::var("YW_VK_ASYNC").map_or(true, |v| v != "0");
            set_async(on);
            on
        }
        v => v != 0,
    }
}

pub fn set_async(on: bool) {
    VK_ASYNC.store(on as u8, Ordering::Relaxed);
}

/// Força a sombra mesmo com N = 1 e sem FXAA (testes: a sombra 1× tem de reproduzir a imagem nativa).
pub fn set_hires_force(on: bool) {
    HIRES_FORCE.store(on as u8, Ordering::Relaxed);
}

/// O backend Vulkan mantém a sombra N× dos alvos de tela?
pub fn hires_on() -> bool {
    scale() > 1 || fxaa() || HIRES_FORCE.load(Ordering::Relaxed) != 0 || interp_target() > 0 || vulkan::interp_capturing()
}

/// Amostras máximas do filtro anisotrópico (0 = desligado, 2, 4, 8 ou 16). `YW_ANISO` define o valor inicial; o menu
/// o troca com `set_aniso`. Só o backend Vulkan usa (o PICA real não tem esse filtro; o soft é a referência exata).
pub fn aniso() -> u32 {
    match ANISO.load(Ordering::Relaxed) {
        u32::MAX => {
            let n = std::env::var("YW_ANISO").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
            set_aniso(n);
            aniso()
        }
        n => n,
    }
}

pub fn set_aniso(n: u32) {
    let n = match n {
        0 | 1 => 0,
        2..=3 => 2,
        4..=7 => 4,
        8..=15 => 8,
        _ => 16,
    };
    ANISO.store(n, Ordering::Relaxed);
}

/// Origem da profundidade no pica.frag (`DrawUniforms::depth[3]`): `DEPTH_FRAGCOORD` (vértices da CPU: gl_FragCoord.z
/// já é z/w·zscale + zoffset) ou `DEPTH_ZNDC` (vértices da GPU: zoffset − zscale·gl_FragCoord.z), mais o bit
/// `DEPTH_WBUF` com o w-buffer ligado (divide por gl_FragCoord.w: (z/w·zscale + zoffset)·w, como o PICA/Citra).
pub const DEPTH_FRAGCOORD: u32 = 0;
pub const DEPTH_ZNDC: u32 = 1;
pub const DEPTH_WBUF: u32 = 4;

/// Empacota o estado do fragmento de um draw.
pub fn uniforms(c: &Ctx) -> DrawUniforms {
    let mut u = DrawUniforms::default();
    for (i, s) in c.stages.iter().enumerate() {
        let src = s.src_rgb[0] | s.src_rgb[1] << 4 | s.src_rgb[2] << 8 | s.src_a[0] << 16 | s.src_a[1] << 20 | s.src_a[2] << 24;
        let op = s.op_rgb[0] | s.op_rgb[1] << 4 | s.op_rgb[2] << 8 | s.op_a[0] << 12 | s.op_a[1] << 16 | s.op_a[2] << 20;
        u.tev[i] = [src, op, s.comb_rgb | s.comb_a << 16, u32::from_le_bytes(s.konst)];
        u.scale[i / 4][i % 4] = s.scale_rgb | s.scale_a << 16;
    }
    let (at, af, aref) = c.alpha_test;
    let mut flags = 0;
    if c.lighting {
        flags |= F_LIGHTING;
    }
    if c.tex2_uses_tc1 {
        flags |= F_TEX2_TC1;
    }
    if c.scissor.0 == 1 {
        flags |= F_SCISSOR_EXCLUDE;
    }
    for (i, t) in c.tex.iter().enumerate() {
        if let Some(t) = t {
            flags |= F_TEX0 << i;
            // textura sem endereço/tamanho: o soft devolve (0, 0, 0, 255) — largura 0 faz o mesmo no shader
            let (w, h) = if t.addr == 0 || t.w == 0 || t.h == 0 { (0, 0) } else { (t.w, t.h) };
            u.tex[i] = [w | h << 16, t.wrap_s | t.wrap_t << 4 | (t.linear as u32) << 8, u32::from_le_bytes(t.border), 0];
        }
    }
    let an = aniso();
    if an != 0 {
        flags |= an.trailing_zeros() << F_ANISO_SHIFT; // 2→1, 4→2, 8→3, 16→4
    }
    u.misc = [u32::from_le_bytes(c.buffer_color), c.buffer_update, at as u32 | af << 4 | (aref as u32) << 8, flags];
    let sc = c.scissor.1;
    u.scissor = [sc[0], sc[1], sc[2], sc[3]];
    u.depth = [if c.depth_fmt == 0 { 0xffff } else { 0xff_ffff }, c.zscale.to_bits(), c.zoffset.to_bits(), DEPTH_FRAGCOORD | if c.wbuffer { DEPTH_WBUF } else { 0 }];
    u.tex[0][3] = tev_active(c);
    u
}

/// Quantos estágios do TEV o pica.frag precisa executar: até o último que não é passagem direta. Passagem direta =
/// fonte 0 "anterior" (15), operando 0, combinador Replace (0) e escala 1, em RGB e em alfa: devolve a cor que entrou
/// sem ler mais nada, e os estágios seguintes (também passagem direta) não leem o buffer de combinação, então pular
/// esses estágios no fim não muda o resultado.
pub fn tev_active(c: &Ctx) -> u32 {
    let pass = |s: &crate::pica::raster::Stage| {
        s.src_rgb[0] == 15 && s.op_rgb[0] == 0 && s.comb_rgb == 0 && s.scale_rgb == 0 && s.src_a[0] == 15 && s.op_a[0] == 0 && s.comb_a == 0 && s.scale_a == 0
    };
    c.stages.iter().rposition(|s| !pass(s)).map_or(0, |i| i as u32 + 1)
}

/// Bytes por pixel do color buffer (formatos de framebuffer do PICA).
pub fn color_bpp(fmt: u32) -> u32 {
    crate::hle::gpu::bpp(fmt)
}

/// Bytes por pixel do depth buffer: 0 D16, 1 D24 (sem uso), 2 D24, 3 D24S8.
pub fn depth_bpp(fmt: u32) -> u32 {
    match fmt {
        0 => 2,
        1 | 2 => 3,
        _ => 4,
    }
}

/// Hash rápido de um bloco de memória (4 vias de multiplicação; não criptográfico) para o cache de texturas.
pub fn hash_bytes(p: &[u8]) -> u64 {
    const K: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = [K ^ p.len() as u64, 0x243F_6A88_85A3_08D3, 0x1319_8A2E_0370_7344, 0xA409_3822_299F_31D0];
    let mut chunks = p.chunks_exact(32);
    for c in &mut chunks {
        for (l, hv) in h.iter_mut().enumerate() {
            let w = u64::from_le_bytes(c[8 * l..8 * l + 8].try_into().unwrap());
            *hv = (*hv ^ w).wrapping_mul(K).rotate_left(29);
        }
    }
    for (i, &b) in chunks.remainder().iter().enumerate() {
        h[i & 3] = (h[i & 3] ^ b as u64).wrapping_mul(K);
    }
    let mut r = 0u64;
    for hv in h {
        r = (r ^ hv).wrapping_mul(K).rotate_left(31);
    }
    r ^ (r >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O código gravado no atômico tem de voltar como o mesmo backend (e 0 = "ainda não escolhido").
    #[test]
    fn backend_code_roundtrip() {
        for b in [Backend::Soft, Backend::Vulkan, Backend::None] {
            assert_ne!(code(b), 0);
            assert_eq!(decode(code(b)), Some(b));
        }
        assert_eq!(decode(0), None);
    }
}
