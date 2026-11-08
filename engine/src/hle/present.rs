//! Onde as telas ficam dentro da janela (em pixels físicos) e a conversão do mouse para o toque da tela de baixo.
//! Funções puras, testadas para cada layout. Mesma ideia do `layout` do Zakuro: o conjunto das telas é escalado
//! para caber na janela mantendo a proporção, centralizado; com escala inteira, o fator é arredondado para baixo.

use super::settings::Layout;

pub const TOP: (f32, f32) = (400.0, 240.0);
pub const BOTTOM: (f32, f32) = (320.0, 240.0);

/// Retângulo em pixels da janela.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
}

/// Tamanho do conjunto das telas em escala 1 (pixels do 3DS).
pub fn base_size(layout: Layout) -> (u32, u32) {
    match layout {
        Layout::Empilhadas => (400, 480),
        Layout::LadoALado => (720, 240),
        Layout::SoCima => (400, 240),
        Layout::SoBaixo => (320, 240),
        // tela de baixo com metade do tamanho, à direita da de cima
        Layout::GrandePequena => (560, 240),
    }
}

/// (tela de cima, tela de baixo) dentro de uma janela `win_w`×`win_h`; `None` = tela não aparece.
pub fn place(layout: Layout, win_w: f32, win_h: f32, integer: bool) -> (Option<Rect>, Option<Rect>) {
    let (bw, bh) = base_size(layout);
    let (bw, bh) = (bw as f32, bh as f32);
    let mut s = (win_w / bw).min(win_h / bh);
    if !(s.is_finite() && s > 0.0) {
        return (None, None);
    }
    if integer && s >= 1.0 {
        s = s.floor();
    }
    let mut ox = (win_w - bw * s) / 2.0;
    let mut oy = (win_h - bh * s) / 2.0;
    if integer {
        ox = ox.floor();
        oy = oy.floor();
    }
    let r = |x: f32, y: f32, w: f32, h: f32| Rect { x: ox + x, y: oy + y, w, h };
    let top = || r(0.0, 0.0, TOP.0 * s, TOP.1 * s);
    match layout {
        Layout::Empilhadas => (Some(top()), Some(r(40.0 * s, 240.0 * s, BOTTOM.0 * s, BOTTOM.1 * s))),
        Layout::LadoALado => (Some(top()), Some(r(400.0 * s, 0.0, BOTTOM.0 * s, BOTTOM.1 * s))),
        Layout::SoCima => (Some(top()), None),
        Layout::SoBaixo => (None, Some(r(0.0, 0.0, BOTTOM.0 * s, BOTTOM.1 * s))),
        Layout::GrandePequena => {
            let mut small = s * 0.5;
            if integer && small >= 1.0 {
                small = small.floor();
            }
            (
                Some(top()),
                Some(r(400.0 * s, 240.0 * s - 240.0 * small, BOTTOM.0 * small, BOTTOM.1 * small)),
            )
        }
    }
}

/// Posição do mouse (pixels da janela) → coordenada do toque (0–319 × 0–239), se estiver sobre a tela de baixo.
pub fn touch_point(bottom: Option<Rect>, px: f32, py: f32) -> Option<(u16, u16)> {
    let b = bottom?;
    if !b.contains(px, py) || b.w <= 0.0 || b.h <= 0.0 {
        return None;
    }
    let tx = ((px - b.x) / b.w * BOTTOM.0).floor().clamp(0.0, BOTTOM.0 - 1.0);
    let ty = ((py - b.y) / b.h * BOTTOM.1).floor().clamp(0.0, BOTTOM.1 - 1.0);
    Some((tx as u16, ty as u16))
}

/// Atalho: toque a partir do layout e do tamanho da janela.
pub fn touch_at(layout: Layout, win_w: f32, win_h: f32, integer: bool, px: f32, py: f32) -> Option<(u16, u16)> {
    touch_point(place(layout, win_w, win_h, integer).1, px, py)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empilhadas_escala_2_igual_a_janela_antiga() {
        // a janela minifb era 400×480 com a tela de baixo em x 40..360, y 240..480 (escala 2 = o dobro)
        let (t, b) = place(Layout::Empilhadas, 800.0, 960.0, false);
        assert_eq!(t, Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 480.0 }));
        assert_eq!(b, Some(Rect { x: 80.0, y: 480.0, w: 640.0, h: 480.0 }));
        assert_eq!(touch_at(Layout::Empilhadas, 800.0, 960.0, false, 80.0, 480.0), Some((0, 0)));
        assert_eq!(touch_at(Layout::Empilhadas, 800.0, 960.0, false, 719.9, 959.9), Some((319, 239)));
        assert_eq!(touch_at(Layout::Empilhadas, 800.0, 960.0, false, 400.0, 720.0), Some((160, 120)));
        // na tela de cima ou nas bordas pretas: sem toque
        assert_eq!(touch_at(Layout::Empilhadas, 800.0, 960.0, false, 400.0, 100.0), None);
        assert_eq!(touch_at(Layout::Empilhadas, 800.0, 960.0, false, 40.0, 700.0), None);
        assert_eq!(touch_at(Layout::Empilhadas, 800.0, 960.0, false, 720.0, 700.0), None);
    }

    #[test]
    fn empilhadas_janela_larga_centraliza() {
        // 1000×480: escala 1, sobra 300 px de cada lado
        let (t, b) = place(Layout::Empilhadas, 1000.0, 480.0, false);
        assert_eq!(t.unwrap().x, 300.0);
        assert_eq!(b.unwrap(), Rect { x: 340.0, y: 240.0, w: 320.0, h: 240.0 });
        assert_eq!(touch_at(Layout::Empilhadas, 1000.0, 480.0, false, 340.0, 240.0), Some((0, 0)));
        assert_eq!(touch_at(Layout::Empilhadas, 1000.0, 480.0, false, 500.5, 360.5), Some((160, 120)));
    }

    #[test]
    fn lado_a_lado() {
        let (t, b) = place(Layout::LadoALado, 1440.0, 480.0, false);
        assert_eq!(t.unwrap(), Rect { x: 0.0, y: 0.0, w: 800.0, h: 480.0 });
        assert_eq!(b.unwrap(), Rect { x: 800.0, y: 0.0, w: 640.0, h: 480.0 });
        assert_eq!(touch_at(Layout::LadoALado, 1440.0, 480.0, false, 800.0, 0.0), Some((0, 0)));
        assert_eq!(touch_at(Layout::LadoALado, 1440.0, 480.0, false, 1439.0, 479.0), Some((319, 239)));
        assert_eq!(touch_at(Layout::LadoALado, 1440.0, 480.0, false, 799.0, 10.0), None);
    }

    #[test]
    fn so_cima_nao_tem_toque_e_so_baixo_ocupa_tudo() {
        assert_eq!(place(Layout::SoCima, 800.0, 480.0, false).1, None);
        assert_eq!(touch_at(Layout::SoCima, 800.0, 480.0, false, 400.0, 240.0), None);
        let (t, b) = place(Layout::SoBaixo, 960.0, 720.0, false);
        assert_eq!(t, None);
        assert_eq!(b.unwrap(), Rect { x: 0.0, y: 0.0, w: 960.0, h: 720.0 });
        assert_eq!(touch_at(Layout::SoBaixo, 960.0, 720.0, false, 480.0, 360.0), Some((160, 120)));
    }

    #[test]
    fn grande_pequena() {
        // 1120×480: escala 2; tela de baixo em escala 1 no canto inferior direito
        let (t, b) = place(Layout::GrandePequena, 1120.0, 480.0, false);
        assert_eq!(t.unwrap(), Rect { x: 0.0, y: 0.0, w: 800.0, h: 480.0 });
        assert_eq!(b.unwrap(), Rect { x: 800.0, y: 240.0, w: 320.0, h: 240.0 });
        assert_eq!(touch_at(Layout::GrandePequena, 1120.0, 480.0, false, 800.0, 240.0), Some((0, 0)));
        assert_eq!(touch_at(Layout::GrandePequena, 1120.0, 480.0, false, 1119.0, 479.0), Some((319, 239)));
        assert_eq!(touch_at(Layout::GrandePequena, 1120.0, 480.0, false, 900.0, 100.0), None);
    }

    #[test]
    fn escala_inteira() {
        // 1000×1000 empilhadas: 2,08× → 2×, centralizado em pixels inteiros
        let (t, b) = place(Layout::Empilhadas, 1000.0, 1000.0, true);
        assert_eq!(t.unwrap(), Rect { x: 100.0, y: 20.0, w: 800.0, h: 480.0 });
        assert_eq!(b.unwrap(), Rect { x: 180.0, y: 500.0, w: 640.0, h: 480.0 });
        assert_eq!(touch_at(Layout::Empilhadas, 1000.0, 1000.0, true, 180.0, 500.0), Some((0, 0)));
        assert_eq!(touch_at(Layout::Empilhadas, 1000.0, 1000.0, true, 819.0, 979.0), Some((319, 239)));
        // sem escala inteira a mesma janela usa 2,08×
        let (_, b) = place(Layout::Empilhadas, 1000.0, 1000.0, false);
        assert!((b.unwrap().w - 320.0 * 1000.0 / 480.0).abs() < 1e-3);
        // janela menor que 1×: não arredonda para zero
        let (t, _) = place(Layout::SoCima, 200.0, 120.0, true);
        assert_eq!(t.unwrap().w, 200.0);
    }

    #[test]
    fn toque_cobre_toda_a_tela_em_qualquer_layout() {
        for layout in Layout::ALL {
            for integer in [false, true] {
                for (w, h) in [(800.0, 960.0), (1920.0, 1080.0), (333.0, 777.0)] {
                    let Some(b) = place(layout, w, h, integer).1 else { continue };
                    let c = |fx: f32, fy: f32| touch_point(Some(b), b.x + b.w * fx, b.y + b.h * fy);
                    assert_eq!(c(0.0, 0.0), Some((0, 0)), "{layout:?} {w}×{h}");
                    assert_eq!(c(0.9999, 0.9999), Some((319, 239)), "{layout:?} {w}×{h}");
                    let (x, y) = c(0.5, 0.5).unwrap();
                    assert!((159..=160).contains(&x) && (119..=120).contains(&y), "{layout:?}");
                    assert_eq!(c(1.0, 0.5), None);
                    assert_eq!(c(-0.01, 0.5), None);
                }
            }
        }
    }

    #[test]
    fn janela_minimizada() {
        assert_eq!(place(Layout::Empilhadas, 0.0, 0.0, false), (None, None));
    }
}
