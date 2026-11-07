//! Rasterizador 2D em software para os layouts de menu: triângulos com textura (UV em texels,
//! repetição nas bordas), amostragem do texel mais próximo e mistura alfa "source over".
//! Suficiente para 400×240 + 320×240; o renderizador PICA de verdade (GPU) vem depois.

use crate::assets::imgc::Image;

pub struct Target<'a> {
    pub pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Copy)]
pub struct Vertex {
    pub x: f32,
    pub y: f32,
    pub u: f32,
    pub v: f32,
}

fn edge(a: Vertex, b: Vertex, px: f32, py: f32) -> f32 {
    (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x)
}

/// Regra de desempate: cada aresta compartilhada é percorrida em sentidos opostos pelos dois
/// triângulos vizinhos, então só um deles fica com os pixels exatamente sobre ela (sem dupla mistura).
fn owns_edge(a: Vertex, b: Vertex) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    dy > 0.0 || (dy == 0.0 && dx < 0.0)
}

/// Núcleo do rasterizador: varre um triângulo e pinta cada pixel com `sample(u, v)` (ABGR em u32) —
/// a função de amostragem decide triângulo texturizado (`draw_triangle`) ou cor sólida (`draw_triangle_flat`).
fn scan(t: &mut Target, mut v: [Vertex; 3], alpha: f32, sample: impl Fn(f32, f32) -> u32) {
    let mut area = edge(v[0], v[1], v[2].x, v[2].y);
    if area == 0.0 {
        return;
    }
    if area < 0.0 {
        v.swap(1, 2);
        area = -area;
    }
    let min_x = v
        .iter()
        .map(|p| p.x)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let min_y = v
        .iter()
        .map(|p| p.y)
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let max_x =
        (v.iter().map(|p| p.x).fold(f32::MIN, f32::max).ceil() as isize).min(t.width as isize);
    let max_y =
        (v.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil() as isize).min(t.height as isize);
    if max_x <= 0 || max_y <= 0 {
        return;
    }
    let edges = [(v[1], v[2]), (v[2], v[0]), (v[0], v[1])];
    let owns = edges.map(|(a, b)| owns_edge(a, b));
    for y in min_y..max_y as usize {
        let py = y as f32 + 0.5;
        for x in min_x..max_x as usize {
            let px = x as f32 + 0.5;
            let mut w = [0f32; 3];
            let mut inside = true;
            for (k, (a, b)) in edges.iter().enumerate() {
                w[k] = edge(*a, *b, px, py);
                if w[k] < 0.0 || (w[k] == 0.0 && !owns[k]) {
                    inside = false;
                    break;
                }
            }
            if !inside {
                continue;
            }
            let (l0, l1, l2) = (w[0] / area, w[1] / area, w[2] / area);
            let u = l0 * v[0].u + l1 * v[1].u + l2 * v[2].u;
            let tv = l0 * v[0].v + l1 * v[1].v + l2 * v[2].v;
            let src = sample(u, tv);
            let a = ((src >> 24) as f32 * alpha) as u32;
            if a == 0 {
                continue;
            }
            let dst = &mut t.pixels[y * t.width + x];
            *dst = if a >= 255 {
                src & 0x00ff_ffff
            } else {
                blend(*dst, src, a)
            };
        }
    }
}

pub fn draw_triangle(t: &mut Target, tex: &Image, v: [Vertex; 3], alpha: f32) {
    let (w, h) = (tex.width as f32, tex.height as f32);
    let px = &tex.pixels;
    let tw = tex.width;
    scan(t, v, alpha, |u, v| {
        let tx = u.floor().rem_euclid(w) as usize;
        let ty = v.floor().rem_euclid(h) as usize;
        px[ty * tw + tx]
    });
}

/// Triângulo de cor sólida (material sem textura — ex. `#cover 0`/`bg_black` nos menus).
/// `color` em ABGR: canal A do argumento × `alpha` da animação.
pub fn draw_triangle_flat(t: &mut Target, v: [Vertex; 3], alpha: f32, color: u32) {
    scan(t, v, alpha, |_, _| color);
}

fn blend(dst: u32, src: u32, a: u32) -> u32 {
    let mix = |s: u32| {
        let (sc, dc) = ((src >> s) & 0xff, (dst >> s) & 0xff);
        ((sc * a + dc * (255 - a)) / 255) << s
    };
    mix(16) | mix(8) | mix(0)
}
