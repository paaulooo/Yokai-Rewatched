//! Operações do GPU vistas pelo GSP: preenchimento de memória, DisplayTransfer (tiled ↔ linear, conversão de formato,
//! redução), TextureCopy, listas de comando PICA (entregues ao renderizador) e o estado dos framebuffers mostrados.
//!
//! Semântica como no `video_core` do Citra/Azahar (`GPU::DisplayTransfer`, `MemoryFill`) e no 3dbrew "GPU/External
//! Registers". Endereços aqui são virtuais (o GSP recebe VAs na fila).

use std::sync::Mutex;
use yw1_rt::{r16, r32, r8, w16, w32, w8};

#[derive(Clone, Copy, Debug, Default)]
pub struct FbInfo {
    pub active: u32,
    pub left: u32,
    pub right: u32,
    pub stride: u32,
    pub format: u32,
}

pub struct State {
    pub fb: [FbInfo; 2],
    pub cmdlists: u64,
    pub fills: u64,
    pub transfers: u64,
    /// listas de comando do quadro atual (endereço, tamanho), para o renderizador
    pub pending: Vec<(u32, u32)>,
}

pub static GPU: Mutex<State> = Mutex::new(State {
    fb: [FbInfo {
        active: 0,
        left: 0,
        right: 0,
        stride: 0,
        format: 0,
    }; 2],
    cmdlists: 0,
    fills: 0,
    transfers: 0,
    pending: Vec::new(),
});

/// `YW_GPU_TRACE=1`: registra cada combinação nova de parâmetros de DisplayTransfer/framebuffer.
fn trace() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("YW_GPU_TRACE").is_ok_and(|v| v != "0"))
}

pub fn set_framebuffer(screen: u32, info: FbInfo) {
    if let Some(f) = GPU.lock().unwrap().fb.get_mut(screen as usize) {
        *f = info;
    }
}

/// Escritas diretas em registradores do LCD/GPU (endereços relativos a 0x1EB00000): framebuffers via registrador.
pub fn hw_regs_written(reg: u32, size: u32, ptr: u32) {
    // 0x400468.. tela de cima, 0x400568.. de baixo: esq1, esq2, formato, …, seleção, …, stride
    for (base, screen) in [(0x0040_0468u32, 0usize), (0x0040_0568, 1)] {
        for i in 0..size / 4 {
            let a = reg + 4 * i;
            let v = r32(ptr + 4 * i);
            let mut g = GPU.lock().unwrap();
            let f = &mut g.fb[screen];
            match a.wrapping_sub(base) {
                0x00 => f.left = v,
                0x08 => f.format = v,
                0x28 => f.stride = v,
                _ => {}
            }
        }
    }
}

pub fn submit_cmdlist(addr: u32, size: u32) {
    submit_cmdlist_words(addr, size, &crate::pica::read_list(addr, size));
}

/// Lista já copiada da memória no despacho (GPU assíncrona).
pub fn submit_cmdlist_words(addr: u32, size: u32, words: &[u32]) {
    let mut g = GPU.lock().unwrap();
    g.cmdlists += 1;
    g.pending.push((addr, size));
    if g.pending.len() > 256 {
        g.pending.remove(0);
    }
    drop(g);
    crate::pica::execute_words(words);
}

/// Preenche `[start, end)` com `value`; `control` bits 8/9 = 24/32 bits (senão 16).
pub fn memory_fill(start: u32, value: u32, end: u32, control: u32) {
    // o backend de GPU devolve/descarta antes o que só ele tem nessa faixa e, depois, transforma o fill em clear
    crate::render::before_write(start, end);
    crate::pica::timed(5, || memory_fill_inner(start, value, end, control));
    let fill_bpp = if control & 0x200 != 0 { 4 } else if control & 0x100 != 0 { 3 } else { 2 };
    crate::render::after_fill(start, end, fill_bpp);
}

fn memory_fill_inner(start: u32, value: u32, end: u32, control: u32) {
    if start == 0 || end <= start || !super::mem::is_mapped(start) {
        return;
    }
    GPU.lock().unwrap().fills += 1;
    let mut a = start;
    if control & 0x200 != 0 {
        while a + 4 <= end {
            w32(a, value);
            a += 4;
        }
    } else if control & 0x100 != 0 {
        while a + 3 <= end {
            w8(a, value);
            w8(a + 1, value >> 8);
            w8(a + 2, value >> 16);
            a += 3;
        }
    } else {
        while a + 2 <= end {
            w16(a, value);
            a += 2;
        }
    }
}

pub fn bpp(format: u32) -> u32 {
    match format & 7 {
        0 => 4,
        1 => 3,
        _ => 2,
    }
}

/// Índice de Morton (Z-order) de (x, y) dentro de um bloco 8×8.
pub fn morton8(x: u32, y: u32) -> u32 {
    let mut m = 0;
    for i in 0..3 {
        m |= ((x >> i) & 1) << (2 * i) | ((y >> i) & 1) << (2 * i + 1);
    }
    m
}

/// Deslocamento em bytes de um pixel numa imagem em blocos 8×8 (formato de render target do PICA).
pub fn tiled_offset(x: u32, y: u32, width: u32, bpp: u32) -> u32 {
    let tile = (y / 8) * (width / 8) + x / 8;
    (tile * 64 + morton8(x & 7, y & 7)) * bpp
}

/// Lê um pixel como RGBA8 (r, g, b, a).
pub fn read_pixel(a: u32, format: u32) -> [u8; 4] {
    match format & 7 {
        0 => {
            let v = r32(a);
            [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]
        }
        1 => [r8(a + 2) as u8, r8(a + 1) as u8, r8(a) as u8, 255],
        2 => {
            let v = r16(a);
            let r = (v >> 11) & 31;
            let g = (v >> 5) & 63;
            let b = v & 31;
            [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8, 255]
        }
        3 => {
            let v = r16(a);
            let c = |s: u32| {
                let x = (v >> s) & 31;
                (x << 3 | x >> 2) as u8
            };
            [c(11), c(6), c(1), if v & 1 != 0 { 255 } else { 0 }]
        }
        _ => {
            let v = r16(a);
            let c = |s: u32| (((v >> s) & 15) * 17) as u8;
            [c(12), c(8), c(4), c(0)]
        }
    }
}

pub fn write_pixel(a: u32, format: u32, p: [u8; 4]) {
    let [r, g, b, al] = p.map(|x| x as u32);
    match format & 7 {
        0 => w32(a, r << 24 | g << 16 | b << 8 | al),
        1 => {
            w8(a, b);
            w8(a + 1, g);
            w8(a + 2, r);
        }
        2 => w16(a, (r >> 3) << 11 | (g >> 2) << 5 | (b >> 3)),
        3 => w16(a, (r >> 3) << 11 | (g >> 3) << 6 | (b >> 3) << 1 | (al >> 7)),
        _ => w16(a, (r >> 4) << 12 | (g >> 4) << 8 | (b >> 4) << 4 | (al >> 4)),
    }
}

/// Formato de cor de render target do PICA → numeração do GSP/LCD que `read_pixel`/`write_pixel` usam.
/// As duas tabelas diferem nos formatos de 16 bits: PICA (registrador 0x117, bits 16–18) é RGBA8 0, RGB8 1,
/// RGB5A1 2, RGB565 3, RGBA4 4; GSP (DisplayTransfer e framebuffers do LCD) é RGBA8 0, RGB8 1, RGB565 2, RGB5A1 3,
/// RGBA4 4 (Azahar: `regs_framebuffer.h` ColorFormat × `regs_external.h` PixelFormat). Quem lê/escreve um render
/// target com o formato do PICA tem de passar por aqui.
pub fn pica_color_fmt_to_gsp(fmt: u32) -> u32 {
    match fmt & 7 {
        2 => 3,
        3 => 2,
        f => f,
    }
}

/// DisplayTransfer(entrada, saída, dimensões de entrada, de saída, flags).
/// flags: bit0 inverter vertical, bit1 entrada linear (→ saída em blocos), bit3 cópia crua, bit5 sem swizzle,
/// bits 8–10 formato de entrada, 12–14 formato de saída, 24–25 redução (0 nenhuma, 1 2×1, 2 2×2).
pub fn display_transfer(inp: u32, out: u32, in_dims: u32, out_dims: u32, flags: u32) {
    let (iw, ih, ow, oh) = (in_dims & 0xffff, in_dims >> 16, out_dims & 0xffff, out_dims >> 16);
    crate::render::before_read(inp, inp.wrapping_add(iw * ih * bpp((flags >> 8) & 7)));
    crate::render::before_write(out, out.wrapping_add(ow * oh * bpp((flags >> 12) & 7)));
    crate::pica::timed(4, || display_transfer_inner(inp, out, in_dims, out_dims, flags))
}

fn display_transfer_inner(inp: u32, out: u32, in_dims: u32, out_dims: u32, flags: u32) {
    if !super::mem::is_mapped(inp) || !super::mem::is_mapped(out) {
        eprintln!("[gpu] DisplayTransfer com endereço não mapeado {inp:#x} → {out:#x}");
        return;
    }
    GPU.lock().unwrap().transfers += 1;
    if trace() {
        static SEEN: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(Vec::new());
        let k = (in_dims, out_dims, flags);
        let mut s = SEEN.lock().unwrap();
        if !s.contains(&k) {
            s.push(k);
            eprintln!(
                "[gpu] DisplayTransfer novo: {inp:#x} {}x{} → {out:#x} {}x{} flags {flags:#x}",
                in_dims & 0xffff,
                in_dims >> 16,
                out_dims & 0xffff,
                out_dims >> 16
            );
        }
    }
    let (iw, ih) = (in_dims & 0xffff, in_dims >> 16);
    let (ow, oh) = (out_dims & 0xffff, out_dims >> 16);
    let flip = flags & 1 != 0;
    let in_linear = flags & 2 != 0;
    let no_swizzle = flags & 0x20 != 0;
    let fin = (flags >> 8) & 7;
    let fout = (flags >> 12) & 7;
    let scale = (flags >> 24) & 3;
    let (sx, sy) = match scale {
        1 => (2, 1),
        2 => (2, 2),
        _ => (1, 1),
    };
    let (bi, bo) = (bpp(fin), bpp(fout));
    // Citra `GPU::DisplayTransfer`: com redução, a largura/altura de saída do registrador é dividida pela escala e
    // ESSA é a largura de linha da saída (antes usávamos a do registrador: listras nas cenas 3D com 2×1, 480→240)
    let (ow, oh) = (ow / sx, oh / sy);
    let w = ow.min(iw / sx);
    let h = oh.min(ih / sy);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            for dy in 0..sy {
                for dx in 0..sx {
                    let (ix, iy) = (x * sx + dx, y * sy + dy);
                    let src = if in_linear || no_swizzle {
                        (iy * iw + ix) * bi
                    } else {
                        tiled_offset(ix, iy, iw, bi)
                    };
                    let p = read_pixel(inp + src, fin);
                    for k in 0..4 {
                        acc[k] += p[k] as u32;
                    }
                }
            }
            let n = sx * sy;
            let p = acc.map(|v| (v / n) as u8);
            let oy = if flip { h - 1 - y } else { y };
            let dst = if in_linear && !no_swizzle {
                tiled_offset(x, oy, ow, bo)
            } else {
                (oy * ow + x) * bo
            };
            write_pixel(out + dst, fout, p);
        }
    }
}

/// TextureCopy(entrada, saída, tamanho total, largura/espaço de entrada, de saída, flags) — unidades de 16 bytes.
pub fn texture_copy(inp: u32, out: u32, total: u32, in_wg: u32, out_wg: u32, _flags: u32) {
    if !super::mem::is_mapped(inp) || !super::mem::is_mapped(out) {
        return;
    }
    let (iw, ig) = ((in_wg & 0xffff) * 16, (in_wg >> 16) * 16);
    let (ow, og) = ((out_wg & 0xffff) * 16, (out_wg >> 16) * 16);
    let (iw, ow) = (if iw == 0 { total } else { iw }, if ow == 0 { total } else { ow });
    if iw == 0 || ow == 0 {
        return;
    }
    // faixas tocadas (linhas de `iw`/`ow` bytes separadas por `ig`/`og`)
    let span = |w: u32, g: u32| total.div_ceil(w) * (w + g);
    crate::render::before_read(inp, inp.wrapping_add(span(iw, ig)));
    crate::render::before_write(out, out.wrapping_add(span(ow, og)));
    let mut done = 0;
    let (mut ia, mut oa) = (inp, out);
    let (mut il, mut ol) = (0, 0);
    while done < total {
        let n = (iw - il).min(ow - ol).min(total - done);
        let mut buf = vec![0u8; n as usize];
        yw1_rt::read_bytes(ia, &mut buf);
        yw1_rt::write_bytes(oa, &buf);
        done += n;
        ia += n;
        oa += n;
        il += n;
        ol += n;
        if il == iw {
            ia += ig;
            il = 0;
        }
        if ol == ow {
            oa += og;
            ol = 0;
        }
    }
}

/// Imagem RGBA (linhas de cima para baixo) da tela `screen` (0 = cima 400×240, 1 = baixo 320×240), lida do
/// framebuffer linear que o jogo mostra (guardado "deitado": coluna a coluna, de baixo para cima).
pub fn screen_rgba(screen: usize) -> Option<(u32, u32, Vec<u8>)> {
    let f = GPU.lock().unwrap().fb[screen];
    // o jogo pode dar o endereço físico (registradores do LCD) ou virtual (SetBufferSwap)
    let addr = super::mem::pa_to_va(f.left).unwrap_or(f.left);
    if addr == 0 || !super::mem::is_mapped(addr) {
        return None;
    }
    let (w, h) = if screen == 0 { (400u32, 240u32) } else { (320, 240) };
    let fmt = f.format & 7;
    let b = bpp(fmt);
    let stride = if f.stride != 0 { f.stride } else { 240 * b };
    let mut out = vec![0u8; (w * h * 4) as usize];
    for x in 0..w {
        for y in 0..h {
            let a = addr + x * stride + (h - 1 - y) * b;
            let p = read_pixel(a, fmt);
            let o = ((y * w + x) * 4) as usize;
            out[o..o + 3].copy_from_slice(&p[..3]);
            out[o + 3] = 255;
        }
    }
    Some((w, h, out))
}
