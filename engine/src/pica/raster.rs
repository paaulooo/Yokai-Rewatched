//! Rasterização e operações por fragmento do PICA (semântica do `rasterizer.cpp` do Citra/Azahar):
//! ponto fixo 12.4 com regra de preenchimento (arestas da direita e de baixo planas não desenham), interpolação com
//! correção de perspectiva, texturas, TEV (6 estágios + buffer de combinação), alpha test, stencil, depth, blending
//! / logic op e escrita no color buffer em blocos 8×8 (linhas de baixo para cima: `y = altura − 1 − y`).
//!
//! [H] / falta: iluminação por fragmento (as cores "primária/secundária de fragmento" valem 0, como no Citra com a
//! iluminação desligada — com ela ligada usamos a cor do vértice, ver `Ctx::lighting`), fog, procedural texture,
//! shadow/gas.

use super::texture::{sample, Tex};
use crate::hle::gpu::{read_pixel, tiled_offset, write_pixel};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use yw1_rt::{r16, r32, r8, w16, w32, w8};

/// Contadores de diagnóstico (por draw, zerados pelo chamador): triângulos, recortados fora, descartados por
/// orientação, fragmentos, reprovados no alpha / stencil / depth, escritos.
pub static STATS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
fn stat(i: usize) {
    STATS[i].fetch_add(1, Relaxed);
}

/// Vértice depois do shader, em coordenadas de recorte.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vtx {
    pub pos: [f32; 4],
    pub color: [f32; 4],
    pub tc0: [f32; 2],
    pub tc1: [f32; 2],
    pub tc2: [f32; 2],
    pub tc0w: f32,
    pub view: [f32; 3],
    pub quat: [f32; 4],
}

impl Vtx {
    fn lerp(&self, o: &Vtx, t: f32) -> Vtx {
        let l = |a: f32, b: f32| a + (b - a) * t;
        let l2 = |a: [f32; 2], b: [f32; 2]| [l(a[0], b[0]), l(a[1], b[1])];
        let l3 = |a: [f32; 3], b: [f32; 3]| [l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2])];
        let l4 = |a: [f32; 4], b: [f32; 4]| [l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2]), l(a[3], b[3])];
        Vtx {
            pos: l4(self.pos, o.pos),
            color: l4(self.color, o.color),
            tc0: l2(self.tc0, o.tc0),
            tc1: l2(self.tc1, o.tc1),
            tc2: l2(self.tc2, o.tc2),
            tc0w: l(self.tc0w, o.tc0w),
            view: l3(self.view, o.view),
            quat: l4(self.quat, o.quat),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Stage {
    pub src_rgb: [u32; 3],
    pub src_a: [u32; 3],
    pub op_rgb: [u32; 3],
    pub op_a: [u32; 3],
    pub comb_rgb: u32,
    pub comb_a: u32,
    pub konst: [u8; 4],
    pub scale_rgb: u32,
    pub scale_a: u32,
}

/// Estado por draw (lido dos registradores uma vez).
pub struct Ctx {
    pub color_addr: u32,
    pub depth_addr: u32,
    pub width: u32,
    /// campo de altura do registrador (= altura − 1)
    pub height_m1: u32,
    pub color_fmt: u32,
    pub depth_fmt: u32,
    pub vp: [f32; 4],
    pub zscale: f32,
    pub zoffset: f32,
    pub wbuffer: bool,
    pub cull: u32,
    pub scissor: (u32, [u32; 4]),
    pub tex: [Option<Tex>; 3],
    pub tex2_uses_tc1: bool,
    pub stages: [Stage; 6],
    pub buffer_color: [u8; 4],
    pub buffer_update: u32,
    pub lighting: bool,
    pub alpha_test: (bool, u32, u8),
    pub stencil: (bool, u32, u8, u8, u8, [u32; 3]),
    pub depth_test: bool,
    pub depth_func: u32,
    pub depth_write: bool,
    pub color_mask: u32,
    pub color_read: bool,
    pub color_write: bool,
    pub depth_read: bool,
    pub depth_rw: bool,
    pub blend: bool,
    pub blend_eq: (u32, u32),
    pub blend_fac: [u32; 4],
    pub blend_color: [u8; 4],
    pub logic_op: u32,
}

/// Recorta contra os planos do PICA (Citra `clipper.cpp`): −w ≤ x,y ≤ w, −w ≤ z ≤ 0, w ≥ ε.
pub fn clip(tri: [Vtx; 3]) -> Vec<Vtx> {
    const EPS: f32 = 1.0 / (1u32 << 23) as f32;
    let planes: [([f32; 4], f32); 7] = [
        ([-1.0, 0.0, 0.0, 1.0], 0.0),
        ([1.0, 0.0, 0.0, 1.0], 0.0),
        ([0.0, -1.0, 0.0, 1.0], 0.0),
        ([0.0, 1.0, 0.0, 1.0], 0.0),
        ([0.0, 0.0, -1.0, 0.0], 0.0),
        ([0.0, 0.0, 1.0, 1.0], 0.0),
        ([0.0, 0.0, 0.0, 1.0], EPS),
    ];
    let dist = |v: &Vtx, p: &([f32; 4], f32)| -> f32 {
        p.0[0] * v.pos[0] + p.0[1] * v.pos[1] + p.0[2] * v.pos[2] + p.0[3] * v.pos[3] - p.1
    };
    let mut poly: Vec<Vtx> = tri.to_vec();
    for p in &planes {
        if poly.is_empty() {
            break;
        }
        // atalho: todos dentro
        if poly.iter().all(|v| dist(v, p) >= 0.0) {
            continue;
        }
        let mut out = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let a = &poly[i];
            let b = &poly[(i + 1) % poly.len()];
            let (da, db) = (dist(a, p), dist(b, p));
            if da >= 0.0 {
                out.push(*a);
            }
            if (da >= 0.0) != (db >= 0.0) {
                out.push(a.lerp(b, da / (da - db)));
            }
        }
        poly = out;
    }
    poly
}

struct Screen {
    /// 12.4 fixo
    x: i32,
    y: i32,
    z: f32,
    inv_w: f32,
    v: Vtx,
}

fn to_screen(c: &Ctx, v: &Vtx) -> Screen {
    let inv_w = 1.0 / v.pos[3];
    let sx = (v.pos[0] * inv_w + 1.0) * c.vp[0] + c.vp[2];
    let sy = (v.pos[1] * inv_w + 1.0) * c.vp[1] + c.vp[3];
    let z = if c.wbuffer { v.pos[2] } else { v.pos[2] * inv_w };
    Screen {
        x: (sx * 16.0).round() as i32,
        y: (sy * 16.0).round() as i32,
        z: c.zoffset + z * c.zscale,
        inv_w,
        v: *v,
    }
}

fn area(a: (i32, i32), b: (i32, i32), p: (i32, i32)) -> i64 {
    (b.0 - a.0) as i64 * (p.1 - a.1) as i64 - (b.1 - a.1) as i64 * (p.0 - a.0) as i64
}

fn right_or_flat_bottom(v: (i32, i32), l1: (i32, i32), l2: (i32, i32)) -> bool {
    if l1.1 == l2.1 {
        v.1 < l1.1
    } else {
        (v.0 as i64) < l1.0 as i64 + (l2.0 - l1.0) as i64 * (v.1 - l1.1) as i64 / (l2.1 - l1.1) as i64
    }
}

/// Desenha um triângulo já recortado (vértices em coordenadas de recorte).
pub fn triangle(c: &Ctx, a: &Vtx, b: &Vtx, d: &Vtx) {
    stat(0);
    let poly = clip([*a, *b, *d]);
    if poly.len() < 3 {
        stat(1);
        return;
    }
    let s: Vec<Screen> = poly.iter().map(|v| to_screen(c, v)).collect();
    for i in 1..s.len() - 1 {
        raster(c, &s[0], &s[i], &s[i + 1], false);
    }
}

fn raster(c: &Ctx, v0: &Screen, v1: &Screen, v2: &Screen, reversed: bool) {
    let p = |v: &Screen| (v.x, v.y);
    let a = area(p(v0), p(v1), p(v2));
    match c.cull {
        0 => {
            if !reversed && a <= 0 {
                return raster(c, v0, v2, v1, true);
            }
        }
        _ => {
            if !reversed && c.cull == 1 {
                return raster(c, v0, v2, v1, true);
            }
            if a <= 0 {
                stat(2);
                return;
            }
        }
    }
    let (p0, p1, p2) = (p(v0), p(v1), p(v2));
    let mut min_x = p0.0.min(p1.0).min(p2.0) & !15;
    let mut min_y = p0.1.min(p1.1).min(p2.1) & !15;
    let mut max_x = (p0.0.max(p1.0).max(p2.0) + 15) & !15;
    let mut max_y = (p0.1.max(p1.1).max(p2.1) + 15) & !15;
    let fb_h = (c.height_m1 + 1) as i32;
    min_x = min_x.max(0);
    min_y = min_y.max(0);
    max_x = max_x.min(c.width as i32 * 16);
    max_y = max_y.min(fb_h * 16);
    let (smode, sc) = c.scissor;
    if smode == 3 {
        min_x = min_x.max(sc[0] as i32 * 16);
        min_y = min_y.max(sc[1] as i32 * 16);
        max_x = max_x.min((sc[2] as i32 + 1) * 16);
        max_y = max_y.min((sc[3] as i32 + 1) * 16);
    }
    let b0 = if right_or_flat_bottom(p0, p1, p2) { -1 } else { 0 };
    let b1 = if right_or_flat_bottom(p1, p2, p0) { -1 } else { 0 };
    let b2 = if right_or_flat_bottom(p2, p0, p1) { -1 } else { 0 };
    let mut y = min_y + 8;
    while y < max_y {
        let mut x = min_x + 8;
        while x < max_x {
            let w0 = b0 + area(p1, p2, (x, y));
            let w1 = b1 + area(p2, p0, (x, y));
            let w2 = b2 + area(p0, p1, (x, y));
            if w0 >= 0 && w1 >= 0 && w2 >= 0 {
                let px = (x >> 4) as u32;
                let py = (y >> 4) as u32;
                let excluded = smode == 1 && px >= sc[0] && px <= sc[2] && py >= sc[1] && py <= sc[3];
                if !excluded {
                    let sum = (w0 + w1 + w2) as f32;
                    let bc = [w0 as f32 / sum, w1 as f32 / sum, w2 as f32 / sum];
                    fragment(c, px, py, v0, v1, v2, bc);
                }
            }
            x += 16;
        }
        y += 16;
    }
}

#[allow(clippy::too_many_arguments)]
fn fragment(c: &Ctx, x: u32, y: u32, v0: &Screen, v1: &Screen, v2: &Screen, bc: [f32; 3]) {
    stat(3);
    let wi = v0.inv_w * bc[0] + v1.inv_w * bc[1] + v2.inv_w * bc[2];
    let persp = |f: &dyn Fn(&Vtx) -> f32| -> f32 {
        (f(&v0.v) * v0.inv_w * bc[0] + f(&v1.v) * v1.inv_w * bc[1] + f(&v2.v) * v2.inv_w * bc[2]) / wi
    };
    // profundidade
    let z = (v0.z * bc[0] + v1.z * bc[1] + v2.z * bc[2]).clamp(0.0, 1.0);
    let fy = c.height_m1 - y;
    let dbpp = match c.depth_fmt {
        0 => 2,
        1 => 3,
        2 => 3,
        _ => 4,
    };
    let dmax = if c.depth_fmt == 0 { 0xffff as f32 } else { 0xff_ffff as f32 };
    let zi = (z * dmax) as u32;
    let daddr = c.depth_addr + tiled_offset(x, fy, c.width, dbpp);
    let have_depth = c.depth_addr != 0;

    // cor primária (vértice): valor absoluto saturado antes da interpolação (Citra)
    let col = |k: usize| (persp(&|v: &Vtx| v.color[k].abs().min(1.0)) * 255.0) as u8;
    let primary = [col(0), col(1), col(2), col(3)];
    let uv = |k: usize| -> [f32; 2] {
        match k {
            0 => [persp(&|v: &Vtx| v.tc0[0]), persp(&|v: &Vtx| v.tc0[1])],
            1 => [persp(&|v: &Vtx| v.tc1[0]), persp(&|v: &Vtx| v.tc1[1])],
            _ => [persp(&|v: &Vtx| v.tc2[0]), persp(&|v: &Vtx| v.tc2[1])],
        }
    };
    let mut texs = [[0u8, 0, 0, 255]; 3];
    for (i, t) in c.tex.iter().enumerate() {
        if let Some(t) = t {
            let k = if i == 2 && c.tex2_uses_tc1 { 1 } else { i };
            let [u, v] = uv(k);
            texs[i] = sample(t, u, v);
        }
    }
    // iluminação por fragmento: não implementada [H] — com ela ligada, usa a cor do vértice como luz primária
    let (frag_p, frag_s) = if c.lighting {
        ([255, 255, 255, 255], [0, 0, 0, 0])
    } else {
        ([0u8; 4], [0u8; 4])
    };

    // TEV
    let mut out = primary;
    let mut buffer = [0u8; 4];
    let mut next_buffer = c.buffer_color;
    for (si, st) in c.stages.iter().enumerate() {
        let get = |src: u32| -> [u8; 4] {
            match src {
                0 => primary,
                1 => frag_p,
                2 => frag_s,
                3 => texs[0],
                4 => texs[1],
                5 => texs[2],
                0xd => buffer,
                0xe => st.konst,
                0xf => out,
                _ => [0, 0, 0, 0],
            }
        };
        let rgb_in: [[u8; 3]; 3] = [0, 1, 2].map(|k| {
            let s = get(st.src_rgb[k]);
            let (r, g, b, a) = (s[0], s[1], s[2], s[3]);
            match st.op_rgb[k] {
                0 => [r, g, b],
                1 => [255 - r, 255 - g, 255 - b],
                2 => [a; 3],
                3 => [255 - a; 3],
                4 => [r; 3],
                5 => [255 - r; 3],
                8 => [g; 3],
                9 => [255 - g; 3],
                12 => [b; 3],
                13 => [255 - b; 3],
                _ => [r, g, b],
            }
        });
        let a_in: [u8; 3] = [0, 1, 2].map(|k| {
            let s = get(st.src_a[k]);
            match st.op_a[k] {
                0 => s[3],
                1 => 255 - s[3],
                2 => s[0],
                3 => 255 - s[0],
                4 => s[1],
                5 => 255 - s[1],
                6 => s[2],
                _ => 255 - s[2],
            }
        });
        let comb = |op: u32, a: i32, b: i32, cc: i32| -> i32 {
            match op {
                0 => a,
                1 => a * b / 255,
                2 => (a + b).min(255),
                3 => (a + b - 128).clamp(0, 255),
                4 => (a * cc + b * (255 - cc)) / 255,
                5 => (a - b).max(0),
                8 => ((a * b + 255 * cc) / 255).min(255),
                9 => ((a + b).min(255) * cc) / 255,
                _ => a,
            }
        };
        let mut rgb = [0i32; 3];
        let dot = matches!(st.comb_rgb, 6 | 7);
        if dot {
            let (x, yv) = (rgb_in[0], rgb_in[1]);
            let d: i32 = (0..3)
                .map(|k| ((x[k] as i32 * 2 - 255) * (yv[k] as i32 * 2 - 255) + 128) / 256)
                .sum();
            rgb = [d.clamp(0, 255); 3];
        } else {
            for k in 0..3 {
                rgb[k] = comb(
                    st.comb_rgb,
                    rgb_in[0][k] as i32,
                    rgb_in[1][k] as i32,
                    rgb_in[2][k] as i32,
                );
            }
        }
        let mut al = if st.comb_rgb == 7 {
            rgb[0]
        } else {
            comb(st.comb_a, a_in[0] as i32, a_in[1] as i32, a_in[2] as i32)
        };
        let sr = 1 << st.scale_rgb.min(2);
        let sa = 1 << st.scale_a.min(2);
        for v in &mut rgb {
            *v = (*v * sr).min(255);
        }
        al = (al * sa).min(255);
        out = [rgb[0] as u8, rgb[1] as u8, rgb[2] as u8, al as u8];
        buffer = next_buffer;
        if si < 4 {
            if c.buffer_update >> si & 1 != 0 {
                next_buffer[0] = out[0];
                next_buffer[1] = out[1];
                next_buffer[2] = out[2];
            }
            if c.buffer_update >> (4 + si) & 1 != 0 {
                next_buffer[3] = out[3];
            }
        }
    }

    // alpha test
    let (at, af, aref) = c.alpha_test;
    if at && !test(af, out[3] as u32, aref as u32) {
        stat(4);
        return;
    }
    // stencil + depth
    let (st_on, st_func, st_wmask, st_ref, st_imask, st_ops) = c.stencil;
    let mut stencil_val = 0u8;
    let mut dval = 0u32;
    if have_depth && c.depth_read {
        dval = match dbpp {
            2 => r16(daddr),
            3 => r8(daddr) | r8(daddr + 1) << 8 | r8(daddr + 2) << 16,
            _ => r32(daddr) & 0xff_ffff,
        };
        if dbpp == 4 {
            stencil_val = (r32(daddr) >> 24) as u8;
        }
    }
    let stencil_ops = c.depth_fmt == 3 && st_on && have_depth;
    let write_stencil = |op: u32, sv: u8| {
        if !stencil_ops {
            return;
        }
        let nv: u8 = match op {
            0 => sv,
            1 => 0,
            2 => st_ref,
            3 => sv.saturating_add(1),
            4 => sv.saturating_sub(1),
            5 => !sv,
            6 => sv.wrapping_add(1),
            _ => sv.wrapping_sub(1),
        };
        let v = (sv & !st_wmask) | (nv & st_wmask);
        let w = r32(daddr);
        w32(daddr, (w & 0x00ff_ffff) | (v as u32) << 24);
    };
    if stencil_ops {
        let pass = test(st_func, (st_ref & st_imask) as u32, (stencil_val & st_imask) as u32);
        if !pass {
            stat(5);
            write_stencil(st_ops[0], stencil_val);
            return;
        }
    }
    if c.depth_test && have_depth && !test(c.depth_func, zi, dval) {
        stat(6);
        write_stencil(st_ops[1], stencil_val);
        return;
    }
    write_stencil(st_ops[2], stencil_val);
    if have_depth && c.depth_write && c.depth_rw {
        match dbpp {
            2 => w16(daddr, zi),
            3 => {
                w8(daddr, zi);
                w8(daddr + 1, zi >> 8);
                w8(daddr + 2, zi >> 16);
            }
            _ => {
                let w = r32(daddr);
                w32(daddr, (w & 0xff00_0000) | (zi & 0xff_ffff));
            }
        }
    }

    // blending / logic op e escrita
    if c.color_addr == 0 || !c.color_write || c.color_mask == 0 {
        return;
    }
    let bpp = match c.color_fmt {
        0 => 4,
        1 => 3,
        _ => 2,
    };
    let caddr = c.color_addr + tiled_offset(x, fy, c.width, bpp);
    let dst = if c.color_read {
        read_pixel(caddr, c.color_fmt)
    } else {
        [0, 0, 0, 0]
    };
    let src = out;
    let res = if c.blend {
        blend(c, src, dst)
    } else {
        logic(c.logic_op, src, dst)
    };
    let mut fin = dst;
    for k in 0..4 {
        // máscara: bit 0 = R … bit 3 = A
        if c.color_mask >> k & 1 != 0 {
            fin[k] = res[k];
        }
    }
    stat(7);
    write_pixel(caddr, c.color_fmt, fin);
}

fn test(func: u32, a: u32, b: u32) -> bool {
    match func {
        0 => false,
        1 => true,
        2 => a == b,
        3 => a != b,
        4 => a < b,
        5 => a <= b,
        6 => a > b,
        _ => a >= b,
    }
}

fn blend(c: &Ctx, s: [u8; 4], d: [u8; 4]) -> [u8; 4] {
    let f = |fac: u32, k: usize, alpha: bool| -> i32 {
        let (s, d, cc) = (s.map(|x| x as i32), d.map(|x| x as i32), c.blend_color.map(|x| x as i32));
        match fac {
            0 => 0,
            1 => 255,
            2 => s[k],
            3 => 255 - s[k],
            4 => d[k],
            5 => 255 - d[k],
            6 => s[3],
            7 => 255 - s[3],
            8 => d[3],
            9 => 255 - d[3],
            10 => cc[k],
            11 => 255 - cc[k],
            12 => cc[3],
            13 => 255 - cc[3],
            _ => {
                if alpha {
                    255
                } else {
                    s[3].min(255 - d[3])
                }
            }
        }
    };
    let eq = |e: u32, a: i32, b: i32, sv: i32, dv: i32| -> u8 {
        let r = match e {
            0 => a + b,
            1 => a - b,
            2 => b - a,
            3 => sv.min(dv) * 255,
            _ => sv.max(dv) * 255,
        };
        (r / 255).clamp(0, 255) as u8
    };
    let mut o = [0u8; 4];
    for k in 0..3 {
        let a = s[k] as i32 * f(c.blend_fac[0], k, false);
        let b = d[k] as i32 * f(c.blend_fac[1], k, false);
        o[k] = eq(c.blend_eq.0, a, b, s[k] as i32, d[k] as i32);
    }
    let a = s[3] as i32 * f(c.blend_fac[2], 3, true);
    let b = d[3] as i32 * f(c.blend_fac[3], 3, true);
    o[3] = eq(c.blend_eq.1, a, b, s[3] as i32, d[3] as i32);
    o
}

fn logic(op: u32, s: [u8; 4], d: [u8; 4]) -> [u8; 4] {
    let mut o = [0u8; 4];
    for k in 0..4 {
        let (a, b) = (s[k], d[k]);
        o[k] = match op {
            0 => 0,
            1 => a & b,
            2 => a & !b,
            3 => a,
            4 => 255,
            5 => !a,
            6 => b,
            7 => !b,
            8 => !(a & b),
            9 => a | b,
            10 => !(a | b),
            11 => a ^ b,
            12 => !(a ^ b),
            13 => !a & b,
            14 => a | !b,
            _ => !a | b,
        };
    }
    o
}
