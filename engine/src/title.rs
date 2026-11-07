//! Menu de título: layouts reais (`data/menu/title_{u,d}00_en.xa`) + a lógica de
//! `seq/menu/TitleMenu.xq` (desmontado com `tools/xq32_disasm.py`; ver docs/FORMATS.md, "XQ32").
//!
//! Literal do script: quais objetos/animações (`bg00`, `title_00`, `youkai_wa`, `main_bg`), quais botões
//! existem (New Game sempre; Continue se há save; Yo-kai Cam se algum save liberou a lente), o
//! deslocamento −38/+22 com 2+ botões, as caixas de toque (164×52, 182×40), a música `M01_MAIN_THEME`,
//! e todas as durações abaixo. O SIGNIFICADO de opcodes nativos é interpretação pelo uso (anotado em
//! cada constante). HIPÓTESE pura: `PUSH_FRAMES` e "qualquer entrada zera o contador ocioso".

use crate::assets::{arc0::Archive, Result};
use crate::audio::{self, bcstm};
use crate::input::{Button, Input};
use crate::layout::{Anim, Layout};
use crate::render2d::Target;
use crate::screen::{Screens, BOTTOM_H, BOTTOM_W, TOP_H, TOP_W};
use std::path::Path;
use std::sync::Arc;

/// `call #3262fc41(30)` antes de `Menu_TitleStart` (interpretado: fade de entrada).
const FADE_IN: u32 = 30;
/// `op5103('ttl_obj_logo', 1.0, 120)` (interpretado: alfa do logo até 1 em 120 quadros).
const LOGO_FADE: u32 = 120;
/// `op134($4002, 1800)`: quadros ociosos até repetir a abertura.
const IDLE_LIMIT: u32 = 1800;
/// `op14808/op14809(bgm, 120)` (interpretado: fade out da música).
const BGM_FADE_OUT: u32 = 120;
/// `call #d0fd3a01(30)` depois de `op5622('ttl_obj_yokai_wa', 'youkai_wa', 12)` em `EndAnimationUpScreen`:
/// a moldura de Yo-kai só ENTRA (fecha de 2000×1200 até 404×244) quando o jogador escolhe New Game/Continue;
/// no título parado ela fica fora de cena (flag 2 na criação, flag 6 no `Menu_TitleStartReset`).
const OUTRO_WAIT: u32 = 30;
/// `call #3bd4b06a(60)` (interpretado: fade out da tela).
const SCREEN_FADE_OUT: u32 = 60;
/// HIPÓTESE: o botão é nativo do motor; quanto tempo o estado `push` fica visível não está no script.
const PUSH_FRAMES: u32 = 8;
const BGM: &str = "snd/product/stream/M01_main_theme_e24.dspadpcm.bcstm";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// `Menu_TitleToNew` → cena 2111 com `new = 1`.
    NewGame,
    /// `Menu_TitleToContinue` → cena 2111 com `new = 0`.
    Continue,
    /// `Menu_TitleToLens` → Yo-kai Cam.
    YokaiCam,
    /// 30 s ociosos: o script toca `yw1_op` de novo e volta.
    Attract,
}

/// O que o script consulta do save (`op12052`, `op12054(slot, 0) & 1024`). Ainda não há saves no port.
#[derive(Clone, Copy, Default)]
pub struct SaveInfo {
    pub has_save: bool,
    pub lens_unlocked: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    FadeIn(u32),
    LogoIn(u32),
    Idle,
    Pressing(usize, u32),
    Outro(usize, u32),
    AttractOut(u32),
    Suspended,
}

struct MenuButton {
    event: Event,
    off: Anim,
    on: Anim,
    push: Anim,
    rect: (i32, i32, i32, i32),
}

pub struct Title {
    top: Layout,
    bottom: Layout,
    back: Anim,
    logo: Anim,
    wa: Anim,
    background: Anim,
    buttons: Vec<MenuButton>,
    selected: usize,
    touching: Option<usize>,
    phase: Phase,
    idle: u32,
    fade: f32,
    bgm_data: Option<Arc<bcstm::Stream>>,
    bgm: Option<audio::Player>,
}

impl Title {
    pub fn load(fa: &Archive, romfs: &Path, save: SaveInfo) -> Result<Title> {
        let top = Layout::load(&fa.read("data/menu/title_u00_en.xa")?)?;
        let bottom = Layout::load(&fa.read("data/menu/title_d00_en.xa")?)?;
        // SetupScreenA
        let back = Anim::new(&top, "bg00", true)?;
        let mut logo = Anim::new(&top, "title_00", true)?;
        logo.alpha = 0.0;
        // Moldura de Yo-kai: escondida até escolher New Game/Continue.
        let mut wa = Anim::new(&top, "youkai_wa", false)?;
        wa.alpha = 0.0;
        // SetupScreenB + Menu_TitleUpdateBtn
        let count = 1 + save.has_save as usize + save.lens_unlocked as usize;
        let mut specs = vec![(Event::NewGame, "btn_01", 164, 52)];
        if count > 1 {
            specs.push((Event::Continue, "btn_02", 164, 52));
        }
        if count > 2 {
            specs.push((Event::YokaiCam, "btn_03", 182, 40));
        }
        let mut buttons = Vec::new();
        for (i, (event, name, w, h)) in specs.into_iter().enumerate() {
            let shift = match (count > 1, i) {
                (true, 0) => -38.0,
                (true, 1) => 22.0,
                _ => 0.0,
            };
            let state = |s: &str| -> Result<Anim> {
                let mut a = Anim::new(&bottom, &format!("{name}_{s}"), true)?;
                a.offset = (0.0, shift);
                Ok(a)
            };
            let off = state("off")?;
            let p = bottom.instance_position(off.instance);
            let (x, y) = (p[0] as i32, p[1] as i32 + shift as i32);
            buttons.push(MenuButton {
                event,
                on: state("on")?,
                push: state("push")?,
                off,
                rect: (x, y, x + w, y + h),
            });
        }
        let bgm_data = match std::fs::read(romfs.join(BGM))
            .map_err(|e| e.to_string())
            .and_then(|b| bcstm::decode(&b).map_err(|e| e.to_string()))
        {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                eprintln!("[yw1] título sem música ({BGM}): {e}");
                None
            }
        };
        Ok(Title {
            background: Anim::new(&bottom, "main_bg", true)?,
            top,
            bottom,
            back,
            logo,
            wa,
            buttons,
            selected: 0,
            touching: None,
            phase: Phase::FadeIn(FADE_IN),
            idle: 0,
            fade: 1.0,
            bgm_data,
            bgm: None,
        })
    }

    fn start_bgm(&mut self) {
        self.bgm = self.bgm_data.clone().and_then(|s| {
            audio::Player::play(s)
                .map_err(|e| eprintln!("[yw1] música do título: {e}"))
                .ok()
        });
    }

    fn set_bgm_volume(&self, v: f32) {
        if let Some(p) = &self.bgm {
            p.set_volume(v);
        }
    }

    /// Volta da abertura repetida (modo demonstração): música do começo e fade de entrada.
    pub fn resume_after_movie(&mut self) {
        self.idle = 0;
        self.start_bgm();
        self.phase = Phase::FadeIn(FADE_IN);
    }

    /// Um quadro de 60 Hz (`input` só no primeiro tick de cada quadro da janela).
    pub fn tick(&mut self, input: Option<&Input>) -> Option<Event> {
        for a in [&mut self.back, &mut self.logo, &mut self.wa] {
            a.tick();
        }
        let any = input.is_some_and(Input::any_pressed);
        match self.phase {
            Phase::FadeIn(n) => {
                self.fade = n as f32 / FADE_IN as f32;
                self.phase = match n {
                    0 if self.logo.alpha < 1.0 => {
                        self.start_bgm();
                        Phase::LogoIn(0)
                    }
                    0 => Phase::Idle,
                    _ => Phase::FadeIn(n - 1),
                };
            }
            Phase::LogoIn(k) => {
                let k = k + 1;
                self.logo.alpha = k as f32 / LOGO_FADE as f32;
                self.phase = Phase::LogoIn(k);
                if any || k >= LOGO_FADE {
                    self.logo.alpha = 1.0;
                    self.idle = 0;
                    self.phase = Phase::Idle;
                }
            }
            Phase::Idle => {
                self.idle += 1;
                if any || input.is_some_and(|i| i.touch().is_some()) {
                    self.idle = 0;
                }
                if let Some(input) = input {
                    self.handle_input(input);
                }
                if self.idle > IDLE_LIMIT {
                    self.phase = Phase::AttractOut(0);
                }
            }
            Phase::Pressing(b, n) => {
                self.phase = if n <= 1 {
                    Phase::Outro(b, 0)
                } else {
                    Phase::Pressing(b, n - 1)
                };
            }
            Phase::Outro(b, t) => return self.outro(b, t),
            Phase::AttractOut(t) => {
                self.set_bgm_volume(1.0 - t as f32 / BGM_FADE_OUT as f32);
                if t >= BGM_FADE_OUT {
                    self.bgm = None;
                    self.phase = Phase::Suspended;
                    return Some(Event::Attract);
                }
                self.phase = Phase::AttractOut(t + 1);
            }
            Phase::Suspended => {}
        }
        None
    }

    /// `Menu_TitleToNewMain`/`Menu_TitleToLensMain`: moldura recolhe, música e tela somem, cena seguinte;
    /// ao voltar dela (`RestructScreenB` + `Menu_TitleStart`) o título recomeça com o logo em fade.
    fn outro(&mut self, b: usize, t: u32) -> Option<Event> {
        if t == 0 {
            // EndAnimationUpScreen: os Yo-kai entram em volta da tela.
            self.wa.restart(false, false);
            self.wa.alpha = 1.0;
        }
        if t >= OUTRO_WAIT {
            let k = t - OUTRO_WAIT;
            self.fade = (k as f32 / SCREEN_FADE_OUT as f32).min(1.0);
            self.set_bgm_volume(1.0 - k as f32 / BGM_FADE_OUT as f32);
            if k == SCREEN_FADE_OUT {
                // Menu_TitleStartReset
                self.logo.alpha = 0.0;
                self.wa.alpha = 0.0;
            }
            if k >= BGM_FADE_OUT {
                self.bgm = None;
                self.phase = Phase::FadeIn(FADE_IN);
                return Some(self.buttons[b].event);
            }
        }
        self.phase = Phase::Outro(b, t + 1);
        None
    }

    fn handle_input(&mut self, input: &Input) {
        let n = self.buttons.len();
        if input.pressed(Button::Down) || input.pressed(Button::Right) {
            self.selected = (self.selected + 1) % n;
        }
        if input.pressed(Button::Up) || input.pressed(Button::Left) {
            self.selected = (self.selected + n - 1) % n;
        }
        if input.pressed(Button::A) || input.pressed(Button::Start) {
            self.phase = Phase::Pressing(self.selected, PUSH_FRAMES);
        }
        let hit = |p: (i32, i32)| {
            self.buttons.iter().position(|b| {
                (b.rect.0..b.rect.2).contains(&p.0) && (b.rect.1..b.rect.3).contains(&p.1)
            })
        };
        if let Some(p) = input.touch() {
            self.touching = hit(p);
            if let Some(b) = self.touching {
                self.selected = b;
            }
        } else if let Some(p) = input.touch_released() {
            if let (Some(b), Some(start)) = (hit(p), self.touching) {
                if b == start {
                    self.phase = Phase::Pressing(b, PUSH_FRAMES);
                }
            }
            self.touching = None;
        }
    }

    pub fn draw(&self, screens: &mut Screens) {
        screens.clear();
        let mut top = Target {
            pixels: &mut screens.top,
            width: TOP_W,
            height: TOP_H,
        };
        let mut anims = vec![&self.back, &self.logo, &self.wa];
        anims.sort_by_key(|a| a.instance);
        self.top.draw(&mut top, &anims);

        let mut bottom = Target {
            pixels: &mut screens.bottom,
            width: BOTTOM_W,
            height: BOTTOM_H,
        };
        let pushed = match self.phase {
            Phase::Pressing(b, _) | Phase::Outro(b, _) => Some(b),
            _ => self.touching,
        };
        let mut anims = vec![&self.background];
        for (i, b) in self.buttons.iter().enumerate() {
            anims.push(if pushed == Some(i) {
                &b.push
            } else if i == self.selected {
                &b.on
            } else {
                &b.off
            });
        }
        anims.sort_by_key(|a| a.instance);
        self.bottom.draw(&mut bottom, &anims);

        if self.fade > 0.0 {
            let k = ((1.0 - self.fade).clamp(0.0, 1.0) * 256.0) as u32;
            for p in screens.top.iter_mut().chain(screens.bottom.iter_mut()) {
                let c = |s: u32| ((((*p >> s) & 0xff) * k) >> 8) << s;
                *p = c(16) | c(8) | c(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{WIN_H, WIN_W};

    fn title(save: SaveInfo) -> Option<Title> {
        let romfs = crate::romfs::locate(None)?;
        let fa = Archive::open(&romfs.join("yw1_a.fa")).ok()?;
        Some(Title::load(&fa, &romfs, save).unwrap())
    }

    /// Botões conforme `Menu_TitleUpdateBtn`: só New Game sem save; com save e lente, os três,
    /// com New Game 38 px acima e Continue 22 px abaixo da posição do layout (78,94).
    #[test]
    fn buttons_follow_script() {
        let Some(t) = title(SaveInfo::default()) else {
            return;
        };
        assert_eq!(t.buttons.len(), 1);
        assert_eq!(t.buttons[0].rect, (78, 94, 242, 146));
        let t = title(SaveInfo {
            has_save: true,
            lens_unlocked: true,
        })
        .unwrap();
        let ev: Vec<Event> = t.buttons.iter().map(|b| b.event).collect();
        assert_eq!(ev, [Event::NewGame, Event::Continue, Event::YokaiCam]);
        assert_eq!(t.buttons[0].rect.1, 56);
        assert_eq!(t.buttons[1].rect.1, 116);
    }

    /// Sem entrada: fade (30) + logo (120) + ocioso até passar de 1800 → música some (120) → `Attract`.
    #[test]
    fn idle_triggers_attract_after_script_timings() {
        let Some(mut t) = title(SaveInfo::default()) else {
            return;
        };
        let mut frames = 0;
        let event = loop {
            frames += 1;
            if let Some(e) = t.tick(None) {
                break e;
            }
            assert!(frames < 5000);
        };
        assert_eq!(event, Event::Attract);
        assert_eq!(
            frames,
            (FADE_IN + 1) + LOGO_FADE + (IDLE_LIMIT + 1) + (BGM_FADE_OUT + 1)
        );
    }

    /// Renderiza o título pelo mesmo caminho do runtime (sem janela) e grava RGBA cru em work/title/.
    /// Manual: `cargo test --release -- --ignored render_title_snapshots`.
    #[test]
    #[ignore]
    fn render_title_snapshots() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/title");
        let mut screens = Screens::new();
        let mut fb = vec![0u32; WIN_W * WIN_H];
        for (label, save, frames) in [
            ("nosave_f020", SaveInfo::default(), 20),
            ("nosave_f090", SaveInfo::default(), 90),
            ("nosave_f300", SaveInfo::default(), 300),
            (
                "save_f300",
                SaveInfo {
                    has_save: true,
                    lens_unlocked: true,
                },
                300,
            ),
        ] {
            let Some(mut t) = title(save) else { return };
            t.bgm_data = None;
            for _ in 0..frames {
                t.tick(None);
            }
            t.draw(&mut screens);
            screens.compose(&mut fb);
            let raw: Vec<u8> = fb
                .iter()
                .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                .collect();
            std::fs::write(dir.join(format!("rust_title_{label}.rgba")), raw).unwrap();
        }
        // New Game escolhido: a moldura de Yo-kai entra (12 quadros) durante a saída.
        let Some(mut t) = title(SaveInfo::default()) else {
            return;
        };
        t.bgm_data = None;
        for _ in 0..300 {
            t.tick(None);
        }
        t.phase = Phase::Outro(0, 0);
        for _ in 0..14 {
            t.tick(None);
        }
        t.draw(&mut screens);
        screens.compose(&mut fb);
        let raw: Vec<u8> = fb
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
            .collect();
        std::fs::write(dir.join("rust_title_newgame_outro.rgba"), raw).unwrap();
    }
}
