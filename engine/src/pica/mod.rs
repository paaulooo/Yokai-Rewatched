//! Front-end do PICA200 + backend de referência por software (`soft`).
//!
//! Recebe as listas de comando reais do jogo (via GSP, `hle::gpu::submit_cmdlist`), mantém os registradores, carrega
//! vértices, roda o shader de vértice (`shader.rs`), monta e recorta primitivas e rasteriza (`raster.rs`) direto no
//! color/depth buffer da memória guest (blocos 8×8), que o DisplayTransfer do GSP já converte para a tela.
//!
//! Registradores: 3dbrew "GPU/Internal Registers" e `regs_*.h` do Citra/Azahar. Com `YW_RENDERER=vulkan` (padrão)
//! o draw vai para o backend de GPU (`crate::render`) em vez de `raster.rs`: normalmente com o shader de vértice
//! interpretado na própria GPU (`vs_gpu.rs` prepara os registradores; `render/shaders/pica_vs.vert`); com
//! `YW_VK_CPU_VS=1` (ou sem suporte) o shader roda aqui na CPU e só os triângulos sombreados vão para a GPU.

pub mod raster;
pub mod shader;
pub mod texture;
mod vs_gpu;

use crate::hle::mem::pa_to_va;
use raster::{Ctx, Stage, Vtx};
use shader::{f24, unpack_f24x4, Exec, Program, V4};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use texture::Tex;
use yw1_rt::{r16, r32, r8};

pub static LISTS: AtomicU64 = AtomicU64::new(0);
pub static DRAWS: AtomicU64 = AtomicU64::new(0);

/// Tempo gasto (ns, acumulado) em cada parte da renderização — o relatório sem janela mostra por segundo.
/// 0 lista inteira · 1 shader de vértice (CPU) · 2 vulkan::draw · 3 sincronismo GPU↔memória (execução do lote e
/// devolução preguiçosa, nos ganchos do GSP; fora da lista)
/// · 4 DisplayTransfer · 5 MemoryFill
pub static PROF: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];

/// Mede `f` e soma em `PROF[i]`.
pub fn timed<R>(i: usize, f: impl FnOnce() -> R) -> R {
    let t = std::time::Instant::now();
    let r = f();
    PROF[i].fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    r
}

/// `YW_RENDERER=none` desliga o desenho (só conta), para medir o resto do jogo; `vulkan` usa o backend de GPU
/// (`crate::render`), `soft` (padrão) o rasterizador de referência deste módulo.
fn enabled() -> bool {
    crate::render::backend() != crate::render::Backend::None
}

pub struct State {
    pub regs: Vec<u32>,
    pub vs: Program,
    float_buf: [u32; 4],
    float_n: usize,
    fixed_buf: [u32; 4],
    fixed_n: usize,
    pub fixed_attr: [V4; 16],
    /// muda a cada escrita no código/descritores do shader de vértice (o backend de GPU só recalcula o hash do
    /// programa quando ela muda); única no processo, inclusive entre `State`s diferentes
    pub prog_gen: u64,
    /// etiquetas por disparo de draw da lista em execução (`None` fora de uma lista com etiquetas) e se a lista teve
    /// as etiquetas descartadas; `etiq_n` = disparos já vistos
    etiq: Option<(Vec<Option<crate::hle::etiquetas::EtiqDraw>>, bool)>,
    etiq_n: usize,
}

static PROG_GEN: AtomicU64 = AtomicU64::new(1);

fn next_prog_gen() -> u64 {
    PROG_GEN.fetch_add(1, Ordering::Relaxed)
}

impl State {
    fn new() -> Self {
        State {
            regs: vec![0; 0x300],
            vs: Program::default(),
            float_buf: [0; 4],
            float_n: 0,
            fixed_buf: [0; 4],
            fixed_n: 0,
            fixed_attr: [[0.0, 0.0, 0.0, 1.0]; 16],
            prog_gen: next_prog_gen(),
            etiq: None,
            etiq_n: 0,
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Executa uma lista de comandos (endereço virtual guest, tamanho em bytes).
pub fn execute(addr: u32, size: u32) {
    let words = read_list(addr, size);
    execute_words(&words);
}

/// Copia uma lista de comandos da memória guest (palavras de 32 bits; vazia se não mapeada).
pub fn read_list(addr: u32, size: u32) -> Vec<u32> {
    let n = (size / 4) as usize;
    if n == 0 || !crate::hle::mem::is_mapped(addr) || !crate::hle::mem::is_mapped(addr + size - 1) {
        return Vec::new();
    }
    (0..n as u32).map(|i| r32(addr + 4 * i)).collect()
}

/// Executa uma lista já copiada (a GPU assíncrona copia no despacho: o jogo pode reescrever o buffer depois).
pub fn execute_words(words: &[u32]) {
    timed(0, || execute_inner(words, None))
}

/// Como `execute_words`, com as etiquetas de draw do jogo da faixa da lista (G1, `hle/etiquetas.rs`): `base` = endereço
/// guest da lista. Cada disparo de draw recebe a sua etiqueta (ou nenhuma) em `etiquetas::draw_atual` enquanto é
/// desenhado — na própria thread que desenha, sem estado entre threads.
pub fn execute_words_etiq(base: u32, words: &[u32], et: Option<&crate::hle::etiquetas::EtiqLista>) {
    use crate::hle::etiquetas as e;
    let por_draw = et.and_then(|l| match e::casar(base, words, l) {
        Ok(v) => {
            e::N_CASADAS.fetch_add(l.itens.len() as u64, Ordering::Relaxed);
            Some((v, false))
        }
        Err(m) => {
            e::lista_ruim(base, &m);
            Some((Vec::new(), true))
        }
    });
    timed(0, || execute_inner(words, por_draw));
    e::set_draw_atual(e::Atual::default());
}

fn execute_inner(words: &[u32], etiq: Option<(Vec<Option<crate::hle::etiquetas::EtiqDraw>>, bool)>) {
    LISTS.fetch_add(1, Ordering::Relaxed);
    let mut g = STATE.lock().unwrap();
    let st = g.get_or_insert_with(State::new);
    st.etiq_n = 0;
    st.etiq = etiq;
    let draws_before = DRAWS.load(Ordering::Relaxed);
    let mut p = 0usize;
    while p + 2 <= words.len() {
        let first = words[p];
        let hdr = words[p + 1];
        let id = hdr & 0xffff;
        let mask = (hdr >> 16) & 0xf;
        let extra = ((hdr >> 20) & 0xff) as usize;
        let consecutive = hdr >> 31 != 0;
        for k in 0..=extra {
            let val = if k == 0 { first } else { words.get(p + 1 + k).copied().unwrap_or(0) };
            let reg = if consecutive { id + k as u32 } else { id };
            write_reg(st, reg, val, mask);
        }
        p += 2 + extra;
        if extra % 2 == 1 {
            p += 1;
        }
    }
    st.etiq = None;
    // backend de GPU: devolve os render targets para a memória guest antes do GSP continuar
    if DRAWS.load(Ordering::Relaxed) != draws_before {
        timed(3, crate::render::end_list);
    }
}

fn write_reg(st: &mut State, reg: u32, val: u32, mask: u32) {
    let reg = reg as usize;
    if reg >= st.regs.len() {
        return;
    }
    let m = (0..4).fold(0u32, |a, i| if mask >> i & 1 != 0 { a | 0xff << (8 * i) } else { a });
    let v = (st.regs[reg] & !m) | (val & m);
    st.regs[reg] = v;
    match reg {
        // desenho
        0x22e | 0x22f => {
            if val != 0 || reg == 0x22f || reg == 0x22e {
                DRAWS.fetch_add(1, Ordering::Relaxed);
                // etiqueta do jogo deste disparo (mesma ordem de `etiquetas::disparos`)
                if let Some((v, ruim)) = &st.etiq {
                    let et = v.get(st.etiq_n).copied().flatten();
                    crate::hle::etiquetas::set_draw_atual(crate::hle::etiquetas::Atual { et, ruim: *ruim });
                    st.etiq_n += 1;
                }
                if enabled() {
                    draw(st, reg == 0x22f);
                }
                if st.etiq.is_some() {
                    crate::hle::etiquetas::set_draw_atual(crate::hle::etiquetas::Atual::default());
                }
            }
        }
        // atributo fixo: índice (0x232) + 3 palavras de dados (0x233–0x235)
        0x232 => st.fixed_n = 0,
        0x233..=0x235 => {
            st.fixed_buf[st.fixed_n] = v;
            st.fixed_n += 1;
            if st.fixed_n == 3 {
                st.fixed_n = 0;
                let idx = (st.regs[0x232] & 0xf) as usize;
                if idx < 12 {
                    st.fixed_attr[idx] = unpack_f24x4(&st.fixed_buf);
                }
                // índice 15 = modo imediato (não usado pelo jogo até agora) [H]
            }
        }
        // uniforms float do shader de vértice
        0x2c0 => st.float_n = 0,
        0x2c1..=0x2c8 => {
            st.float_buf[st.float_n] = v;
            st.float_n += 1;
            let setup = st.regs[0x2c0];
            let f32mode = setup >> 31 != 0;
            if (f32mode && st.float_n == 4) || (!f32mode && st.float_n == 3) {
                st.float_n = 0;
                let idx = (setup & 0xff) as usize;
                if idx < 96 {
                    st.vs.f[idx] = if f32mode {
                        // ordem de escrita: w, z, y, x
                        let b = st.float_buf;
                        [f32::from_bits(b[3]), f32::from_bits(b[2]), f32::from_bits(b[1]), f32::from_bits(b[0])]
                    } else {
                        unpack_f24x4(&st.float_buf)
                    };
                }
                st.regs[0x2c0] = (setup & !0xff) | ((idx as u32 + 1) & 0xff);
            }
        }
        // bool e int uniforms
        0x2b0 => st.vs.b = (v & 0xffff) as u16,
        0x2b1..=0x2b4 => st.vs.i[reg - 0x2b1] = v.to_le_bytes(),
        0x2ba => st.vs.entry = v & 0xffff,
        // código do shader de vértice
        0x2cc..=0x2d3 => {
            let off = (st.regs[0x2cb] & 0xfff) as usize;
            if off < st.vs.code.len() {
                st.vs.code[off] = v;
                st.prog_gen = next_prog_gen();
            }
            st.regs[0x2cb] = (st.regs[0x2cb] & !0xfff) | ((off as u32 + 1) & 0xfff);
        }
        0x2d6..=0x2dd => {
            let off = (st.regs[0x2d5] & 0x7f) as usize;
            if off < st.vs.desc.len() {
                st.vs.desc[off] = v;
                st.prog_gen = next_prog_gen();
            }
            st.regs[0x2d5] = (st.regs[0x2d5] & !0x7f) | ((off as u32 + 1) & 0x7f);
        }
        _ => {}
    }
}

/// Escreve um registrador como se viesse de uma lista (máscara completa).
pub fn set_reg(reg: u32, val: u32) {
    let mut g = STATE.lock().unwrap();
    write_reg(g.get_or_insert_with(State::new), reg, val, 0xf);
}

fn va(pa: u32) -> u32 {
    pa_to_va(pa).unwrap_or(0)
}

/// Lê os atributos do vértice `index` dos 12 carregadores (Citra `vertex_loader.cpp`).
fn debug_on() -> bool {
    static D: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *D.get_or_init(|| std::env::var_os("YW_PICA_DEBUG").is_some())
}

/// Os arranjos de vértices que o draw vai ler (índices `lo..=hi`) estão na memória mapeada?
fn vertex_memory_ok(st: &State, lo: u32, hi: u32) -> bool {
    let r = &st.regs;
    let base = r[0x200] << 3;
    for l in 0..12usize {
        let off = r[0x203 + 3 * l];
        let c_hi = r[0x205 + 3 * l];
        let n = (c_hi >> 28) & 0xf;
        let stride = (c_hi >> 16) & 0xff;
        if n == 0 {
            continue;
        }
        let a = va(base + off);
        let first = a.wrapping_add(lo.wrapping_mul(stride));
        let last = a.wrapping_add(hi.wrapping_add(1).wrapping_mul(stride.max(1))).wrapping_sub(1);
        if !crate::hle::mem::is_mapped(first) || !crate::hle::mem::is_mapped(last) || last < first {
            return false;
        }
    }
    true
}

/// Draw descartado por dados inválidos: avisa (poucas vezes) e conta.
fn bad_draw(why: &str) {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    if n < 5 || n.is_power_of_two() {
        eprintln!("[pica] draw ignorado ({why}) — {} até agora", n + 1);
    }
}

fn load_vertex(st: &State, index: u32, out: &mut [V4; 16]) {
    let r = &st.regs;
    let base = r[0x200] << 3;
    let fmt_lo = r[0x201];
    let fmt_hi = r[0x202];
    let fixed_mask = (fmt_hi >> 16) & 0xfff;
    let total = ((fmt_hi >> 28) & 0xf) + 1;
    let mut attrs = [[0.0f32, 0.0, 0.0, 1.0]; 12];
    let fmt = |a: u32| -> (u32, u32) {
        let f = if a < 8 { fmt_lo >> (4 * a) } else { fmt_hi >> (4 * (a - 8)) } & 0xf;
        (f & 3, (f >> 2) + 1)
    };
    for l in 0..12usize {
        let off = r[0x203 + 3 * l];
        let c_lo = r[0x204 + 3 * l];
        let c_hi = r[0x205 + 3 * l];
        let n = (c_hi >> 28) & 0xf;
        let stride = (c_hi >> 16) & 0xff;
        let mut o = 0u32;
        for k in 0..n.min(12) {
            let id = if k < 8 { c_lo >> (4 * k) } else { c_hi >> (4 * (k - 8)) } & 0xf;
            if id < 12 {
                let (ty, size) = fmt(id);
                let es = [1, 1, 2, 4][ty as usize];
                o = o.div_ceil(es) * es;
                let a = va(base + off) + index * stride + o;
                for c in 0..size as usize {
                    attrs[id as usize][c] = match ty {
                        0 => r8(a + c as u32) as u8 as i8 as f32,
                        1 => r8(a + c as u32) as f32,
                        2 => r16(a + 2 * c as u32) as u16 as i16 as f32,
                        _ => f32::from_bits(r32(a + 4 * c as u32)),
                    };
                }
                for c in size as usize..4 {
                    attrs[id as usize][c] = if c == 3 { 1.0 } else { 0.0 };
                }
                o += size * es;
            } else {
                o = o.div_ceil(4) * 4 + (id - 11) * 4;
            }
        }
    }
    for a in 0..total.min(12) {
        if fixed_mask >> a & 1 != 0 {
            attrs[a as usize] = st.fixed_attr[a as usize];
        }
    }
    // atributo → registrador de entrada do shader
    let nin = (r[0x2b9] & 0xf) + 1;
    let perm = r[0x2bb] as u64 | (r[0x2bc] as u64) << 32;
    for a in 0..nin.min(12) {
        let reg = ((perm >> (4 * a)) & 0xf) as usize;
        out[reg] = attrs[a as usize];
    }
}

/// Saídas do shader → vértice (máscara 0x2BD empacotada + mapa de semântica 0x50–0x56).
fn output_vertex(st: &State, o: &[V4; 16]) -> Vtx {
    let r = &st.regs;
    let mask = r[0x2bd] & 0xffff;
    let total = (r[0x4f] & 7) as usize;
    let mut slots = [0.0f32; 24];
    slots[3] = 1.0;
    let mut packed = Vec::with_capacity(7);
    for i in 0..16 {
        if mask >> i & 1 != 0 {
            packed.push(o[i]);
        }
    }
    for (i, v) in packed.iter().enumerate().take(total.min(7)) {
        let map = r[0x50 + i];
        for (c, val) in v.iter().enumerate() {
            let sem = ((map >> (8 * c)) & 0x1f) as usize;
            if sem < 24 {
                slots[sem] = *val;
            }
        }
    }
    Vtx {
        pos: [slots[0], slots[1], slots[2], slots[3]],
        quat: [slots[4], slots[5], slots[6], slots[7]],
        color: [slots[8], slots[9], slots[10], slots[11]],
        tc0: [slots[12], slots[13]],
        tc1: [slots[14], slots[15]],
        tc0w: slots[16],
        view: [slots[18], slots[19], slots[20]],
        tc2: [slots[22], slots[23]],
    }
}

fn tex_unit(r: &[u32], i: usize) -> Option<Tex> {
    if r[0x80] >> i & 1 == 0 {
        return None;
    }
    let (border, dim, param, addr, fmt) = match i {
        0 => (0x81, 0x82, 0x83, 0x85, 0x8e),
        1 => (0x91, 0x92, 0x93, 0x95, 0x96),
        _ => (0x99, 0x9a, 0x9b, 0x9d, 0x9e),
    };
    let p = r[param];
    let b = r[border];
    Some(Tex {
        addr: va(r[addr] << 3),
        w: (r[dim] >> 16) & 0x7ff,
        h: r[dim] & 0x7ff,
        format: r[fmt] & 0xf,
        wrap_s: (p >> 12) & 7,
        wrap_t: (p >> 8) & 7,
        border: [b as u8, (b >> 8) as u8, (b >> 16) as u8, (b >> 24) as u8],
        linear: p >> 1 & 1 != 0,
    })
}

fn context(r: &[u32]) -> Ctx {
    let stage_base = [0xc0, 0xc8, 0xd0, 0xd8, 0xf0, 0xf8];
    let stages = stage_base.map(|b| {
        let (src, op, comb, k, sc) = (r[b], r[b + 1], r[b + 2], r[b + 3], r[b + 4]);
        Stage {
            src_rgb: [src & 0xf, (src >> 4) & 0xf, (src >> 8) & 0xf],
            src_a: [(src >> 16) & 0xf, (src >> 20) & 0xf, (src >> 24) & 0xf],
            op_rgb: [op & 0xf, (op >> 4) & 0xf, (op >> 8) & 0xf],
            op_a: [(op >> 12) & 7, (op >> 16) & 7, (op >> 20) & 7],
            comb_rgb: comb & 0xf,
            comb_a: (comb >> 16) & 0xf,
            konst: k.to_le_bytes(),
            scale_rgb: sc & 3,
            scale_a: (sc >> 16) & 3,
        }
    });
    let dims = r[0x11e];
    let at = r[0x104];
    let s0 = r[0x105];
    let s1 = r[0x106];
    let dm = r[0x107];
    let bf = r[0x101];
    Ctx {
        color_addr: va(r[0x11d] << 3),
        depth_addr: va(r[0x11c] << 3),
        width: dims & 0x7ff,
        height_m1: (dims >> 12) & 0x3ff,
        color_fmt: (r[0x117] >> 16) & 7,
        depth_fmt: r[0x116] & 3,
        vp: [
            f24(r[0x41]),
            f24(r[0x43]),
            (r[0x68] & 0x3ff) as f32,
            ((r[0x68] >> 16) & 0x3ff) as f32,
        ],
        zscale: force_zmap().map_or(f24(r[0x4d]), |z| z.0),
        zoffset: force_zmap().map_or(f24(r[0x4e]), |z| z.1),
        // Citra: DepthBuffering::WBuffering = 0 [H: confirmar na captura]
        wbuffer: r[0x6d] & 1 == 0 || force_wbuf(),
        cull: r[0x40] & 3,
        scissor: (
            r[0x65] & 3,
            [r[0x66] & 0x3ff, (r[0x66] >> 16) & 0x3ff, r[0x67] & 0x3ff, (r[0x67] >> 16) & 0x3ff],
        ),
        tex: [tex_unit(r, 0), tex_unit(r, 1), tex_unit(r, 2)],
        tex2_uses_tc1: r[0x80] >> 13 & 1 != 0,
        stages,
        buffer_color: r[0xfd].to_le_bytes(),
        buffer_update: (r[0xe0] >> 8) & 0xff,
        lighting: r[0x8f] & 1 != 0 && r[0x1c6] & 1 == 0,
        alpha_test: (at & 1 != 0, (at >> 4) & 7, (at >> 8) as u8),
        stencil: (
            s0 & 1 != 0,
            (s0 >> 4) & 7,
            (s0 >> 8) as u8,
            (s0 >> 16) as u8,
            (s0 >> 24) as u8,
            [s1 & 7, (s1 >> 4) & 7, (s1 >> 8) & 7],
        ),
        depth_test: dm & 1 != 0,
        depth_func: (dm >> 4) & 7,
        depth_write: dm >> 12 & 1 != 0,
        color_mask: (dm >> 8) & 0xf,
        color_read: r[0x112] & 0xf != 0,
        color_write: r[0x113] & 0xf != 0,
        depth_read: r[0x114] & 3 != 0,
        depth_rw: r[0x115] & 3 != 0,
        blend: r[0x100] >> 8 & 1 != 0,
        blend_eq: (bf & 7, (bf >> 8) & 7),
        blend_fac: [(bf >> 16) & 0xf, (bf >> 20) & 0xf, (bf >> 24) & 0xf, (bf >> 28) & 0xf],
        blend_color: r[0x103].to_le_bytes(),
        logic_op: r[0x102] & 0xf,
        larga: crate::render::LARGA_NADA,
        ancora: 0.0,
    }
}

/// Widescreen (G3): classe de um draw da tela de cima (`render::LARGA_*`).
/// - Projeção perspectiva em c0..c3 (a linha w = c3 tem x/y/z ≠ 0 e w = 0; o jogo põe a projeção da câmera em c0..c3,
///   `FUN_00129d28`, confirmado no passo 0: c3 = (0, 0, −1, 0) na perspectiva e (0, 0, 0, 1) na ortográfica) → cena.
/// - Senão (2D): roda o shader de vértice na CPU em até 16 vértices distintos; se eles cobrem toda a horizontal da tela
///   (os 400, com folga de 1 pixel) e ≥ 2/3 da vertical, e o quadro tem cena 3D ou o draw não tem textura (cor chapada)
///   → tela cheia (estica); senão 2D central (a arte das telas só 2D — título, aviso, vídeo — não deforma). Mais de 16
///   vértices = 2D central.
fn classe_larga(st: &State, ctx: &Ctx, idx: &[u32], hud: bool) -> (u8, Option<[f32; 4]>) {
    use crate::render::{LARGA_2D, LARGA_CENA, LARGA_CHEIA, LARGA_NADA};
    let alto = ctx.height_m1 + 1;
    if alto != 400 || !(ctx.width == 240 || ctx.width == 480) {
        return (LARGA_NADA, None);
    }
    let c3 = st.vs.f[3];
    if c3[3] == 0.0 && (c3[0] != 0.0 || c3[1] != 0.0 || c3[2] != 0.0) {
        crate::render::CENA_QUADRO.store(true, Ordering::Relaxed);
        return (LARGA_CENA, None);
    }
    // até 16 vértices distintos para a tela cheia; com a HUD ancorada (G5) a caixa de todo draw 2D (até 1024)
    let Some([x0, y0, x1, y1]) = caixa_2d(st, ctx, idx, if hud { 1024 } else { 16 }) else {
        larga_log(ctx, idx.len(), None, LARGA_2D);
        return (LARGA_2D, None);
    };
    let bbox = Some([x0, y0, x1, y1]);
    let mut uniq = 0;
    let mut vistos: Vec<u32> = Vec::with_capacity(17);
    for &i in idx {
        if !vistos.contains(&i) {
            vistos.push(i);
            uniq += 1;
            if uniq > 16 {
                break;
            }
        }
    }
    if uniq > 16 {
        larga_log(ctx, idx.len(), bbox, LARGA_2D);
        return (LARGA_2D, bbox);
    }
    let (w, h) = (ctx.width as f32, alto as f32);
    // tela cheia: estica com cena 3D neste quadro ou no anterior (ver `render::CENA_QUADRO`) ou se é cor chapada (sem
    // textura: fade, fundo preto do "carregando" — esticar não deforma nada); arte de tela cheia numa tela só 2D
    // (título, aviso, vídeo) fica no 5:3 central
    let sem_textura = ctx.tex.iter().all(|t| t.is_none());
    let cena = sem_textura || crate::render::CENA_QUADRO.load(Ordering::Relaxed) || crate::render::CENA_ANTERIOR.load(Ordering::Relaxed);
    // "tela cheia" = toda a horizontal da tela (o eixo y do alvo, 400) e pelo menos 2/3 da vertical (o x do alvo):
    // medido na rua, o véu aditivo sobre a cena cobre x 30..480 de 480 (94 %); a faixa do "Objetivo" (y 0..400,
    // x −6..58: 13 %) e o texto ficam no 5:3 central
    let c = if cena && y0 <= 1.0 && y1 >= h - 1.0 && (x1.min(w) - x0.max(0.0)) >= w * 2.0 / 3.0 { LARGA_CHEIA } else { LARGA_2D };
    larga_log(ctx, idx.len(), bbox, c);
    (c, bbox)
}

/// Caixa (x0, y0, x1, y1) no alvo de um draw 2D, rodando o shader de vértice na CPU em até `max` vértices distintos
/// (`None` = mais que isso, ou w inválido).
fn caixa_2d(st: &State, ctx: &Ctx, idx: &[u32], max: usize) -> Option<[f32; 4]> {
    let mut uniq: Vec<u32> = Vec::with_capacity(max.min(64));
    if max <= 16 {
        for &i in idx {
            if !uniq.contains(&i) {
                if uniq.len() == max {
                    return None;
                }
                uniq.push(i);
            }
        }
    } else {
        let mut v = idx.to_vec();
        v.sort_unstable();
        v.dedup();
        if v.len() > max {
            return None;
        }
        uniq = v;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &index in &uniq {
        let mut e = Exec::new(&st.vs);
        load_vertex(st, index, &mut e.input);
        e.run();
        let p = output_vertex(st, &e.out).pos;
        if !(p[3].is_finite() && p[3].abs() > 1e-6) {
            return None;
        }
        // viewport do PICA (como o Vulkan: x = vp[2] + (x/w + 1)·vp[0])
        let x = ctx.vp[2] + (p[0] / p[3] + 1.0) * ctx.vp[0];
        let y = ctx.vp[3] + (p[1] / p[3] + 1.0) * ctx.vp[1];
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    (x0 <= x1).then_some([x0, y0, x1, y1])
}

/// `YW_LARGA_LOG=N`: as N primeiras classificações de draws 2D da tela de cima (diagnóstico do widescreen).
fn larga_log(ctx: &Ctx, nidx: usize, bbox: Option<[f32; 4]>, c: u8) {
    static MAX: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    static N: AtomicU64 = AtomicU64::new(0);
    let max = *MAX.get_or_init(|| std::env::var("YW_LARGA_LOG").ok().and_then(|v| v.parse().ok()).unwrap_or(0));
    if max == 0 || N.fetch_add(1, Ordering::Relaxed) >= max {
        return;
    }
    let t = ctx.tex[0].as_ref().map(|t| format!("{:#x} {}x{} fmt {}", t.addr, t.w, t.h, t.format)).unwrap_or_else(|| "-".into());
    eprintln!(
        "[larga] classe {c} · {nidx} índices · bbox {:?} · dtest {} dwrite {} · blend {} {:?} · tex0 {t} · vp {:?} · alvo {:#x}",
        bbox.map(|b| b.map(|v| (v * 10.0).round() / 10.0)),
        ctx.depth_test,
        ctx.depth_write,
        ctx.blend,
        ctx.blend_fac,
        ctx.vp,
        ctx.color_addr
    );
}

/// Já apareceu um draw com w-buffer (log uma vez).
static WBUF_SEEN: AtomicBool = AtomicBool::new(false);

/// Testes: força o w-buffer em todos os draws (o quadro capturado não o liga; `tests::pica_frame_wbuffer`).
#[cfg(test)]
pub(crate) static FORCE_WBUF: AtomicBool = AtomicBool::new(false);

/// Testes: substitui (zscale, zoffset) — bits f32 de cada um em 32 bits; 0 = não substitui.
#[cfg(test)]
pub(crate) static FORCE_ZMAP: AtomicU64 = AtomicU64::new(0);

fn force_zmap() -> Option<(f32, f32)> {
    #[cfg(test)]
    {
        let v = FORCE_ZMAP.load(Ordering::Relaxed);
        (v != 0).then(|| (f32::from_bits((v >> 32) as u32), f32::from_bits(v as u32)))
    }
    #[cfg(not(test))]
    {
        None
    }
}

fn force_wbuf() -> bool {
    #[cfg(test)]
    {
        FORCE_WBUF.load(Ordering::Relaxed)
    }
    #[cfg(not(test))]
    {
        false
    }
}

fn draw(st: &mut State, indexed: bool) {
    let r = &st.regs;
    let n = r[0x228];
    if n == 0 || n > 0x10_0000 {
        return;
    }
    let mut ctx = context(r);
    if ctx.color_addr == 0 && ctx.depth_addr == 0 {
        return;
    }
    // w-buffer em uso (profundidade = z·zscale + w·zoffset com correção de perspectiva): avisa uma vez — o quadro de
    // teste do quarto não liga (0x6D = 1), então só as rodadas mostram se alguma cena usa
    if ctx.wbuffer && ctx.depth_addr != 0 && (ctx.depth_test || ctx.depth_write) && !WBUF_SEEN.swap(true, Ordering::Relaxed) {
        eprintln!(
            "[pica] w-buffer ligado (0x6D = {:#x}, zscale {}, zoffset {}) — 1º draw com ele",
            r[0x6d], ctx.zscale, ctx.zoffset
        );
    }
    let base = r[0x200] << 3;
    let icfg = r[0x227];
    let iaddr = va(base + (icfg & 0x0fff_ffff));
    let i16 = icfg >> 31 != 0;
    let first = r[0x22a];
    let topo = (r[0x25e] >> 8) & 3;
    if indexed {
        let bytes = if i16 { 2 * n } else { n };
        if !crate::hle::mem::is_mapped(iaddr) || !crate::hle::mem::is_mapped(iaddr.wrapping_add(bytes - 1)) {
            bad_draw("índices fora da memória mapeada");
            return;
        }
    }
    let debug = debug_on();
    if debug {
        for s in &raster::STATS {
            s.store(0, Ordering::Relaxed);
        }
    }
    // índices → vértices distintos (tabela densa pela faixa de índices) → shader de vértice em paralelo quando há
    // muitos (cada vértice é independente; mesmo resultado bit a bit) → montagem de primitivas na ordem original
    let idx: Vec<u32> = (0..n)
        .map(|k| {
            if indexed {
                if i16 {
                    r16(iaddr + 2 * k)
                } else {
                    r8(iaddr + k)
                }
            } else {
                first + k
            }
        })
        .collect();
    let lo = idx.iter().copied().min().unwrap_or(0);
    let hi = idx.iter().copied().max().unwrap_or(0);
    // memória dos vértices mapeada? (com a GPU assíncrona o jogo pode ter reescrito o que a lista aponta; no console
    // isso só leria lixo — aqui pula o draw em vez de derrubar o processo)
    if !vertex_memory_ok(st, lo, hi) {
        bad_draw("vértices fora da memória mapeada");
        return;
    }
    // widescreen da tela de cima (G3): o renderizador põe a cena na largura inteira e o 2D no retângulo 5:3 central
    if crate::render::wide_active() {
        let hud = crate::render::hud_ativa();
        let (c, caixa) = classe_larga(st, &ctx, &idx, hud);
        ctx.larga = c;
        crate::render::LARGA_CONT[ctx.larga as usize & 3].fetch_add(1, Ordering::Relaxed);
        // HUD ancorada (G5): o 2D central vai para a borda do seu grupo
        // — só no alvo que vai para a tela (o jogo também desenha texto em texturas 400×240, ex. o nome no balão, e
        // ancorar letra a letra ali desalinharia a textura) e só sobre a cena 3D (HUD do jogo): numa tela só 2D (menu,
        // título, nome) a arte de fundo fica no 5:3 central e os elementos ficam com ela
        let cena = crate::render::CENA_QUADRO.load(Ordering::Relaxed) || crate::render::CENA_ANTERIOR.load(Ordering::Relaxed);
        if hud && cena && c == crate::render::LARGA_2D && crate::hle::widescreen::hud::alvo_da_tela(ctx.color_addr) {
            ctx.ancora = crate::hle::widescreen::hud::ancora(&ctx, caixa);
        }
    }
    // shader de vértice na GPU: a CPU só traduz os registradores; se o draw não couber nesse caminho, segue abaixo
    if crate::render::gpu_vs() {
        let prep = timed(1, || vs_gpu::prepare(st, &ctx, &idx, lo, hi, topo));
        if let Some(p) = prep {
            let d = crate::render::VsDraw {
                indices: &p.indices,
                spans: &p.spans,
                shading: &p.shading,
                prog_gen: st.prog_gen,
                code: &st.vs.code,
                desc: &st.vs.desc,
                data: None,
            };
            if timed(2, || crate::render::vulkan::draw_vs(&ctx, &d)) {
                if debug {
                    eprintln!("[pica] draw n={n} topo={topo} entry={:#x} (vértices na GPU)", st.vs.entry);
                }
                return;
            }
        }
    }
    let gpu = crate::render::backend() == crate::render::Backend::Vulkan;
    let mut gpu_verts: Vec<crate::render::GpuVtx> = Vec::new();
    let mut tri = |a: &Vtx, b: &Vtx, c: &Vtx| {
        if gpu {
            crate::render::prepare(&ctx, a, b, c, &mut gpu_verts);
        } else {
            raster::triangle(&ctx, a, b, c);
        }
    };
    let mut slot = vec![u32::MAX; (hi - lo) as usize + 1];
    let mut uniq: Vec<u32> = Vec::new();
    for &i in &idx {
        let s = &mut slot[(i - lo) as usize];
        if *s == u32::MAX {
            *s = uniq.len() as u32;
            uniq.push(i);
        }
    }
    let st_ref: &State = st;
    let shade = |&index: &u32| {
        let mut e = Exec::new(&st_ref.vs);
        load_vertex(st_ref, index, &mut e.input);
        e.run();
        output_vertex(st_ref, &e.out)
    };
    let shaded: Vec<Vtx> = timed(1, || {
        if uniq.len() >= 96 {
            use rayon::prelude::*;
            uniq.par_iter().with_min_len(32).map(shade).collect()
        } else {
            uniq.iter().map(shade).collect()
        }
    });
    let cache: Vec<Vtx> = shaded;
    let mut buf: [Vtx; 2] = [Vtx::default(); 2];
    let mut bi = 0usize;
    let mut ready = false;
    for &index in &idx {
        let v = cache[slot[(index - lo) as usize] as usize];
        // montagem de primitivas (Citra `primitive_assembly.cpp`)
        match topo {
            1 | 2 => {
                if ready {
                    tri(&buf[0], &buf[1], &v);
                }
                buf[bi] = v;
                ready |= bi == 1;
                bi = if topo == 1 { 1 - bi } else { 1 };
            }
            _ => {
                if bi < 2 {
                    buf[bi] = v;
                    bi += 1;
                } else {
                    bi = 0;
                    tri(&buf[0], &buf[1], &v);
                }
            }
        }
    }
    if gpu {
        timed(2, || crate::render::vulkan::draw(&ctx, &gpu_verts));
    }
    if debug {
        let s: Vec<u64> = raster::STATS.iter().map(|x| x.load(Ordering::Relaxed)).collect();
        let v0 = cache.first().copied().unwrap_or_default();
        eprintln!(
            "[pica] draw n={n} topo={topo} entry={:#x} tri={} fora={} cull={} frag={} alfa={} sten={} prof={} escritos={} · v0 pos={:?} cor={:?} uv={:?} · z(esc={} off={} wbuf={}) dtest={} f={}",
            st.vs.entry, s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], v0.pos, v0.color, v0.tc0, ctx.zscale, ctx.zoffset, ctx.wbuffer, ctx.depth_test, ctx.depth_func
        );
        if let Some(t) = &ctx.tex[0] {
            let mid = texture::texel(t, t.w / 2, t.h / 2);
            let st0 = &ctx.stages[0];
            eprintln!(
                "[pica]   tex0 {:#x} {}x{} fmt {} meio={mid:?} · tev0 src {:?}/{:?} op {:?} comb {} {} · blend {} {:?}",
                t.addr, t.w, t.h, t.format, st0.src_rgb, st0.src_a, st0.op_rgb, st0.comb_rgb, st0.comb_a, ctx.blend, ctx.blend_fac
            );
        }
    }
}

#[cfg(test)]
mod tests;
