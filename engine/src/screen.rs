//! As duas telas do 3DS compostas numa janela: superior 400×240 em cima, inferior 320×240
//! centralizada embaixo.

pub const TOP_W: usize = 400;
pub const TOP_H: usize = 240;
pub const BOTTOM_W: usize = 320;
pub const BOTTOM_H: usize = 240;
pub const WIN_W: usize = TOP_W;
pub const WIN_H: usize = TOP_H + BOTTOM_H;

pub struct Screens {
    pub top: Vec<u32>,
    pub bottom: Vec<u32>,
}

impl Screens {
    pub fn new() -> Screens {
        Screens {
            top: vec![0; TOP_W * TOP_H],
            bottom: vec![0; BOTTOM_W * BOTTOM_H],
        }
    }

    pub fn clear(&mut self) {
        self.top.fill(0);
        self.bottom.fill(0);
    }

    /// Compõe a janela em `scale`× (400·scale × 480·scale). As telas 2D (resolução do 3DS) são ampliadas por
    /// vizinho mais próximo; `hi_top` (400·scale × 240·scale), se houver, substitui a tela de cima (3D nativo).
    pub fn compose_scaled(&self, out: &mut Vec<u32>, scale: usize, hi_top: Option<&[u32]>) {
        let (w, h) = (WIN_W * scale, WIN_H * scale);
        out.resize(w * h, 0);
        match hi_top {
            Some(top) => out[..w * TOP_H * scale].copy_from_slice(top),
            None => {
                for y in 0..TOP_H * scale {
                    let src = &self.top[(y / scale) * TOP_W..(y / scale + 1) * TOP_W];
                    for (x, o) in out[y * w..(y + 1) * w].iter_mut().enumerate() {
                        *o = src[x / scale];
                    }
                }
            }
        }
        let x0 = (WIN_W - BOTTOM_W) / 2 * scale;
        for y in 0..BOTTOM_H * scale {
            let row = &mut out[(TOP_H * scale + y) * w..(TOP_H * scale + y + 1) * w];
            row.fill(0);
            let src = &self.bottom[(y / scale) * BOTTOM_W..(y / scale + 1) * BOTTOM_W];
            for (x, o) in row[x0..x0 + BOTTOM_W * scale].iter_mut().enumerate() {
                *o = src[x / scale];
            }
        }
    }

    #[cfg(test)]
    pub fn compose(&self, out: &mut [u32]) {
        out[..TOP_W * TOP_H].copy_from_slice(&self.top);
        let x0 = (WIN_W - BOTTOM_W) / 2;
        for y in 0..BOTTOM_H {
            let row = &mut out[(TOP_H + y) * WIN_W..(TOP_H + y + 1) * WIN_W];
            row.fill(0);
            row[x0..x0 + BOTTOM_W].copy_from_slice(&self.bottom[y * BOTTOM_W..(y + 1) * BOTTOM_W]);
        }
    }
}
