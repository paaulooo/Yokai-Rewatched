//! Capítulo 1 depois da escada até o primeiro Yo-kai, e o jogo livre com busca de Yo-kai.
//!
//! Ordem das fases de `seq/phs/c01_trigger_0.01.xq` (`RunCmd_PhaseN`):
//! 4/5 ev01_0030 (mãe) · 6 ev98_0050 + ev01_0040 · 7 ev01_0050 · 8–10 ev01_0060 · 11 ev01_0070 · 12 ev01_0080 ·
//! 13 ev01_0090 · 14 ev01_0095 · 18 ev01_0100 · 19/20 ev01_0110 (e102g001) + ev01_0120 (t102g00 640,0,-42) ·
//! 22 ev01_0130 · 23 AddParty Cadin (`para_y311`) + ev01_0140 + batalha `ev01_btl0010` em `t100b02` ·
//! 26 sai o Cadin + ev51_0010 (t102g00 693,-20,-96) · 27 AddParty Buhu (`#8443d22d`) + ev01_0150/0151 +
//! ev01_0160 (e102g001) + SaveRun → jogo livre.
//!
//! [H] No jogo, cada fase dispara por gatilhos de mapa (`<mapa>_trigger.cfg.bin`, condições em bytecode base64
//! ainda não decifrado) e conversa com NPCs. Aqui as fases andam em sequência: as que acontecem em outro mapa
//! esperam o jogador chegar lá pelas saídas reais (casa → rua → monte); as que o próprio script de fase
//! teleporta (`map_warp`) são teleportadas igual.

use crate::assets::arc0::Archive;
use crate::battle::{Battle, Outcome};
use crate::dialog::{Dialog, Mode};
use crate::event_host::EventScene;
use crate::gfx3d::{Frame, V3};
use crate::input::{Button, Input};
use crate::room::Room;
use crate::save::Save;
use crate::screen::Screens;
use crate::yokai::{hash, Db, Member, Watchmap};
use std::collections::HashMap;
use std::rc::Rc;

/// Eventos na ordem (versão do menino; usado pelo teste que roda todos).
#[cfg(test)]
pub const EVENTS_BOY: &[&str] = &[
    "ev01_0030",
    "ev98_0050",
    "ev01_0040",
    "ev01_0050",
    "ev01_0060",
    "ev01_0070",
    "ev01_0080",
    "ev01_0090",
    "ev01_0095",
    "ev01_0100",
    "ev01_0110a",
    "ev01_0120a",
    "ev01_0130a",
    "ev01_0140",
    "ev51_0010",
    "ev01_0150",
    "ev01_0151",
    "ev01_0160a",
];

#[derive(Clone, Copy, Debug)]
pub enum Step {
    /// Espera o jogador chegar ao mapa (com uma dica na tela de baixo).
    Arrive(&'static str, &'static str),
    /// Evento pelo script real (menino, menina).
    Event(&'static str, &'static str),
    Warp(&'static str, V3, f32),
    Join(u32, u32),
    Leave(u32),
    Battle(&'static str, &'static str),
    /// Janela de tutorial (`OpenInfoWin(0, id, 0, 'tr0004')` dos scripts de fase), texto em `system_text_en`.
    Info(&'static str),
    Save,
}

const CADIN: u32 = 0xaf6e_81ee; // crc32("para_y311")
const BUHU: u32 = 0x8443_d22d;

pub const STEPS: &[Step] = &[
    Step::Event("ev01_0030", "ev01_0030"),
    Step::Arrive("t101g00", "Go outside and head for the park!"),
    Step::Event("ev98_0050", "ev98_0050"),
    Step::Event("ev01_0040", "ev01_0040"),
    Step::Event("ev01_0050", "ev01_0050"),
    Step::Info("sys_info_win_11"),
    Step::Event("ev01_0060", "ev01_0060"),
    Step::Arrive("t102g00", "Head to Mt. Wildwood to catch bugs!"),
    Step::Event("ev01_0070", "ev01_0070"),
    Step::Event("ev01_0080", "ev01_0080"),
    Step::Event("ev01_0090", "ev01_0090"),
    Step::Event("ev01_0095", "ev01_0095"),
    Step::Event("ev01_0100", "ev01_0100"),
    Step::Warp("e102g001", [0.0, 0.0, 0.0], 0.0),
    Step::Event("ev01_0110a", "ev01_0110b"),
    Step::Warp("t102g00", [640.0, 0.0, -42.0], 0.0),
    Step::Event("ev01_0120a", "ev01_0120b"),
    Step::Event("ev01_0130a", "ev01_0130b"),
    Step::Join(CADIN, 1),
    Step::Event("ev01_0140", "ev01_0140"),
    Step::Battle("ev01_btl0010", "t100b02"),
    Step::Leave(CADIN),
    Step::Warp("t102g00", [693.0, -20.0, -96.0], -90.0),
    Step::Event("ev51_0010", "ev51_0010"),
    Step::Join(BUHU, 1),
    Step::Event("ev01_0150", "ev01_0150"),
    Step::Event("ev01_0151", "ev01_0151"),
    Step::Warp("e102g001", [0.0, 0.0, 0.0], 0.0),
    Step::Event("ev01_0160a", "ev01_0160b"),
    Step::Info("sys_info_win_13"),
    Step::Info("sys_info_win_20"),
    Step::Save,
    Step::Warp("t102g00", [693.0, -20.0, -96.0], -90.0),
];

#[cfg_attr(not(test), allow(dead_code))]
pub const DONE: u32 = STEPS.len() as u32;

/// Pedidos para o laço principal.
#[derive(Debug, PartialEq)]
pub enum Out {
    Warp(String, V3, f32),
    Save,
}

/// [H] busca com a lente: segurar Y parado por `LENS_TICKS` quadros; chance de achar um Yo-kai.
const LENS_TICKS: u32 = 75;
const FIND_CHANCE: f32 = 0.6;

pub struct Story {
    fa: Rc<Archive>,
    pub db: Rc<Db>,
    watch: HashMap<String, Watchmap>,
    scene: Option<EventScene>,
    battle: Option<Battle>,
    dialog: Option<Dialog>,
    lens: u32,
    rng: u64,
    player_type: i32,
    font: Option<crate::assets::font::Font>,
    sys_texts: HashMap<u32, String>,
}

impl Story {
    pub fn new(fa: Rc<Archive>, player_type: i32) -> crate::assets::Result<Story> {
        let mut db = Db::load(&fa)?;
        let mut watch = HashMap::new();
        for m in ["t101g00", "t102g00"] {
            if let Some(w) = db.load_map(&fa, m) {
                watch.insert(m.to_string(), w);
            }
        }
        let rng = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(12345, |d| d.as_nanos() as u64)
            | 1;
        let font = fa_font(&fa);
        let fa_for_texts = fa.clone();
        Ok(Story {
            fa,
            db: Rc::new(db),
            watch,
            scene: None,
            battle: None,
            dialog: None,
            lens: 0,
            rng,
            player_type,
            font,
            sys_texts: crate::event_host::system_texts(&fa_for_texts),
        })
    }

    fn roll(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Há algo em primeiro plano (evento, batalha ou mensagem): o campo não anda.
    pub fn busy(&self) -> bool {
        self.scene.is_some() || self.battle.is_some() || self.dialog.is_some()
    }

    pub fn in_battle(&self) -> bool {
        self.battle.is_some()
    }

    fn say(&mut self, lines: &[String]) {
        self.dialog = Dialog::new(&self.fa, lines, Mode::Balloon).ok();
    }

    fn party(&self, save: &Save) -> Vec<Member> {
        save.party
            .iter()
            .map(|&(p, l, e)| {
                let mut m = Member::new(&self.db, p, l);
                m.exp = e;
                m
            })
            .collect()
    }

    /// Dica da tela de baixo (objetivo atual).
    pub fn hint(&self, save: &Save) -> Option<String> {
        if self.busy() {
            return None;
        }
        match STEPS.get(save.chapter as usize) {
            Some(Step::Arrive(_, h)) => Some(h.to_string()),
            None if self.watch.contains_key("t102g00") => Some(if self.lens > 0 {
                "Searching with the Yo-kai Lens...".to_string()
            } else {
                "Hold Y (still) to search for Yo-kai with the Lens.".to_string()
            }),
            _ => None,
        }
    }

    pub fn tick(&mut self, room: &mut Room, input: Option<&Input>, save: &mut Save) -> Vec<Out> {
        let mut out = Vec::new();
        if let Some(d) = self.dialog.as_mut() {
            d.tick(input);
            if d.done {
                self.dialog = None;
            }
            room.idle();
            return out;
        }
        if let Some(b) = self.battle.as_mut() {
            b.tick(input);
            if let Some(o) = b.outcome {
                let party = std::mem::take(&mut b.result_party);
                self.battle = None;
                self.finish_battle(o, party, save);
            }
            return out;
        }
        if let Some(ev) = self.scene.as_mut() {
            room.idle();
            let done = match ev.tick(input) {
                Ok(()) => ev.finished,
                Err(e) => {
                    eprintln!("[yw1] erro no evento: {e}");
                    true
                }
            };
            if done {
                self.scene = None;
                save.chapter += 1;
            }
            return out;
        }
        let Some(step) = STEPS.get(save.chapter as usize).copied() else {
            self.free_play(room, input, save);
            return out;
        };
        match step {
            Step::Arrive(map, _) => {
                if room.id == map {
                    save.chapter += 1;
                }
            }
            Step::Event(boy, girl) => {
                let ev = if self.player_type == 2 { girl } else { boy };
                let stage = Room::load(&self.fa, &room.id).ok();
                match EventScene::open(self.fa.clone(), ev, stage, self.player_type) {
                    Ok(s) => self.scene = Some(s),
                    Err(e) => {
                        eprintln!("[yw1] {ev} indisponível: {e}");
                        save.chapter += 1;
                    }
                }
            }
            Step::Warp(map, pos, yaw) => {
                save.chapter += 1;
                out.push(Out::Warp(map.to_string(), pos, yaw));
            }
            Step::Join(p, lvl) => {
                let p = p as i32;
                if !save.party.iter().any(|m| m.0 == p) && save.party.len() < 6 {
                    save.party.push((p, lvl, 0));
                }
                save.chapter += 1;
                let name = self
                    .db
                    .species
                    .get(&p)
                    .map_or("A Yo-kai".into(), |s| s.name.clone());
                self.say(&[format!("{name} joined your team!")]);
            }
            Step::Leave(p) => {
                save.party.retain(|m| m.0 != p as i32);
                save.chapter += 1;
            }
            Step::Battle(name, stage) => {
                let party = self.party(save);
                let enemies = self.db.encounter(name);
                self.battle = Some(Battle::new(
                    &self.fa,
                    self.db.clone(),
                    &party,
                    &enemies,
                    stage,
                    false,
                ));
            }
            Step::Info(id) => {
                save.chapter += 1;
                let key = crate::assets::crc32(id.as_bytes());
                if let Some(t) = self.sys_texts.get(&key).cloned() {
                    self.say(&[t]);
                }
            }
            Step::Save => {
                save.chapter += 1;
                save.has_watch = true;
                out.push(Out::Save);
            }
        }
        out
    }

    fn finish_battle(&mut self, o: Outcome, party: Vec<Member>, save: &mut Save) {
        let story = (save.chapter as usize) < STEPS.len();
        // [decisão do port] o time volta com HP cheio depois de cada batalha (ainda não há cura no campo).
        save.party = party.iter().map(|m| (m.param, m.level, m.exp)).collect();
        match o {
            Outcome::Won {
                exp,
                money,
                befriended,
            } => {
                save.money += money;
                let mut lines = vec![format!(
                    "Your Yo-kai earned {exp} EXP and you got {money} money!"
                )];
                if let Some(p) = befriended {
                    let name = self
                        .db
                        .species
                        .get(&p)
                        .map_or("The Yo-kai".into(), |s| s.name.clone());
                    if save.party.len() < 6 {
                        save.party.push((p, party_level_hint(&party), 0));
                        lines.push(format!(
                            "{name} wants to be your friend!\n{name} joined your team!"
                        ));
                    } else {
                        lines.push(format!(
                            "{name} wanted to be your friend, but your team is full."
                        ));
                    }
                }
                if story {
                    save.chapter += 1;
                }
                self.say(&lines);
            }
            Outcome::Lost if story => {
                // batalha da história: tenta de novo (o tutorial não tem derrota de verdade)
                self.say(&["Your Yo-kai were defeated... Try again!".to_string()]);
            }
            Outcome::Lost => self.say(&["Your Yo-kai were defeated...".to_string()]),
            Outcome::Fled => {}
        }
    }

    /// Jogo livre: busca com a lente nos mapas com relógio de Yo-kai (`watchmap`).
    fn free_play(&mut self, room: &mut Room, input: Option<&Input>, save: &Save) {
        let Some(i) = input else {
            return;
        };
        if !self.watch.contains_key(&room.id) || !i.down(Button::Y) || save.party.is_empty() {
            self.lens = 0;
            return;
        }
        room.idle();
        self.lens += 1;
        if self.lens < LENS_TICKS {
            return;
        }
        self.lens = 0;
        if self.roll() > FIND_CHANCE {
            self.say(&["There doesn't seem to be anything here.".to_string()]);
            return;
        }
        // período do relógio do jogo (= hora do PC): 0 dia, 3 noite
        let period = crate::clock::query(3);
        // [H] os 8 pontos de busca: setor do mapa pela posição do jogador
        let p = room.player.pos;
        let spot = 1
            + (((p[0] / 80.0).floor() as i32 * 7 + (p[2] / 80.0).floor() as i32 * 13)
                .rem_euclid(8));
        let roll = self.roll();
        let watch_rank = 0; // relógio rank E no começo do jogo
        let Some(table) = self.watch[&room.id].pick(&self.db, watch_rank, period, 0, spot, roll)
        else {
            return;
        };
        let enemies = self.db.encounter_by_hash(table);
        if enemies.is_empty() {
            return;
        }
        let names: Vec<String> = enemies
            .iter()
            .filter_map(|e| self.db.species.get(&e.0))
            .map(|s| s.name.clone())
            .collect();
        eprintln!("[yw1] lente: ponto {spot}, período {period}, {names:?}");
        let party = self.party(save);
        self.battle = Some(Battle::new(
            &self.fa,
            self.db.clone(),
            &party,
            &enemies,
            "t100b02",
            true,
        ));
    }

    /// Quadro 3D do que estiver em primeiro plano (batalha ou evento).
    pub fn frame(&mut self, scale: usize, screens: &mut Screens) -> Option<&Frame> {
        if let Some(b) = self.battle.as_mut() {
            return Some(b.draw(scale, screens));
        }
        self.scene.as_mut().map(|e| e.draw(scale, screens))
    }

    /// Por cima: mensagens do port (entrada no time, resultado da batalha).
    pub fn overlay(&self, top: &mut [u32], w: usize, h: usize, screens: &mut Screens) {
        if let Some(d) = &self.dialog {
            d.draw(top, w, h, screens);
        }
    }
}

fn fa_font(fa: &Archive) -> Option<crate::assets::font::Font> {
    fa.read("fnt/ft_nrm.xf")
        .and_then(|b| crate::assets::font::Font::parse(&b))
        .ok()
}

impl Story {
    /// Objetivo atual numa faixa no topo da tela de baixo.
    pub fn draw_hint(&self, text: &str, screens: &mut Screens) {
        use crate::screen::{BOTTOM_H, BOTTOM_W};
        for px in screens.bottom[..22 * BOTTOM_W].iter_mut() {
            *px = 0x10_1830;
        }
        if let Some(f) = &self.font {
            f.draw(
                &mut screens.bottom,
                BOTTOM_W,
                BOTTOM_H,
                (8.0, 4.0),
                text,
                0xff_ffff,
                0.8,
                1.0,
            );
        }
    }
}

fn party_level_hint(party: &[Member]) -> u32 {
    party.iter().map(|m| m.level).min().unwrap_or(1).max(1)
}

#[allow(dead_code)]
fn _check_hashes() {
    debug_assert_eq!(hash("para_y311") as u32, CADIN);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Os tutoriais dos scripts de fase têm texto em `system_text_en` (achado pelo hash do nome).
    #[test]
    fn tutorial_texts_resolve() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let t = crate::event_host::system_texts(&fa);
        for id in ["sys_info_win_11", "sys_info_win_13", "sys_info_win_20"] {
            let text = t.get(&crate::assets::crc32(id.as_bytes()));
            assert!(text.is_some_and(|x| x.len() > 10), "{id} sem texto");
        }
        let infos = STEPS.iter().filter(|s| matches!(s, Step::Info(_))).count();
        assert_eq!(infos, 3);
    }

    #[test]
    fn step_constants() {
        assert_eq!(hash("para_y311") as u32, CADIN);
        assert!(matches!(STEPS[STEPS.len() - 1], Step::Warp("t102g00", ..)));
    }

    /// Do pé da escada (t101i01) até o jogo livre, sem janela: anda pelos mapas por teleporte quando a fase
    /// pede para "chegar" num mapa, aperta A nas falas e o tutorial tem que ser vencido.
    #[test]
    #[ignore]
    fn chapter1_reaches_free_play() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = Rc::new(Archive::open(&romfs.join("yw1_a.fa")).unwrap());
        let mut story = Story::new(fa.clone(), 1).unwrap();
        let mut save = Save::new_game("Nate", 1);
        save.prologue_done = true;
        let mut room = Room::load(&fa, "t101i01").unwrap();
        let mut input = Input::headless();
        let mut t = 0u64;
        while save.chapter < DONE && t < 400_000 {
            input.set(Button::A, t.is_multiple_of(20));
            if let Some(Step::Arrive(map, _)) = STEPS.get(save.chapter as usize) {
                if room.id != *map {
                    room = Room::load(&fa, map).unwrap();
                }
            }
            for o in story.tick(&mut room, Some(&input), &mut save) {
                if let Out::Warp(m, pos, yaw) = o {
                    room = Room::load(&fa, &m).unwrap();
                    room.place(pos, yaw);
                }
            }
            t += 1;
        }
        println!(
            "passo {} de {DONE} em {t} ticks; time {:?}",
            save.chapter, save.party
        );
        assert_eq!(save.chapter, DONE);
        assert_eq!(save.party.len(), 1);
        assert_eq!(story.db.species[&save.party[0].0].name, "Buhu");
    }
}
