//! Preparação de um draw para o shader de vértice na GPU (`render/shaders/pica_vs.vert`, backend Vulkan).
//!
//! Em vez de rodar `shader.rs` por vértice, a CPU só traduz os registradores do draw para o que o shader da GPU lê:
//! - os 12 carregadores de atributos viram, por registrador de entrada, (byte do vértice 0, passo, tipo, nº de
//!   componentes) dentro dos trechos de memória guest copiados (faixa de índices `lo..=hi` de cada carregador) —
//!   mesma conta de alinhamento/preenchimento de `load_vertex`; atributos fixos e o padrão (0, 0, 0, 1) viram o valor
//!   "sem componente" do registrador; a permutação 0x2BB/0x2BC escolhe o registrador;
//! - a máscara de saídas 0x2BD + o mapa de semânticas 0x50–0x56 viram "semântica → registrador·4 + componente"
//!   (mesma regra de `output_vertex`: a última escrita vence);
//! - strip/fan viram lista de triângulos com a mesma ordem de vértices da montagem da CPU (então o descarte por
//!   orientação no GPU vê os mesmos triângulos), com índices relativos a `lo`.
//!
//! Ideia (storage buffer com os arranjos crus + semânticas) do Zakuro, `crates/zakuro-gpu/shaders/shade.vert`.

use super::raster::Ctx;
use super::shader::f24;
use super::{va, State};
use crate::render::{VsShading, SEM_NONE, VS_F_USER_CLIP, VS_F_WBUFFER};

/// O que `pica::draw` entrega ao backend (dono dos dados; `render::VsDraw` só empresta).
pub(super) struct Prepared {
    pub indices: Vec<u32>,
    pub spans: Vec<(u32, u32)>,
    pub shading: Box<VsShading>,
}

#[derive(Clone, Copy)]
enum Attr {
    /// do carregador `l`: deslocamento no vértice, tipo (0 byte, 1 ubyte, 2 short, 3 float), componentes
    Loaded { l: usize, o: u32, ty: u32, size: u32 },
    Fixed([f32; 4]),
    Default,
}

/// Monta o draw (índices já lidos em `idx`, faixa `lo..=hi`). None = algo que o caminho da GPU não cobre (memória
/// não mapeada, dados grandes demais): o chamador roda o shader na CPU.
pub(super) fn prepare(st: &State, ctx: &Ctx, idx: &[u32], lo: u32, hi: u32, topo: u32) -> Option<Prepared> {
    let r = &st.regs;
    let base = r[0x200] << 3;
    let fmt_lo = r[0x201];
    let fmt_hi = r[0x202];
    let fixed_mask = (fmt_hi >> 16) & 0xfff;
    let total = ((fmt_hi >> 28) & 0xf) + 1;
    let fmt = |a: u32| -> (u32, u32) {
        let f = if a < 8 { fmt_lo >> (4 * a) } else { fmt_hi >> (4 * (a - 8)) } & 0xf;
        (f & 3, (f >> 2) + 1)
    };

    // atributos (load_vertex): o último carregador que cita o atributo vence; atributo fixo sobrepõe
    let mut attrs = [Attr::Default; 12];
    let mut used_end = [0u32; 12];
    for l in 0..12usize {
        let c_lo = r[0x204 + 3 * l];
        let c_hi = r[0x205 + 3 * l];
        let n = (c_hi >> 28) & 0xf;
        let mut o = 0u32;
        for k in 0..n.min(12) {
            let id = if k < 8 { c_lo >> (4 * k) } else { c_hi >> (4 * (k - 8)) } & 0xf;
            if id < 12 {
                let (ty, size) = fmt(id);
                let es = [1, 1, 2, 4][ty as usize];
                o = o.div_ceil(es) * es;
                attrs[id as usize] = Attr::Loaded { l, o, ty, size };
                o += size * es;
            } else {
                o = o.div_ceil(4) * 4 + (id - 11) * 4;
            }
        }
    }
    for a in 0..total.min(12) {
        if fixed_mask >> a & 1 != 0 {
            attrs[a as usize] = Attr::Fixed(st.fixed_attr[a as usize]);
        }
    }
    // atributo → registrador de entrada (registrador sem atributo = 0, como `Exec::new`)
    let nin = (r[0x2b9] & 0xf) + 1;
    let perm = r[0x2bb] as u64 | (r[0x2bc] as u64) << 32;
    let mut reg_attr: [Option<usize>; 16] = [None; 16];
    for a in 0..nin.min(12) {
        reg_attr[((perm >> (4 * a)) & 0xf) as usize] = Some(a as usize);
    }
    // bytes por vértice que cada carregador realmente lê (só dos atributos que chegam a um registrador)
    for a in reg_attr.iter().flatten() {
        if let Attr::Loaded { l, o, ty, size } = attrs[*a] {
            let es = [1, 1, 2, 4][ty as usize];
            used_end[l] = used_end[l].max(o + size * es);
        }
    }
    // trechos da memória guest: [vértice lo, vértice hi + bytes lidos), alinhados a 4 (o shader lê palavras)
    let mut spans: Vec<(u32, u32)> = Vec::new();
    let mut span_pos = [0u32; 12];
    let mut pos = 0u32;
    for l in 0..12usize {
        if used_end[l] == 0 {
            continue;
        }
        let stride = (r[0x205 + 3 * l] >> 16) & 0xff;
        let a = va(base + r[0x203 + 3 * l]);
        let start = a.wrapping_add(lo.wrapping_mul(stride));
        let end = a.wrapping_add(hi.wrapping_mul(stride)).wrapping_add(used_end[l]);
        let (cs, ce) = (start & !3, end.wrapping_add(3) & !3);
        if ce <= cs || !crate::hle::mem::is_mapped(cs) || !crate::hle::mem::is_mapped(ce - 1) {
            return None;
        }
        span_pos[l] = pos + (start - cs);
        spans.push((cs, ce - cs));
        pos = pos.checked_add(ce - cs)?;
    }
    if pos as u64 > crate::render::vulkan::VDATA_RANGE {
        return None;
    }

    let mut sh = Box::new(VsShading {
        f: [[0.0; 4]; 96],
        i: [[0; 4]; 4],
        misc: [0; 4],
        sem: [[SEM_NONE; 4]; 6],
        vp: ctx.vp,
        zmap: [ctx.zscale, ctx.zoffset, 0.0, 0.0],
        clip: [f24(r[0x48]), f24(r[0x49]), f24(r[0x4a]), f24(r[0x4b])],
        attr: [[0; 4]; 16],
        defs: [[0.0; 4]; 16],
    });
    for (reg, a) in reg_attr.iter().enumerate() {
        let Some(a) = *a else { continue };
        match attrs[a] {
            Attr::Loaded { l, o, ty, size } => {
                let stride = (r[0x205 + 3 * l] >> 16) & 0xff;
                sh.attr[reg] = [span_pos[l] + o, stride, ty | size << 8, 0];
                sh.defs[reg] = [0.0, 0.0, 0.0, 1.0];
            }
            Attr::Fixed(v) => sh.defs[reg] = v,
            Attr::Default => sh.defs[reg] = [0.0, 0.0, 0.0, 1.0],
        }
    }

    // uniforms
    sh.f.copy_from_slice(&st.vs.f[..96]);
    for (k, iu) in st.vs.i.iter().enumerate() {
        sh.i[k] = [iu[0] as i32, iu[1] as i32, iu[2] as i8 as i32, 0];
    }
    let user_clip = r[0x47] & 1 != 0;
    if user_clip {
        warn_user_clip(&sh.clip);
    }
    let flags = if ctx.wbuffer { VS_F_WBUFFER } else { 0 } | if user_clip { VS_F_USER_CLIP } else { 0 };
    sh.misc = [st.vs.b as u32, st.vs.entry, flags, 0];

    // saídas → semânticas (output_vertex)
    let mask = r[0x2bd] & 0xffff;
    let total_out = (r[0x4f] & 7) as usize;
    let packed = (0..16u32).filter(|i| mask >> i & 1 != 0);
    for (i, reg) in packed.enumerate().take(total_out.min(7)) {
        let map = r[0x50 + i];
        for c in 0..4u32 {
            let s = ((map >> (8 * c)) & 0x1f) as usize;
            if s < 24 {
                sh.sem[s / 4][s % 4] = reg * 4 + c;
            }
        }
    }

    // montagem de primitivas (igual a `pica::draw`) → lista de triângulos
    let mut indices = Vec::with_capacity(idx.len() * if topo == 0 || topo == 3 { 1 } else { 3 });
    let mut buf = [0u32; 2];
    let mut bi = 0usize;
    let mut ready = false;
    for &i in idx {
        let v = i - lo;
        match topo {
            1 | 2 => {
                if ready {
                    indices.extend_from_slice(&[buf[0], buf[1], v]);
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
                    indices.extend_from_slice(&[buf[0], buf[1], v]);
                }
            }
        }
    }
    Some(Prepared { indices, spans, shading: sh })
}

/// O plano de recorte do usuário (0x47) é feito com gl_ClipDistance só no caminho da GPU (o soft não tem) — avisa
/// uma vez quando o jogo o liga, para saber se a diferença importa.
fn warn_user_clip(c: &[f32; 4]) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| eprintln!("[pica] plano de recorte do usuário ligado ({c:?}) — só o shader de vértice na GPU o aplica"));
}
