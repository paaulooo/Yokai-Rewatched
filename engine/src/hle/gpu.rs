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
    if screen == 0 {
        note_top(&info);
    }
    note_shown(&info);
}

/// Framebuffers (endereço virtual) que as DUAS telas mostraram (os 8 últimos distintos). Um DisplayTransfer para outro
/// endereço é uma cópia que o jogo usa para outra coisa (neste jogo, a cena vai a cada quadro também para 0x1F000000) e
/// nenhuma tela mostra: não precisa de imagem em alta resolução (era FXAA + montagem + leitura de 3,5 MB por quadro a 3×).
static SHOWN: Mutex<[u32; 8]> = Mutex::new([0; 8]);

fn note_shown(f: &FbInfo) {
    let a = super::mem::pa_to_va(f.left).unwrap_or(f.left);
    let mut v = SHOWN.lock().unwrap();
    if a != 0 && !v.contains(&a) {
        v.rotate_right(1);
        v[0] = a;
    }
}

/// `addr` não é framebuffer de nenhuma tela? (só depois que as telas mostraram algo; antes disso tudo vale)
pub fn not_shown(addr: u32) -> bool {
    let v = SHOWN.lock().unwrap();
    v[0] != 0 && !v.contains(&addr)
}

/// Framebuffers (endereço virtual) que o LCD de cima mostrou como olho esquerdo e como olho direito — os 4 últimos
/// distintos de cada (o jogo alterna dois por olho). A janela só mostra o esquerdo: é o único que precisa de imagem em
/// alta resolução e de interpolação. Medido: neste jogo o esquerdo é 0x1F300000/0x1F346500 e o direito
/// 0x1F000000/0x1F046500 (o endereço sozinho não diz qual é qual — por isso vem do que o LCD mostra).
static TOP_EYES: Mutex<([u32; 4], [u32; 4])> = Mutex::new(([0; 4], [0; 4]));

fn note_top(f: &FbInfo) {
    let va = |a: u32| super::mem::pa_to_va(a).unwrap_or(a);
    let (l, r) = (va(f.left), va(f.right));
    let push = |v: &mut [u32; 4], a: u32| {
        if a != 0 && !v.contains(&a) {
            v.rotate_right(1);
            v[0] = a;
        }
    };
    let mut e = TOP_EYES.lock().unwrap();
    push(&mut e.0, l);
    // modo mono (o jogo não ligou o 3D): não há olho escondido. O jogo pode mandar o mesmo buffer nos dois olhos (ou
    // um "direito" que nunca recebe cena); registrar como direito arriscaria `top_hidden` verdadeiro para um buffer
    // que a janela mostra (sem sombra N× nem interpolação nele)
    if r != l && game_stereo() {
        push(&mut e.1, r);
    }
}

/// O jogo ligou o 3D estéreo? `DAT_00608b47` (code.bin.c, `FUN_00114588`): vira 1 logo depois de
/// `nngxSetDisplayMode(0x702)` no boot e é o que o próprio jogo consulta (`FUN_00119a3c`, controle do 3D). Com
/// `graficos.estereo_3d` desligado a sonda do recomp impede a chamada e ele fica 0.
/// Troca da tela de cima sintetizada no modo mono (ver `display_transfer_native`).
static SYNTH_TOP: Mutex<Option<FbInfo>> = Mutex::new(None);

/// A próxima troca sintética da tela de cima, se houver (VBlank da tela de cima, `gsp::update_framebuffer`).
pub fn take_synth_top() -> Option<FbInfo> {
    SYNTH_TOP.lock().unwrap().take()
}

/// Troca sintética adiada (o buffer ainda tem DisplayTransfer na fila): volta, a não ser que já haja uma mais nova.
pub fn put_back_synth_top(i: FbInfo) {
    SYNTH_TOP.lock().unwrap().get_or_insert(i);
}

pub fn game_stereo() -> bool {
    const STEREO_FLAG: u32 = 0x0060_8b47;
    super::mem::is_mapped(STEREO_FLAG) && r8(STEREO_FLAG) != 0
}

/// `addr` é um framebuffer que o LCD de cima mostra como olho esquerdo (o da janela)?
pub fn top_shown(addr: u32) -> bool {
    addr != 0 && TOP_EYES.lock().unwrap().0.contains(&addr)
}

/// `addr` é só o olho direito do 3D estéreo (o LCD o usa como direito e nunca como esquerdo)? A janela não o mostra.
pub fn top_hidden(addr: u32) -> bool {
    let e = TOP_EYES.lock().unwrap();
    addr != 0 && e.1.contains(&addr) && !e.0.contains(&addr)
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
            let info = *f;
            drop(g);
            if screen == 0 {
                note_top(&info);
            }
            note_shown(&info);
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
    if let Some(c) = display_transfer_native(inp, out, in_dims, out_dims, flags) {
        crate::render::present_hires_finish(c);
    }
}

/// A parte "nativa" do DisplayTransfer (o que o jogo pode ver: leitura do alvo, a transferência na memória dele, e o
/// passo da imagem N× que precisa do alvo/texturas ainda intactos: `present_hires_check`). Devolve o resto da imagem
/// N× (`render::present_hires_finish`: montagem, intermediários da interpolação, publicação), que a thread "gpu" roda
/// DEPOIS de sinalizar o PPF ao jogo (PPF adiantado, `gsp.rs`).
pub fn display_transfer_native(
    inp: u32,
    out: u32,
    in_dims: u32,
    out_dims: u32,
    flags: u32,
) -> Option<crate::render::vulkan::PresentChecked> {
    let (iw, ih, ow, oh) = (in_dims & 0xffff, in_dims >> 16, out_dims & 0xffff, out_dims >> 16);
    // modo mono (3D desligado): o jogo transfere cada quadro da tela de cima para um de dois buffers, mas nunca publica a
    // troca (o bloco de atualização da tela de cima na memória compartilhada fica zerado — o caminho mono do SDK não roda
    // em console de varejo). A troca que o jogo faria é sintetizada: este buffer vai para a tela no próximo VBlank
    // (`take_synth_top`, com o mesmo adiamento e contagem das trocas normais).
    // (a saída é medida depois da redução: no quarto o alvo é 480×400 e a transferência reduz 2:1)
    let (rx, ry) = match (flags >> 24) & 3 {
        1 => (2, 1),
        2 => (2, 2),
        _ => (1, 1),
    };
    if !game_stereo() && ow / rx == 240 && oh / ry == 400 {
        let fmt = (flags >> 12) & 7;
        *SYNTH_TOP.lock().unwrap() = Some(FbInfo { active: 0, left: out, right: out, stride: 240 * bpp(fmt), format: fmt });
    }
    // saída que a janela não mostra (olho direito do 3D): a cena que acabou de ser desenhada no alvo não precisa da
    // sombra N× (o nativo, que vai para a memória do jogo, continua igual)
    let hidden = top_hidden(out) || not_shown(out);
    crate::render::before_read_lcd(inp, inp.wrapping_add(iw * ih * bpp((flags >> 8) & 7)), hidden);
    // área que a transferência REALMENTE escreve (ver `display_transfer_inner`: com redução a saída é ow/sx × oh/sy, e
    // só w×h pixels são gravados). Anunciar mais que isso como "escrita" faz o renderizador descartar, sem devolver,
    // superfícies pendentes vizinhas que a transferência nunca tocou.
    let (sx, sy) = match (flags >> 24) & 3 {
        1 => (2, 1),
        2 => (2, 2),
        _ => (1, 1),
    };
    let (ow2, oh2) = (ow / sx, oh / sy);
    let full = iw / sx >= ow2 && ih / sy >= oh2;
    let tiled_out = flags & 2 != 0 && flags & 0x20 == 0;
    let rows = if tiled_out { oh2.next_multiple_of(8) } else { oh2 };
    let hi = out.wrapping_add(ow2 * rows * bpp((flags >> 12) & 7));
    if full {
        crate::render::before_write(out, hi);
    } else {
        crate::render::before_read(out, hi);
    }
    crate::pica::timed(4, || display_transfer_inner(inp, out, in_dims, out_dims, flags));
    // resolução interna: a mesma transferência, em N×, a partir da sombra do alvo (só o caso do LCD: entrada em blocos,
    // saída linear, a saída inteira escrita)
    if crate::render::hires_on() && !full_skip(flags) && full {
        let bo = bpp((flags >> 12) & 7);
        return crate::render::present_hires_check(crate::render::vulkan::PresentReq {
            inp,
            iw,
            ih,
            sx,
            sy,
            flip: flags & 1 != 0,
            ow: ow2,
            hh: oh2,
            out,
            out_bytes: ow2 * oh2 * bo,
        });
    }
    None
}

/// Transferências que a imagem em alta resolução não reproduz (entrada linear ou sem swizzle).
fn full_skip(flags: u32) -> bool {
    flags & 2 != 0 || flags & 0x20 != 0
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
    transfer_buffered(inp, out, in_dims, out_dims, flags);
}

/// Geometria de um DisplayTransfer: (iw, ih, ow, w, h, sx, sy, formato de entrada, de saída, flip, entrada linear,
/// sem swizzle). `ow` já dividido pela redução (Citra `GPU::DisplayTransfer`: com redução, a largura/altura de saída do
/// registrador é dividida pela escala e ESSA é a largura de linha da saída — antes usávamos a do registrador: listras
/// nas cenas 3D com 2×1, 480→240).
struct Xfer {
    iw: u32,
    ow: u32,
    w: u32,
    h: u32,
    sx: u32,
    sy: u32,
    fin: u32,
    fout: u32,
    flip: bool,
    in_linear: bool,
    no_swizzle: bool,
}

impl Xfer {
    fn new(in_dims: u32, out_dims: u32, flags: u32) -> Xfer {
        let (iw, ih) = (in_dims & 0xffff, in_dims >> 16);
        let (ow, oh) = (out_dims & 0xffff, out_dims >> 16);
        let (sx, sy) = match (flags >> 24) & 3 {
            1 => (2, 1),
            2 => (2, 2),
            _ => (1, 1),
        };
        let (ow, oh) = (ow / sx, oh / sy);
        Xfer {
            iw,
            ow,
            w: ow.min(iw / sx),
            h: oh.min(ih / sy),
            sx,
            sy,
            fin: (flags >> 8) & 7,
            fout: (flags >> 12) & 7,
            flip: flags & 1 != 0,
            in_linear: flags & 2 != 0,
            no_swizzle: flags & 0x20 != 0,
        }
    }
    /// deslocamento (bytes) do pixel (ix, iy) na entrada
    fn src(&self, ix: u32, iy: u32) -> u32 {
        let bi = bpp(self.fin);
        if self.in_linear || self.no_swizzle {
            (iy * self.iw + ix) * bi
        } else {
            tiled_offset(ix, iy, self.iw, bi)
        }
    }
    /// deslocamento (bytes) do pixel (x, oy) na saída
    fn dst(&self, x: u32, oy: u32) -> u32 {
        let bo = bpp(self.fout);
        if self.in_linear && !self.no_swizzle {
            tiled_offset(x, oy, self.ow, bo)
        } else {
            (oy * self.ow + x) * bo
        }
    }
    /// pixel (x, y) da saída: média dos sx × sy pixels da entrada
    fn pixel(&self, x: u32, y: u32, get: impl Fn(u32) -> [u8; 4]) -> [u8; 4] {
        let mut acc = [0u32; 4];
        for dy in 0..self.sy {
            for dx in 0..self.sx {
                let p = get(self.src(x * self.sx + dx, y * self.sy + dy));
                for k in 0..4 {
                    acc[k] += p[k] as u32;
                }
            }
        }
        let n = self.sx * self.sy;
        acc.map(|v| (v / n) as u8)
    }
    /// tamanho (bytes) da faixa que a entrada / a saída ocupam: o maior deslocamento usado + 1 pixel (em blocos 8×8, o
    /// maior está no último bloco: deslocamento do bloco + 63 pixels)
    fn spans(&self) -> (u32, u32) {
        let (bi, bo) = (bpp(self.fin), bpp(self.fout));
        let (iw_used, ih_used) = (self.w * self.sx, self.h * self.sy);
        let in_len = if self.in_linear || self.no_swizzle {
            ((ih_used - 1) * self.iw + iw_used) * bi
        } else {
            (((ih_used - 1) / 8) * (self.iw / 8) + (iw_used - 1) / 8 + 1) * 64 * bi
        };
        let out_len = if self.in_linear && !self.no_swizzle {
            (((self.h - 1) / 8) * (self.ow / 8) + (self.w - 1) / 8 + 1) * 64 * bo
        } else {
            ((self.h - 1) * self.ow + self.w) * bo
        };
        (in_len, out_len)
    }
}

/// Pixel RGBA8 de um buffer local no formato do GSP (mesma conversão de `read_pixel`, memória little-endian).
fn px_get(b: &[u8], o: usize, format: u32) -> [u8; 4] {
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
    match format & 7 {
        0 => {
            let v = u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
            [(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]
        }
        1 => [b[o + 2], b[o + 1], b[o], 255],
        2 => {
            let v = u16_at(o);
            let (r, g, bl) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
            [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (bl << 3 | bl >> 2) as u8, 255]
        }
        3 => {
            let v = u16_at(o);
            let c = |s: u32| {
                let x = (v >> s) & 31;
                (x << 3 | x >> 2) as u8
            };
            [c(11), c(6), c(1), if v & 1 != 0 { 255 } else { 0 }]
        }
        _ => {
            let v = u16_at(o);
            let c = |s: u32| (((v >> s) & 15) * 17) as u8;
            [c(12), c(8), c(4), c(0)]
        }
    }
}

/// Grava um pixel RGBA8 num buffer local no formato do GSP (mesma conversão de `write_pixel`).
fn px_put(b: &mut [u8], o: usize, format: u32, p: [u8; 4]) {
    let [r, g, bl, al] = p.map(|x| x as u32);
    let put16 = |b: &mut [u8], v: u32| b[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
    match format & 7 {
        0 => b[o..o + 4].copy_from_slice(&(r << 24 | g << 16 | bl << 8 | al).to_le_bytes()),
        1 => {
            b[o] = bl as u8;
            b[o + 1] = g as u8;
            b[o + 2] = r as u8;
        }
        2 => put16(b, (r >> 3) << 11 | (g >> 2) << 5 | (bl >> 3)),
        3 => put16(b, (r >> 3) << 11 | (g >> 3) << 6 | (bl >> 3) << 1 | (al >> 7)),
        _ => put16(b, (r >> 4) << 12 | (g >> 4) << 8 | (bl >> 4) << 4 | (al >> 4)),
    }
}

/// A transferência com uma cópia em bloco de cada lado (entrada inteira para um buffer local, a faixa de saída lida,
/// convertida e escrita de volta) em vez de um acesso à memória guest por byte de cada pixel: o resultado é o mesmo
/// byte a byte (teste `display_transfer_igual_ao_pixel_a_pixel`), e a thread da GPU deixa de gastar ~3 ms por quadro
/// aqui (medido como "transfer 10 %" no perf.log). Faixa fora da memória mapeada: pixel a pixel, como antes.
fn transfer_buffered(inp: u32, out: u32, in_dims: u32, out_dims: u32, flags: u32) {
    let x = Xfer::new(in_dims, out_dims, flags);
    if x.w == 0 || x.h == 0 {
        return;
    }
    let (in_len, out_len) = x.spans();
    let mapped = |a: u32, n: u32| super::mem::is_mapped(a) && super::mem::is_mapped(a.wrapping_add(n - 1));
    if !mapped(inp, in_len) || !mapped(out, out_len) {
        transfer_per_pixel(inp, out, in_dims, out_dims, flags);
        return;
    }
    let mut ib = vec![0u8; in_len as usize];
    yw1_rt::read_bytes(inp, &mut ib);
    let mut ob = vec![0u8; out_len as usize];
    yw1_rt::read_bytes(out, &mut ob);
    for y in 0..x.h {
        let oy = if x.flip { x.h - 1 - y } else { y };
        for xx in 0..x.w {
            let p = x.pixel(xx, y, |o| px_get(&ib, o as usize, x.fin));
            px_put(&mut ob, x.dst(xx, oy) as usize, x.fout, p);
        }
    }
    yw1_rt::write_bytes(out, &ob);
}

/// A transferência pixel a pixel direto na memória guest (o caminho antigo; referência do teste e reserva quando a
/// faixa não está toda mapeada).
pub(crate) fn transfer_per_pixel(inp: u32, out: u32, in_dims: u32, out_dims: u32, flags: u32) {
    let x = Xfer::new(in_dims, out_dims, flags);
    for y in 0..x.h {
        let oy = if x.flip { x.h - 1 - y } else { y };
        for xx in 0..x.w {
            let p = x.pixel(xx, y, |o| read_pixel(inp + o, x.fin));
            write_pixel(out + x.dst(xx, oy), x.fout, p);
        }
    }
}

/// A transferência rápida (para o teste de equivalência).
#[cfg(test)]
pub(crate) fn transfer_fast_for_test(inp: u32, out: u32, in_dims: u32, out_dims: u32, flags: u32) {
    transfer_buffered(inp, out, in_dims, out_dims, flags);
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
    // (a última linha vai só até onde a cópia escreve, sem o intervalo nem o resto da linha)
    let span = |w: u32, g: u32| (total / w) * (w + g) + total % w;
    crate::render::before_read(inp, inp.wrapping_add(span(iw, ig)));
    if og == 0 {
        crate::render::before_write(out, out.wrapping_add(span(ow, og)));
    } else {
        // com intervalos, a faixa de saída tem bytes que a cópia NÃO escreve: uma superfície pendente dentro dela não
        // pode ser descartada como "sobrescrita por inteiro" — volta à memória (devolução normal) antes da cópia.
        crate::render::before_read(out, out.wrapping_add(span(ow, og)));
    }
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

/// Imagem em alta resolução de uma tela: o quadro real e, se ele foi interpolado, o grupo (intermediários + real) que a
/// janela mostra em ordem, na cadência dela (`window.rs`, `Cadencia`).
pub struct ScreenHi {
    pub real: std::sync::Arc<crate::render::vulkan::HiRes>,
    pub group: Option<std::sync::Arc<crate::render::vulkan::Group>>,
    /// framebuffer (endereço virtual) de onde veio
    pub addr: u32,
}

/// Imagem em alta resolução da tela `screen` (resolução interna / FXAA / interpolação), se o backend a montou para o
/// framebuffer que o jogo mostra agora e esse framebuffer nativo ainda é o que a transferência escreveu (senão o jogo pôs
/// outra coisa ali).
pub fn screen_hires(screen: usize) -> Option<ScreenHi> {
    if !crate::render::hires_on() {
        return None;
    }
    let f = GPU.lock().unwrap().fb[screen];
    let addr = super::mem::pa_to_va(f.left).unwrap_or(f.left);
    let group = crate::render::vulkan::published_group(addr);
    let real = match &group {
        Some(g) => g.real.clone(),
        None => crate::render::vulkan::published(addr)?,
    };
    if addr == 0 || !super::mem::is_mapped(addr) || !super::mem::is_mapped(addr + real.bytes.saturating_sub(1)) {
        return None;
    }
    // a conferência do framebuffer nativo (hash de ~288 KB) só é refeita quando algo mudou — outro endereço, outra
    // troca de framebuffer, outra imagem publicada — ou a cada 100 ms (o jogo pode escrever no buffer mostrado sem
    // trocar). A janela chama isto a cada redesenho (até centenas por segundo)
    use std::sync::atomic::Ordering::Relaxed;
    let swaps = super::services::gsp::SWAPS[screen & 1].load(Relaxed);
    let key = (addr, swaps, std::sync::Arc::as_ptr(&real) as usize, real.hash);
    let now = std::time::Instant::now();
    let mut cache = SCREEN_HI_CHECK.lock().unwrap();
    let ok = match cache[screen & 1] {
        Some((k, t, ok)) if k == key && now.duration_since(t) < std::time::Duration::from_millis(100) => ok,
        _ => {
            // SAFETY: a faixa inteira está mapeada (conferido acima) e a reserva vive até o fim do processo; o jogo
            // ou a thread "gpu" podem estar escrevendo nela ao mesmo tempo, como em qualquer leitura da memória guest
            let mem = unsafe { std::slice::from_raw_parts(yw1_rt::host_ptr(addr), real.bytes as usize) };
            let ok = crate::render::hash_bytes(mem) == real.hash;
            cache[screen & 1] = Some((key, now, ok));
            ok
        }
    };
    drop(cache);
    ok.then_some(ScreenHi { real, group, addr })
}

/// Última conferência de `screen_hires` por tela: ((endereço, trocas, imagem publicada, hash), quando, resultado).
#[allow(clippy::type_complexity)]
static SCREEN_HI_CHECK: Mutex<[Option<((u32, u64, usize, u64), std::time::Instant, bool)>; 2]> = Mutex::new([None; 2]);

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
