//! Prólogo do capítulo 1 no campo, na ordem de `seq/phs/c01_trigger_0.01.xq` (RunCmd_Phase1/2 — ver
//! work/notes/xq32_start_sequence.md):
//!   1. `ev01_0003`  monólogo em tela preta (aviso de ficção + relógio do sistema)            → feito (texto real)
//!   2. `ev01_0005a/b` 1ª cutscene 3D (473 quadros, câmera `.xv`, Yo-kai)                        → AINDA NÃO (pulada, com log)
//!   3. `ev01_0010a/b` rua `t101g00`, 19 falas                                                    → AINDA NÃO (mapa `.xr` não lido)
//!   4. `ev01_0020a/b` entra correndo no quarto, 4 falas, jogador assume em (-10,0,15.76) ân. 90 → feito
//!
//! Os passos do 0020 vêm do script (corre de (11.58,0,15.76) até (-17.43,0,15.76), freia, fala) e dos textos
//! `data/txt/ev/ev01_0020_{m,f}_en.cfg.bin`; a música do evento é `ev01_0020_01` (PARAM do
//! `data/snd/cfg/event/ev01_0020.cfg.bin`). Interpretado aqui em Rust, não pela VM (os opcodes 3D de evento
//! do `op2110` ainda não têm host).

use crate::assets::arc0::Archive;
use crate::dialog::{self, Dialog, Mode};
use crate::input::Input;
use crate::room::Room;
use crate::screen::Screens;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Monologue,
    Cutscene,
    Street,
    /// `ev01_0020a/b` pelo script real: entra correndo no quarto, pega a rede de insetos.
    Bedroom,
    Done,
}

pub struct Prologue {
    step: Step,
    monologue: Option<Dialog>,
    bedroom: Option<crate::event_host::EventScene>,
    /// 1ª cutscene (`ev01_0005a/b`), rodada pelo script real no host de eventos.
    cutscene: Option<crate::event_host::EventScene>,
    /// Quadros de 60 Hz desde o começo da cutscene (Start só pula depois de 2 s).
    cut_ticks: u32,
    street: Option<crate::event_host::EventScene>,
    fa: std::rc::Rc<Archive>,
    player_type: i32,
}

/// Pedido de música para o `main` (nome de stream em `snd/product/stream/`).
pub enum Music {
    Play(&'static str),
}

impl Prologue {
    pub fn new(fa_rc: std::rc::Rc<Archive>, player_type: i32) -> Prologue {
        let fa: &Archive = &fa_rc;
        let mono = dialog::event_lines(fa, "ev01_0003", player_type);
        eprintln!(
            "[yw1] prólogo: ev01_0003 ({} textos) → cutscene ev01_0005 → rua ev01_0010 → quarto ev01_0020",
            mono.len()
        );
        Prologue {
            step: Step::Monologue,
            monologue: Dialog::new(fa, &mono, Mode::Monologue).ok(),
            bedroom: None,
            street: None,
            fa: fa_rc.clone(),
            cutscene: None,
            cut_ticks: 0,
            player_type,
        }
    }

    pub fn finished(&self) -> bool {
        self.step == Step::Done
    }

    /// Um quadro. `input` só no primeiro tick do quadro da janela.
    pub fn tick(&mut self, room: &mut Room, input: Option<&Input>) -> Option<Music> {
        let mut music = None;
        match self.step {
            Step::Monologue => {
                room.idle();
                match self.monologue.as_mut() {
                    Some(d) if !d.done => d.tick(input),
                    _ => {
                        self.step = Step::Cutscene;
                        // RunEventEachVersion('ev01_0005'): a = menino, b = menina; cenário = a rua (t101g00)
                        let ev = if self.player_type == 2 {
                            "ev01_0005b"
                        } else {
                            "ev01_0005a"
                        };
                        let stage = Room::load(&self.fa, "t101g00").ok();
                        self.cutscene = crate::event_host::EventScene::open(
                            self.fa.clone(),
                            ev,
                            stage,
                            self.player_type,
                        )
                        .map_err(|e| eprintln!("[yw1] {ev} indisponível: {e}"))
                        .ok();
                        self.cut_ticks = 0;
                        if self.cutscene.is_some() {
                            music = Some(Music::Play("ev01_0005"));
                        }
                    }
                }
            }
            Step::Cutscene => {
                room.idle();
                // Start só pula depois de 2 s (60 quadros da cutscene): um Enter/Start que vinha de confirmar o
                // nome ou avançar o texto não pode engolir a cena de abertura.
                self.cut_ticks += 1;
                let can_skip = self.cut_ticks >= 120;
                let skip =
                    can_skip && input.is_some_and(|i| i.pressed(crate::input::Button::Start));
                let done = match self.cutscene.as_mut() {
                    Some(ev) => {
                        if let Err(e) = ev.tick(input) {
                            eprintln!("[yw1] erro na cutscene: {e}");
                            true
                        } else {
                            ev.finished
                        }
                    }
                    None => true,
                };
                if done || skip {
                    self.cutscene = None;
                    // map_warp('t101g00', 490, 0, -348) + RunEventEachVersion('ev01_0010'): a = menino, b = menina
                    let ev = if self.player_type == 2 {
                        "ev01_0010b"
                    } else {
                        "ev01_0010a"
                    };
                    let stage = Room::load(&self.fa, "t101g00").ok();
                    self.street = crate::event_host::EventScene::open(
                        self.fa.clone(),
                        ev,
                        stage,
                        self.player_type,
                    )
                    .map_err(|e| eprintln!("[yw1] {ev} indisponível: {e}"))
                    .ok();
                    self.step = Step::Street;
                    music = Some(Music::Play("ev01_0010_01"));
                }
            }
            Step::Street => {
                room.idle();
                let finished = match self.street.as_mut() {
                    Some(ev) => {
                        if let Err(e) = ev.tick(input) {
                            eprintln!("[yw1] erro no evento da rua: {e}");
                            true
                        } else {
                            ev.finished
                        }
                    }
                    None => true,
                };
                if finished {
                    self.street = None;
                    // map_warp('t101i02' ou 't101i04') + RunEventEachVersion('ev01_0020'): a = menino, b = menina
                    let (ev, map) = if self.player_type == 2 {
                        ("ev01_0020b", "t101i04")
                    } else {
                        ("ev01_0020a", "t101i02")
                    };
                    let stage = Room::load(&self.fa, map).ok();
                    self.bedroom = crate::event_host::EventScene::open(
                        self.fa.clone(),
                        ev,
                        stage,
                        self.player_type,
                    )
                    .map_err(|e| eprintln!("[yw1] {ev} indisponível: {e}"))
                    .ok();
                    self.step = Step::Bedroom;
                    music = Some(Music::Play("ev01_0020_01"));
                }
            }
            Step::Bedroom => {
                room.idle();
                let finished = match self.bedroom.as_mut() {
                    Some(ev) => {
                        if let Err(e) = ev.tick(input) {
                            eprintln!("[yw1] erro no evento do quarto: {e}");
                            true
                        } else {
                            ev.finished
                        }
                    }
                    None => true,
                };
                if finished {
                    self.bedroom = None;
                    // SetPosLeaderChara do Phase1 (menina: 2.5, 0, 17.36)
                    let pos = if self.player_type == 2 {
                        [2.5, 0.0, 17.36]
                    } else {
                        [-10.0, 0.0, 15.76]
                    };
                    room.place(pos, 90.0);
                    self.step = Step::Done;
                    music = Some(Music::Play("M38_keta_house"));
                }
            }
            Step::Done => {}
        }
        music
    }

    /// Durante a cutscene, o quadro de cima vem dela (e não do mapa atual).
    pub fn scene_frame(
        &mut self,
        scale: usize,
        screens: &mut Screens,
    ) -> Option<&crate::gfx3d::Frame> {
        match self.step {
            Step::Cutscene => self.cutscene.as_mut().map(|e| e.draw(scale, screens)),
            Step::Street => self.street.as_mut().map(|e| e.draw(scale, screens)),
            Step::Bedroom => self.bedroom.as_mut().map(|e| e.draw(scale, screens)),
            _ => None,
        }
    }

    pub fn draw(&self, top: &mut [u32], w: usize, h: usize, screens: &mut Screens) {
        let d = match self.step {
            Step::Monologue => self.monologue.as_ref(),
            _ => None,
        };
        if let Some(d) = d {
            d.draw(top, w, h, screens);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Button;

    /// Prólogo inteiro sem janela, apertando A a cada 40 quadros; grava quadros em work/prologue/.
    #[test]
    fn prologue_runs_to_player_control() {
        let Some(romfs) = crate::romfs::locate(None) else {
            return;
        };
        let Ok(fa) = Archive::open(&romfs.join("yw1_a.fa")) else {
            return;
        };
        let mut room = Room::load(&fa, "t101i02").unwrap();
        room.model = crate::room::PlayerModel::load(&fa, 1).ok();
        let fa = std::rc::Rc::new(fa);
        let mut p = Prologue::new(fa.clone(), 1);
        let mut input = Input::headless();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/prologue");
        let _ = std::fs::create_dir_all(&dir);
        let mut shots = 0;
        let mut last = None;
        for f in 0..40000 {
            input.set(Button::A, f % 40 == 39);
            p.tick(&mut room, Some(&input));
            if last != Some(p.step) {
                println!("tick {f}: {:?}", p.step);
                last = Some(p.step);
            }
            if f % 120 == 30 && shots < 8 {
                let mut screens = Screens::new();
                let frame = room.draw(2);
                let (w, h) = (frame.width, frame.height);
                let mut top = frame.color.clone();
                p.draw(&mut top, w, h, &mut screens);
                let raw: Vec<u8> = top
                    .iter()
                    .flat_map(|c| [(c >> 16) as u8, (c >> 8) as u8, *c as u8, 255])
                    .collect();
                let _ = std::fs::write(dir.join(format!("f{f:04}_{w}x{h}.rgba")), raw);
                shots += 1;
            }
            if p.finished() {
                break;
            }
        }
        assert!(p.finished(), "prólogo não terminou");
        assert_eq!(room.player.pos, [-10.0, 0.0, 15.76]);
    }
}
