//! Rasterizador 3D em software com resolução interna ajustável (o jogo é desenhado direto na resolução
//! pedida, não ampliado depois): projeção em perspectiva, recorte no plano próximo, buffer de profundidade
//! (1/w), UV com correção de perspectiva, cor de vértice × textura e teste de alfa. É um stand-in do
//! renderizador PICA (que virá depois): `Room` usa isto para provar geometria, materiais e câmera.

use crate::assets::imgc::Image;

pub type V3 = [f32; 3];

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
pub fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-9);
    [a[0] / l, a[1] / l, a[2] / l]
}

pub struct Camera {
    pub eye: V3,
    right: V3,
    up: V3,
    fwd: V3,
    /// FOV vertical em radianos.
    pub fov_y: f32,
    pub near: f32,
}

impl Camera {
    pub fn look_at(eye: V3, target: V3, fov_y_deg: f32, near: f32) -> Camera {
        let fwd = norm(sub(target, eye));
        let right = norm(cross(fwd, [0.0, 1.0, 0.0]));
        let up = cross(right, fwd);
        Camera {
            eye,
            right,
            up,
            fwd,
            fov_y: fov_y_deg.to_radians(),
            near,
        }
    }

    pub fn forward(&self) -> V3 {
        self.fwd
    }

    /// Ponto do mundo → pixel numa tela `w`×`h` (None se estiver atrás da câmera).
    pub fn project(&self, p: V3, w: f32, h: f32) -> Option<(f32, f32)> {
        let v = self.to_view(p);
        if v[2] < self.near {
            return None;
        }
        let focal = 0.5 * h / (self.fov_y * 0.5).tan();
        Some((w * 0.5 + v[0] / v[2] * focal, h * 0.5 - v[1] / v[2] * focal))
    }

    fn to_view(&self, p: V3) -> V3 {
        let d = sub(p, self.eye);
        [dot(d, self.right), dot(d, self.up), dot(d, self.fwd)]
    }
}

pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub color: Vec<u32>,
    inv_w: Vec<f32>,
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Frame {
        Frame {
            width,
            height,
            color: vec![0; width * height],
            inv_w: vec![0.0; width * height],
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        if (self.width, self.height) != (width, height) {
            *self = Frame::new(width, height);
        }
    }

    pub fn clear(&mut self, rgb: u32) {
        self.color.fill(rgb);
        self.inv_w.fill(0.0);
    }
}

#[derive(Clone, Copy)]
struct Vtx {
    p: V3, // espaço da câmera
    c: [f32; 3],
    a: f32,
    uv: [f32; 2],
}

fn lerp(a: Vtx, b: Vtx, t: f32) -> Vtx {
    let m = |x: f32, y: f32| x + (y - x) * t;
    Vtx {
        p: [m(a.p[0], b.p[0]), m(a.p[1], b.p[1]), m(a.p[2], b.p[2])],
        c: [m(a.c[0], b.c[0]), m(a.c[1], b.c[1]), m(a.c[2], b.c[2])],
        a: m(a.a, b.a),
        uv: [m(a.uv[0], b.uv[0]), m(a.uv[1], b.uv[1])],
    }
}

pub struct MeshRef<'a> {
    pub pos: &'a [V3],
    pub color: &'a [[f32; 4]],
    pub uv: &'a [[f32; 2]],
    pub tris: &'a [[u32; 3]],
    pub offset: V3,
    pub texture: Option<&'a Image>,
    /// Mistura alfa de verdade (sem escrever profundidade) em vez de teste de alfa.
    pub blend: bool,
    /// Multiplicador de cor (luz ambiente do mapa etc.).
    pub tint: [f32; 3],
}

pub fn draw_mesh(f: &mut Frame, cam: &Camera, m: &MeshRef) {
    let view: Vec<V3> = m
        .pos
        .iter()
        .map(|p| cam.to_view([p[0] + m.offset[0], p[1] + m.offset[1], p[2] + m.offset[2]]))
        .collect();
    for t in m.tris {
        let v = t.map(|i| {
            let i = i as usize;
            Vtx {
                p: view[i],
                c: [
                    m.color[i][0] * m.tint[0],
                    m.color[i][1] * m.tint[1],
                    m.color[i][2] * m.tint[2],
                ],
                a: m.color[i][3],
                uv: m.uv[i],
            }
        });
        draw_clipped(f, cam, v, m.texture, m.blend);
    }
}

fn draw_clipped(f: &mut Frame, cam: &Camera, v: [Vtx; 3], tex: Option<&Image>, blend: bool) {
    let near = cam.near;
    let inside = v.map(|x| x.p[2] >= near);
    let n_in = inside.iter().filter(|&&b| b).count();
    if n_in == 0 {
        return;
    }
    if n_in == 3 {
        raster(f, cam, v, tex, blend);
        return;
    }
    // Sutherland-Hodgman contra z = near.
    let mut poly: Vec<Vtx> = Vec::with_capacity(4);
    for i in 0..3 {
        let (a, b) = (v[i], v[(i + 1) % 3]);
        let (ia, ib) = (a.p[2] >= near, b.p[2] >= near);
        if ia {
            poly.push(a);
        }
        if ia != ib {
            let t = (near - a.p[2]) / (b.p[2] - a.p[2]);
            poly.push(lerp(a, b, t));
        }
    }
    for k in 1..poly.len().saturating_sub(1) {
        raster(f, cam, [poly[0], poly[k], poly[k + 1]], tex, blend);
    }
}

fn raster(f: &mut Frame, cam: &Camera, v: [Vtx; 3], tex: Option<&Image>, blend: bool) {
    let (w, h) = (f.width as f32, f.height as f32);
    let focal = 0.5 * h / (cam.fov_y * 0.5).tan();
    let proj = |p: V3| [w * 0.5 + p[0] / p[2] * focal, h * 0.5 - p[1] / p[2] * focal];
    let s = v.map(|x| proj(x.p));
    let area =
        (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[1][1] - s[0][1]) * (s[2][0] - s[0][0]);
    if area.abs() < 1e-6 {
        return;
    }
    let min_x = s
        .iter()
        .map(|p| p[0])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let min_y = s
        .iter()
        .map(|p| p[1])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x = (s
        .iter()
        .map(|p| p[0])
        .fold(f32::MIN, f32::max)
        .ceil()
        .min(w)) as usize;
    let max_y = (s
        .iter()
        .map(|p| p[1])
        .fold(f32::MIN, f32::max)
        .ceil()
        .min(h)) as usize;
    if min_x >= max_x || min_y >= max_y {
        return;
    }
    let iw = v.map(|x| 1.0 / x.p[2]);
    let inv_area = 1.0 / area;
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let l0 = ((s[1][0] - px) * (s[2][1] - py) - (s[1][1] - py) * (s[2][0] - px)) * inv_area;
            let l1 = ((s[2][0] - px) * (s[0][1] - py) - (s[2][1] - py) * (s[0][0] - px)) * inv_area;
            let l2 = 1.0 - l0 - l1;
            if l0 < 0.0 || l1 < 0.0 || l2 < 0.0 {
                continue;
            }
            let q = l0 * iw[0] + l1 * iw[1] + l2 * iw[2];
            let idx = y * f.width + x;
            if q <= f.inv_w[idx] {
                continue;
            }
            // Interpolação com correção de perspectiva.
            let (p0, p1, p2) = (l0 * iw[0] / q, l1 * iw[1] / q, l2 * iw[2] / q);
            let mut alpha = if blend {
                p0 * v[0].a + p1 * v[1].a + p2 * v[2].a
            } else {
                1.0
            };
            let mut rgb = [
                p0 * v[0].c[0] + p1 * v[1].c[0] + p2 * v[2].c[0],
                p0 * v[0].c[1] + p1 * v[1].c[1] + p2 * v[2].c[1],
                p0 * v[0].c[2] + p1 * v[1].c[2] + p2 * v[2].c[2],
            ];
            if let Some(t) = tex {
                let u = p0 * v[0].uv[0] + p1 * v[1].uv[0] + p2 * v[2].uv[0];
                let tv = p0 * v[0].uv[1] + p1 * v[1].uv[1] + p2 * v[2].uv[1];
                // V direto (linha 0 da imagem = v 0) — conferido nos móveis do quarto (cama, pôster) e no personagem.
                // Filtro bilinear (o 3DS filtra as texturas); os cantos dão a volta (textura repetida).
                let texel = sample_bilinear(t, u, tv);
                // Teste de alfa 0.4 (valor da sonda em Python; HIPÓTESE, o material real é o .mtr).
                alpha *= texel[0];
                let alpha_test = texel[0];
                if (!blend && alpha_test < 0.4) || (blend && alpha < 0.02) {
                    continue;
                }
                rgb[0] *= texel[1];
                rgb[1] *= texel[2];
                rgb[2] *= texel[3];
            }
            let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0) as u32;
            if blend {
                let dst = f.color[idx];
                let mix = |sh: u32, s: f32| {
                    let d = ((dst >> sh) & 0xff) as f32 / 255.0;
                    c(d + (s - d) * alpha) << sh
                };
                f.color[idx] = mix(16, rgb[0]) | mix(8, rgb[1]) | mix(0, rgb[2]);
            } else {
                f.color[idx] = (c(rgb[0]) << 16) | (c(rgb[1]) << 8) | c(rgb[2]);
                f.inv_w[idx] = q;
            }
        }
    }
}

/// Amostra a textura com filtro bilinear; devolve [alfa, r, g, b] em 0..1. `u` e `v` repetem (fração).
fn sample_bilinear(t: &Image, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (t.width, t.height);
    let fu = ((u.fract() + 1.0).fract() * w as f32 - 0.5).max(-0.5);
    let fv = ((v.fract() + 1.0).fract() * h as f32 - 0.5).max(-0.5);
    let (x0, y0) = (fu.floor(), fv.floor());
    let (ax, ay) = (fu - x0, fv - y0);
    let wrap = |i: f32, n: usize| (i as isize).rem_euclid(n as isize) as usize;
    let (xa, xb) = (wrap(x0, w), wrap(x0 + 1.0, w));
    let (ya, yb) = (wrap(y0, h), wrap(y0 + 1.0, h));
    let px = |x: usize, y: usize| {
        let p = t.pixels[y * w + x];
        [
            (p >> 24) as f32 / 255.0,
            ((p >> 16) & 0xff) as f32 / 255.0,
            ((p >> 8) & 0xff) as f32 / 255.0,
            (p & 0xff) as f32 / 255.0,
        ]
    };
    let (c00, c10, c01, c11) = (px(xa, ya), px(xb, ya), px(xa, yb), px(xb, yb));
    let mut out = [0.0; 4];
    for k in 0..4 {
        let top = c00[k] + (c10[k] - c00[k]) * ax;
        let bot = c01[k] + (c11[k] - c01[k]) * ax;
        out[k] = top + (bot - top) * ay;
    }
    out
}

/// Retângulo/caixa simples (jogador provisório): 12 triângulos coloridos.
pub fn unit_box() -> (Vec<V3>, Vec<[u32; 3]>) {
    let mut p = Vec::new();
    for z in [0.0, 1.0] {
        for y in [0.0, 1.0] {
            for x in [-0.5, 0.5] {
                p.push([x, y, z - 0.5]);
            }
        }
    }
    let q = |a: u32, b: u32, c: u32, d: u32| [[a, b, c], [a, c, d]];
    let mut t = Vec::new();
    for f in [
        q(0, 1, 3, 2),
        q(4, 6, 7, 5),
        q(0, 4, 5, 1),
        q(2, 3, 7, 6),
        q(0, 2, 6, 4),
        q(1, 5, 7, 3),
    ] {
        t.extend(f);
    }
    (p, t)
}
