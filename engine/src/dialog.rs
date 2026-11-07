//! Diálogos de evento: textos reais de `data/txt/ev/<evento>_{m|f}_en.cfg.bin` (`TEXT_INFO [crc32(id), página,
//! texto, flag]`, `m` = menino, `f` = menina) desenhados com a fonte do jogo e as peças de janela de
//! `data/menu/win_en.xa` (`mes00` balão, `talk_send` seta de "próximo", `talk_d_bg` fundo da tela de baixo).
//!
//! Dois modos, como nos scripts: `StartMonologue` (tela preta, texto branco centralizado — `ev01_0003`) e fala
//! em balão (`OpenTextCaptionDetail2` — `ev01_0020`). Velocidade do texto: 2 caracteres/quadro (HIPÓTESE; a
//! opção "velocidade do texto" do jogo não foi lida). `<PAGE>` quebra página; A avança/completa.

use crate::assets::arc0::Archive;
use crate::assets::cfgbin::{self, Param};
use crate::assets::font::{clean, Font};
use crate::input::{Button, Input};
use crate::layout::{Anim, Layout};
use crate::render2d::Target;
use crate::screen::{Screens, BOTTOM_H, BOTTOM_W, TOP_H, TOP_W};

const CHARS_PER_FRAME: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Monologue,
    Balloon,
}

/// Nome do jogador usado em `<PNAME>`/`<PNAME1>` (definido ao entrar no campo).
pub fn set_player_name(name: &str) {
    PLAYER_NAME.with(|n| *n.borrow_mut() = name.to_string());
}

thread_local! {
    static PLAYER_NAME: std::cell::RefCell<String> = std::cell::RefCell::new("Nate".into());
}

fn substitute(text: &str) -> String {
    PLAYER_NAME.with(|n| {
        let n = n.borrow();
        text.replace("<PNAME1>", &n).replace("<PNAME>", &n)
    })
}

pub struct Dialog {
    pages: Vec<String>,
    page: usize,
    shown: usize,
    mode: Mode,
    font: Font,
    win: Option<Layout>,
    balloon: Option<Anim>,
    arrow: Option<Anim>,
    bottom_bg: Option<Anim>,
    pub done: bool,
}

/// Textos de um evento, na ordem do arquivo (cada `TEXT_INFO` pode ter várias páginas `<PAGE>`).
pub fn event_lines(fa: &Archive, event: &str, player_type: i32) -> Vec<String> {
    let g = if player_type == 2 { "f" } else { "m" };
    let Ok(bytes) = fa.read(&format!("data/txt/ev/{event}_{g}_en.cfg.bin")) else {
        return Vec::new();
    };
    cfgbin::parse(&bytes)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.name == "TEXT_INFO")
        .filter_map(|e| match e.params.get(2) {
            Some(Param::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

impl Dialog {
    pub fn new(fa: &Archive, lines: &[String], mode: Mode) -> crate::assets::Result<Dialog> {
        let font = Font::parse(&fa.read("fnt/ft_nrm.xf")?)?;
        let win = fa
            .read("data/menu/win_en.xa")
            .ok()
            .and_then(|b| Layout::load(&b).ok());
        let anim = |name: &str| win.as_ref().and_then(|l| Anim::new(l, name, true).ok());
        let pages = lines
            .iter()
            .map(|l| substitute(l))
            .flat_map(|l| {
                l.split("<PAGE>")
                    .map(|p| clean(p).trim_matches('\n').to_string())
                    .collect::<Vec<_>>()
            })
            .filter(|p| !p.is_empty())
            .collect();
        Ok(Dialog {
            pages,
            page: 0,
            shown: 0,
            mode,
            font,
            balloon: anim("mes00"),
            // [H] a instância `talk_send` não tem posição no layout; o motor a põe no canto do balão.
            arrow: anim("talk_send").map(|mut a| {
                a.offset = (356.0, 214.0);
                a
            }),
            bottom_bg: anim("talk_d_bg"),
            win,
            done: false,
        })
    }

    pub fn tick(&mut self, input: Option<&Input>) {
        if self.done {
            return;
        }
        for a in [&mut self.balloon, &mut self.arrow, &mut self.bottom_bg]
            .into_iter()
            .flatten()
        {
            a.tick();
        }
        let len = self.pages.get(self.page).map_or(0, |p| p.chars().count());
        self.shown = (self.shown + CHARS_PER_FRAME).min(len);
        let advance = input.is_some_and(|i| {
            i.pressed(Button::A) || i.pressed(Button::B) || i.touch_released().is_some()
        });
        if advance {
            if self.shown < len {
                self.shown = len;
            } else {
                self.page += 1;
                self.shown = 0;
                if self.page >= self.pages.len() {
                    self.done = true;
                }
            }
        }
    }

    /// Desenha por cima da cena (a tela de cima já contém o 3D, ou fica preta no monólogo).
    pub fn draw(&self, top: &mut [u32], top_w: usize, top_h: usize, screens: &mut Screens) {
        let Some(text) = self.pages.get(self.page) else {
            return;
        };
        let visible: String = text.chars().take(self.shown).collect();
        let complete = self.shown >= text.chars().count();
        let scale_x = top_w as f32 / TOP_W as f32;
        match self.mode {
            Mode::Monologue => {
                top.fill(0);
                let tw = text.lines().map(|l| self.font.width(l)).max().unwrap_or(0) as f32;
                let th = text.lines().count() as f32 * self.font.line_height as f32;
                let s = scale_x;
                let pos = ((top_w as f32 - tw * s) / 2.0, (top_h as f32 - th * s) / 2.0);
                self.font
                    .draw(top, top_w, top_h, pos, &visible, 0xff_ffff, s, 1.0);
            }
            Mode::Balloon => {
                // Balão e seta são 2D na resolução do 3DS: desenha num buffer 400×240 e amplia.
                let mut ui = vec![0u32; TOP_W * TOP_H];
                let mut mask = vec![false; TOP_W * TOP_H];
                if let Some(l) = &self.win {
                    let mut t = Target {
                        pixels: &mut ui,
                        width: TOP_W,
                        height: TOP_H,
                    };
                    let mut anims: Vec<&Anim> = self.balloon.iter().collect();
                    if complete {
                        anims.extend(self.arrow.iter());
                    }
                    // fundo transparente: marca com uma cor sentinela
                    t.pixels.fill(0x00ff_00ff);
                    l.draw(&mut t, &anims);
                    for (m, p) in mask.iter_mut().zip(&ui) {
                        *m = *p != 0x00ff_00ff;
                    }
                }
                // [H] texto dentro do balão `mes00` (posição da instância 40,180), cor escura.
                let mut text_layer = ui.clone();
                self.font.draw(
                    &mut text_layer,
                    TOP_W,
                    TOP_H,
                    (52.0, 182.0),
                    &visible,
                    0x30_2820,
                    1.0,
                    1.0,
                );
                for (i, (p, m)) in text_layer.iter().zip(&mask).enumerate() {
                    if *m || *p != ui[i] {
                        mask_blit(top, top_w, top_h, i % TOP_W, i / TOP_W, *p);
                    }
                }
                if let (Some(l), Some(bg)) = (&self.win, &self.bottom_bg) {
                    let mut t = Target {
                        pixels: &mut screens.bottom,
                        width: BOTTOM_W,
                        height: BOTTOM_H,
                    };
                    l.draw(&mut t, &[bg]);
                }
            }
        }
    }
}

/// Pinta o pixel (x, y) da camada 400×240 no quadro de cima ampliado.
fn mask_blit(top: &mut [u32], w: usize, h: usize, x: usize, y: usize, rgb: u32) {
    let (sx, sy) = (w / TOP_W, h / TOP_H);
    for dy in 0..sy.max(1) {
        for dx in 0..sx.max(1) {
            let (px, py) = (x * sx + dx, y * sy + dy);
            if px < w && py < h {
                top[py * w + px] = rgb;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prologue_texts_are_read() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        let mono = event_lines(&fa, "ev01_0003", 1);
        assert_eq!(mono.len(), 2);
        assert!(mono[0].starts_with("This story is a work of fiction."));
        let room = event_lines(&fa, "ev01_0020", 1);
        assert_eq!(room.len(), 4);
        let d = Dialog::new(&fa, &room, Mode::Balloon).unwrap();
        // a última fala tem <PAGE>: 5 páginas no total
        assert_eq!(d.pages.len(), 5);
    }
}
