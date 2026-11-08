//! Backends de GPU do renderizador PICA. O front-end (`pica`) continua igual: registradores, carregamento de
//! vértices e shader de vértice na CPU (`pica::shader`, exato). Um backend de GPU recebe por draw o estado
//! (`raster::Ctx`) e os triângulos já sombreados; esta camada comum faz o que tem de ser bit a bit igual ao `soft`
//! — recorte nos 7 planos (`raster::clip`), projeção com o viewport do PICA arredondada a 1/16 de pixel (12.4) e o
//! descarte por orientação — e empacota os uniforms do "ubershader" de fragmento (TEV, alpha test, texturas).
//!
//! Seleção: `YW_RENDERER=vulkan|soft|none` (padrão `vulkan`, desde 08/11, depois da revisão). Se o Vulkan não inicializar, cai para `soft` com aviso.
//! O DX12 (alternativa futura) vai consumir os mesmos `GpuVtx` / `DrawUniforms`.
//!
//! Memória guest continua sendo a fonte da verdade: o backend carrega os render targets da memória quando mudaram e
//! devolve o resultado para ela no fim de cada lista de comandos com draws (`end_list`).

pub mod convert;
pub mod vulkan;

use crate::pica::raster::{self, Ctx, Vtx};
use std::sync::atomic::{AtomicU8, Ordering};

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
            let z = if c.wbuffer { v.pos[2] } else { v.pos[2] * inv_w };
            let depth = c.zoffset + z * c.zscale;
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
    u.misc = [u32::from_le_bytes(c.buffer_color), c.buffer_update, at as u32 | af << 4 | (aref as u32) << 8, flags];
    let sc = c.scissor.1;
    u.scissor = [sc[0], sc[1], sc[2], sc[3]];
    u.depth = [if c.depth_fmt == 0 { 0xffff } else { 0xff_ffff }, 0, 0, 0];
    u
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
