//! Batalha de Yo-kai (sistema do YW1: até 6 no anel, 3 na frente lutando sozinhos em tempo real; o jogador gira
//! o anel, escolhe o alvo, dispara Soultimates e purifica).
//!
//! No jogo a lógica da batalha é NATIVA (o `BattleMenu_001e.xq` só desenha menus e `yw_battle_sys_012a.xq` só
//! toca efeitos), então aqui ela é reconstruída em Rust a partir das tabelas reais (`yokai.rs`): atributos, golpes
//! com as chances de cada um, Soultimate, multiplicadores de elemento, experiência e dinheiro. A interface usa as
//! peças dos layouts reais `battle_u00_en.xa` (barras de HP, gauge de Soultimate, caixa de mensagem) e
//! `battle_d00_en.xa` (fundo e botões da tela de baixo); os modelos e clipes de batalha (`戦1…`) são os dos Yo-kai.
//!
//! [H] Ainda NÃO lidos do code.bin: a fórmula de dano, a velocidade do turno, o ganho do gauge, o efeito de cada
//! "inspirar", a chance de fazer amizade e as posições/câmera do cenário. Estão concentrados em `damage`,
//! `GAUGE_RATE`, `SOUL_*`, `befriend_chance` e `slot_pos`.

use crate::assets::arc0::Archive;
use crate::assets::font::Font;
use crate::character::{Character, Motion};
use crate::gfx3d::{self, Camera, Frame, MeshRef, V3};
use crate::input::{Button, Input};
use crate::layout::{Anim, Layout};
use crate::render2d::Target;
use crate::room::Room;
use crate::screen::{Screens, BOTTOM_H, BOTTOM_W, TOP_H, TOP_W};
use crate::yokai::{Db, Kind, Member};
use std::rc::Rc;

/// [H] pontos de gauge de ação por ponto de velocidade, por quadro de 30 Hz (age ao chegar a 100).
const GAUGE_RATE: f32 = 0.11;
/// [H] Soultimate: gauge ganho ao agir e ao receber dano (por ponto de HP perdido em % do HP máximo).
const SOUL_PER_ACTION: f32 = 9.0;
const SOUL_PER_HURT: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    Won {
        exp: u32,
        money: u32,
        /// Param do Yo-kai que quis ser amigo (batalhas na natureza).
        befriended: Option<i32>,
    },
    Lost,
    Fled,
}

struct Fighter {
    member: Member,
    max_hp: i32,
    body: Option<Character>,
    motion: Option<Motion>,
    clip: &'static str,
    frame: f32,
    /// Quadros (30 Hz) até voltar ao clipe parado.
    busy: u32,
    gauge: f32,
    soul: f32,
    guarding: bool,
    /// Inspirado (enfraquecido): quadros restantes.
    inspirited: u32,
    flash: u32,
}

impl Fighter {
    fn new(fa: &Archive, db: &Db, member: Member) -> Fighter {
        let model = member
            .species(db)
            .map(|s| s.model.clone())
            .unwrap_or_default();
        let body = fa
            .read(&format!("data/character/{model}/{model}_p00.xc"))
            .and_then(|b| Character::load(&b))
            .ok();
        let motion = fa
            .read(&format!("data/character/{model}/{model}_p10.xc"))
            .and_then(|b| Motion::load(&b))
            .ok();
        let max_hp = member.stats(db)[0];
        let mut f = Fighter {
            member,
            max_hp,
            body,
            motion,
            clip: "",
            frame: 0.0,
            busy: 0,
            gauge: 0.0,
            soul: 0.0,
            guarding: false,
            inspirited: 0,
            flash: 0,
        };
        f.play("戦1立ち1L");
        f
    }

    fn alive(&self) -> bool {
        self.member.hp > 0
    }

    fn play(&mut self, clip: &'static str) {
        self.clip = clip;
        if let Some(c) = self.motion.as_ref().and_then(|m| m.clip(clip)) {
            self.frame = c.start;
        }
    }

    /// Duração do clipe em quadros de 30 Hz.
    fn clip_len(&self, clip: &str) -> u32 {
        self.motion
            .as_ref()
            .and_then(|m| m.clip(clip))
            .map_or(20, |c| ((c.end - c.start) / c.speed.max(0.1) / 2.0) as u32)
    }

    fn act(&mut self, clip: &'static str) {
        self.play(clip);
        self.busy = self.clip_len(clip).max(10);
    }

    fn animate(&mut self) {
        let Some(c) = self.motion.as_ref().and_then(|m| m.clip(self.clip)) else {
            return;
        };
        let (start, end, speed) = (c.start, c.end, c.speed);
        self.frame += speed * 2.0;
        if self.frame >= end {
            if self.clip.ends_with('L') {
                self.frame = start + (self.frame - start).rem_euclid((end - start).max(1.0));
            } else {
                self.frame = end - 0.01;
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Intro(u32),
    Fight,
    /// Menu de Soultimate aberto: índice na frente (0..3).
    SoulMenu(usize),
    Soultimate {
        who: usize,
        t: u32,
    },
    Won(u32),
    Lost(u32),
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

pub struct Battle {
    db: Rc<Db>,
    stage: Option<Room>,
    party: Vec<Fighter>,
    /// Rotação do anel: os da frente são `ring + 0..3` (módulo o tamanho do time).
    ring: usize,
    enemies: Vec<Fighter>,
    target: usize,
    phase: Phase,
    rng: Rng,
    wild: bool,
    log: Vec<String>,
    log_timer: u32,
    tick: u64,
    font: Option<Font>,
    top_ui: Option<Layout>,
    bottom_ui: Option<Layout>,
    frame: Frame,
    pub outcome: Option<Outcome>,
    /// Time como ficou depois da batalha (HP/experiência), para devolver ao save.
    pub result_party: Vec<Member>,
}

/// [H] Dano: ataque × poder − defesa/2, ±10 %, crítico 5 % (×1,5), guarda ½, multiplicador do elemento.
fn damage(atk: i32, def: i32, power: f32, resist: f32, guard: bool, rng: &mut Rng) -> (i32, bool) {
    let base = (atk as f32 * power - def as f32 * 0.5).max(1.0);
    let crit = rng.next() < 0.05;
    let mut d = base * (0.9 + rng.next() * 0.2) * if crit { 1.5 } else { 1.0 } * resist.max(0.1);
    if guard {
        d *= 0.5;
    }
    (d.round().max(1.0) as i32, crit)
}

/// [H] chance de um Yo-kai derrotado querer ser amigo (no jogo depende de rank, comida dada e sorte).
fn befriend_chance() -> f32 {
    0.35
}

/// [H] posições no cenário `t100b02`: aliados perto da câmera (de costas), inimigos ao fundo.
fn slot_pos(enemy: bool, i: usize, n: usize) -> V3 {
    let x = match n {
        1 => 0.0,
        2 => [-6.0, 6.0][i % 2],
        _ => [-10.0, 0.0, 10.0][i % 3],
    };
    let i = if n == 1 { 0 } else { i };
    if enemy {
        [x, 0.0, -16.0 - if i == 1 { 3.0 } else { 0.0 }]
    } else {
        [x * 1.2, 0.0, 10.0 + if i == 1 { 2.0 } else { 0.0 }]
    }
}

impl Battle {
    /// `party`: o time em ordem do anel (máx. 6); `enemies`: (param, nível); `wild` permite fazer amizade e fugir.
    pub fn new(
        fa: &Archive,
        db: Rc<Db>,
        party: &[Member],
        enemies: &[(i32, u32)],
        stage: &str,
        wild: bool,
    ) -> Battle {
        let party: Vec<Fighter> = party
            .iter()
            .take(6)
            .map(|m| Fighter::new(fa, &db, m.clone()))
            .collect();
        let enemies = enemies
            .iter()
            .take(3)
            .map(|&(p, l)| Fighter::new(fa, &db, Member::new(&db, p, l)))
            .collect();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9, |d| d.as_nanos() as u64)
            | 1;
        Battle {
            stage: Room::load(fa, stage).ok(),
            font: fa.read("fnt/ft_nrm.xf").and_then(|b| Font::parse(&b)).ok(),
            top_ui: fa
                .read("data/menu/battle_u00_en.xa")
                .and_then(|b| Layout::load(&b))
                .ok(),
            bottom_ui: fa
                .read("data/menu/battle_d00_en.xa")
                .and_then(|b| Layout::load(&b))
                .ok(),
            db,
            party,
            ring: 0,
            enemies,
            target: 0,
            phase: Phase::Intro(45),
            rng: Rng(seed),
            wild,
            log: Vec::new(),
            log_timer: 0,
            tick: 0,
            frame: Frame::new(1, 1),
            outcome: None,
            result_party: Vec::new(),
        }
    }

    /// Para testes: semente fixa.
    #[cfg(test)]
    fn seed(&mut self, s: u64) {
        self.rng = Rng(s | 1);
    }

    fn front(&self) -> Vec<usize> {
        let n = self.party.len();
        (0..n.min(3)).map(|k| (self.ring + k) % n).collect()
    }

    fn say(&mut self, s: String) {
        self.log.push(s);
        if self.log.len() > 2 {
            self.log.remove(0);
        }
        self.log_timer = 75;
    }

    pub fn tick(&mut self, input: Option<&Input>) {
        self.tick += 1;
        // lógica a 30 Hz, entrada a cada quadro
        if let Some(i) = input {
            self.handle_input(i);
        }
        if self.tick % 2 == 1 {
            return;
        }
        for f in self.party.iter_mut().chain(self.enemies.iter_mut()) {
            f.animate();
            f.flash = f.flash.saturating_sub(1);
            f.inspirited = f.inspirited.saturating_sub(1);
            if f.busy > 0 {
                f.busy -= 1;
                if f.busy == 0 {
                    f.play(if f.alive() {
                        "戦1立ち1L"
                    } else {
                        "戦1死1L"
                    });
                }
            }
        }
        self.log_timer = self.log_timer.saturating_sub(1);
        match self.phase {
            Phase::Intro(t) => {
                if t == 45 {
                    let names: Vec<String> = self
                        .enemies
                        .iter()
                        .map(|e| e.member.name(&self.db))
                        .collect();
                    self.say(format!("{} appeared!", names.join(", ")));
                }
                self.phase = if t == 0 {
                    Phase::Fight
                } else {
                    Phase::Intro(t - 1)
                };
            }
            Phase::Fight | Phase::SoulMenu(_) => self.fight_step(),
            Phase::Soultimate { who, t } => {
                if t == 0 {
                    self.finish_soultimate(who);
                    self.phase = Phase::Fight;
                } else {
                    self.phase = Phase::Soultimate { who, t: t - 1 };
                }
            }
            Phase::Won(t) => {
                if t == 0 {
                    self.finish(true);
                } else {
                    self.phase = Phase::Won(t - 1);
                }
            }
            Phase::Lost(t) => {
                if t == 0 {
                    self.finish(false);
                } else {
                    self.phase = Phase::Lost(t - 1);
                }
            }
        }
    }

    fn handle_input(&mut self, i: &Input) {
        match self.phase {
            Phase::Fight => {
                // L/R (ou Q/E): girar o anel; ↑/↓: trocar o alvo; A: Soultimate; Y: purificar; B segurado: fugir
                if i.pressed(Button::L) || i.pressed(Button::Left) {
                    self.rotate(-1);
                }
                if i.pressed(Button::R) || i.pressed(Button::Right) {
                    self.rotate(1);
                }
                if i.pressed(Button::Up) || i.pressed(Button::Down) {
                    self.cycle_target(if i.pressed(Button::Up) { -1 } else { 1 });
                }
                if i.pressed(Button::A) {
                    let front = self.front();
                    let ready = front
                        .iter()
                        .position(|&k| self.party[k].alive() && self.party[k].soul >= 100.0);
                    match ready {
                        Some(k) => self.phase = Phase::SoulMenu(k),
                        None => self.say("No Soultimate is charged yet.".into()),
                    }
                }
                if i.pressed(Button::Y) {
                    self.purify();
                }
                if i.pressed(Button::B) && self.wild {
                    self.say("You got away safely!".into());
                    self.outcome = Some(Outcome::Fled);
                    self.result_party = self.party.iter().map(|f| f.member.clone()).collect();
                }
                if let Some((x, y)) = i.touch_released() {
                    self.touch(x, y);
                }
            }
            Phase::SoulMenu(k) => {
                if i.pressed(Button::Left) || i.pressed(Button::L) {
                    self.phase = Phase::SoulMenu((k + 2) % 3);
                }
                if i.pressed(Button::Right) || i.pressed(Button::R) {
                    self.phase = Phase::SoulMenu((k + 1) % 3);
                }
                if i.pressed(Button::B) {
                    self.phase = Phase::Fight;
                }
                if i.pressed(Button::A) {
                    let front = self.front();
                    if let Some(&who) = front.get(k) {
                        if self.party[who].alive() && self.party[who].soul >= 100.0 {
                            self.start_soultimate(who);
                        } else {
                            self.say("That Yo-kai isn't charged.".into());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Tela de baixo: anel (toque à esquerda/direita gira), botão de Soultimate (canto inferior esquerdo).
    fn touch(&mut self, x: i32, y: i32) {
        if y > 180 && x < 100 {
            let front = self.front();
            if let Some(k) = front.iter().position(|&k| self.party[k].soul >= 100.0) {
                self.phase = Phase::SoulMenu(k);
            }
        } else if y < 180 {
            self.rotate(if x < BOTTOM_W as i32 / 2 { -1 } else { 1 });
        }
    }

    fn rotate(&mut self, dir: i32) {
        let n = self.party.len();
        if n <= 3 {
            return;
        }
        self.ring = (self.ring as i32 + dir).rem_euclid(n as i32) as usize;
        for k in self.front() {
            self.party[k].gauge = self.party[k].gauge.min(40.0);
        }
    }

    fn cycle_target(&mut self, dir: i32) {
        let n = self.enemies.len() as i32;
        for step in 1..=n {
            let t = (self.target as i32 + dir * step).rem_euclid(n) as usize;
            if self.enemies[t].alive() {
                self.target = t;
                return;
            }
        }
    }

    fn purify(&mut self) {
        let front = self.front();
        if let Some(&k) = front.iter().find(|&&k| self.party[k].inspirited > 0) {
            self.party[k].inspirited = 0;
            self.party[k].soul = (self.party[k].soul + 25.0).min(100.0);
            let n = self.party[k].member.name(&self.db);
            self.say(format!("{n} was purified!"));
        }
    }

    fn fight_step(&mut self) {
        // aliados da frente
        let front = self.front();
        for &k in &front {
            let f = &mut self.party[k];
            if !f.alive() || f.busy > 0 {
                continue;
            }
            let spd = f.member.stats(&self.db)[4] as f32 * if f.inspirited > 0 { 0.6 } else { 1.0 };
            f.gauge += spd * GAUGE_RATE;
            if f.gauge >= 100.0 {
                f.gauge = 0.0;
                self.act(false, k);
            }
        }
        for k in 0..self.enemies.len() {
            let f = &mut self.enemies[k];
            if !f.alive() || f.busy > 0 {
                continue;
            }
            let spd = f.member.stats(&self.db)[4] as f32 * if f.inspirited > 0 { 0.6 } else { 1.0 };
            f.gauge += spd * GAUGE_RATE;
            if f.gauge >= 100.0 {
                f.gauge = 0.0;
                self.act(true, k);
            }
        }
        if self.enemies.iter().all(|e| !e.alive()) {
            for &k in &front {
                if self.party[k].alive() {
                    self.party[k].act("戦1勝利1");
                }
            }
            self.say("You won!".into());
            self.phase = Phase::Won(60);
        } else if self.party.iter().all(|f| !f.alive()) {
            self.say("Your Yo-kai were defeated...".into());
            self.phase = Phase::Lost(60);
        } else if front.iter().all(|&k| !self.party[k].alive()) {
            // frente toda caída: gira para um vivo
            self.rotate(1);
        }
        if !self.enemies[self.target.min(self.enemies.len() - 1)].alive() {
            self.cycle_target(1);
        }
    }

    /// Um lutador escolhe e executa um golpe pelas chances da tabela.
    fn act(&mut self, enemy: bool, k: usize) {
        let db = self.db.clone();
        let me = if enemy {
            &self.enemies[k]
        } else {
            &self.party[k]
        };
        let Some(sp) = me.member.species(&db) else {
            return;
        };
        let total: i32 = sp.moves.iter().map(|m| m.1.max(0)).sum::<i32>().max(1);
        let mut roll = (self.rng.next() * total as f32) as i32;
        let mut pick = sp.moves[0].0;
        for (h, pct) in sp.moves {
            if roll < pct.max(0) {
                pick = h;
                break;
            }
            roll -= pct.max(0);
        }
        let cmd = db.command(pick).cloned();
        let kind = cmd.as_ref().map_or(Kind::Attack, |c| c.kind);
        let move_name = cmd
            .as_ref()
            .map_or_else(|| "Attack".to_string(), |c| c.name.clone());
        let hits = cmd.as_ref().map_or(1, |c| c.hits).clamp(1, 3);
        let my_name = me.member.name(&db);
        let stats = me.member.stats(&db);
        let weak = me.inspirited > 0;
        // alvo
        let target = if enemy {
            let alive: Vec<usize> = self
                .front()
                .into_iter()
                .filter(|&t| self.party[t].alive())
                .collect();
            if alive.is_empty() {
                return;
            }
            alive[(self.rng.next() * alive.len() as f32) as usize % alive.len()]
        } else {
            if !self.enemies[self.target].alive() {
                self.cycle_target(1);
            }
            self.target
        };
        let me = if enemy {
            &mut self.enemies[k]
        } else {
            &mut self.party[k]
        };
        me.guarding = false;
        me.soul = (me.soul + SOUL_PER_ACTION).min(100.0);
        match kind {
            Kind::Guard => {
                me.guarding = true;
                me.act("戦1ガード1L");
                self.say(format!("{my_name} is guarding."));
                return;
            }
            Kind::Inspirit => {
                me.act("戦1ようじゅつ1");
                let t = if enemy {
                    &mut self.party[target]
                } else {
                    &mut self.enemies[target]
                };
                t.inspirited = 240;
                let tn = t.member.name(&db);
                self.say(format!("{my_name} used {move_name}! {tn} was inspirited."));
                return;
            }
            Kind::Technique | Kind::Soultimate => me.act("戦1ようじゅつ1"),
            Kind::Attack => me.act("戦1こうげき1"),
        }
        let (atk, power) = match kind {
            Kind::Technique => (stats[2], 1.3),
            _ => (stats[1], 1.0),
        };
        let atk = if weak { (atk as f32 * 0.7) as i32 } else { atk };
        let t = if enemy {
            &mut self.party[target]
        } else {
            &mut self.enemies[target]
        };
        let def = t.member.stats(&db)[3];
        let mut total_dmg = 0;
        let mut crit_any = false;
        for _ in 0..hits {
            let (d, crit) = damage(
                atk,
                def,
                power / hits as f32,
                1.0,
                t.guarding,
                &mut self.rng,
            );
            total_dmg += d;
            crit_any |= crit;
        }
        Self::hurt(t, total_dmg);
        let tn = t.member.name(&db);
        let dead = !t.alive();
        self.say(format!(
            "{my_name} used {move_name}!{} {tn} took {total_dmg} damage.",
            if crit_any { " Critical!" } else { "" }
        ));
        if dead {
            self.say(format!("{tn} was defeated!"));
        }
    }

    fn hurt(t: &mut Fighter, dmg: i32) {
        t.member.hp = (t.member.hp - dmg).max(0);
        t.soul = (t.soul + dmg as f32 * 100.0 / t.max_hp.max(1) as f32 * SOUL_PER_HURT).min(100.0);
        t.flash = 6;
        if t.alive() {
            if t.busy == 0 {
                t.act("戦1ダメージ1");
            }
        } else {
            t.act("戦1死1");
            t.gauge = 0.0;
        }
    }

    fn start_soultimate(&mut self, who: usize) {
        let f = &mut self.party[who];
        f.soul = 0.0;
        f.act("戦1ひっさつ1");
        let t = f.busy.max(30);
        let n = f.member.name(&self.db);
        let s = f
            .member
            .species(&self.db)
            .and_then(|s| self.db.command(s.soultimate))
            .map_or("Soultimate".to_string(), |c| c.name.clone());
        self.say(format!("{n}'s Soultimate: {s}!"));
        self.phase = Phase::Soultimate { who, t };
    }

    /// [H] Soultimate: espírito × 2,5 no alvo (o minigame de toque do 3DS ainda não existe: conta como acerto).
    fn finish_soultimate(&mut self, who: usize) {
        let db = self.db.clone();
        let spr = self.party[who].member.stats(&db)[2];
        let target = self.target;
        let t = &mut self.enemies[target];
        if !t.alive() {
            return;
        }
        let def = t.member.stats(&db)[3];
        let (d, _) = damage(spr, def, 2.5, 1.0, false, &mut self.rng);
        Self::hurt(t, d);
        let tn = t.member.name(&db);
        self.say(format!("{tn} took {d} damage!"));
    }

    fn finish(&mut self, won: bool) {
        if self.outcome.is_some() {
            return;
        }
        let db = self.db.clone();
        if !won {
            self.outcome = Some(Outcome::Lost);
            self.result_party = self.party.iter().map(|f| f.member.clone()).collect();
            return;
        }
        let exp: u32 = self
            .enemies
            .iter()
            .filter_map(|e| e.member.species(&db))
            .map(|s| s.exp.max(0) as u32)
            .sum();
        let money: u32 = self
            .enemies
            .iter()
            .filter_map(|e| e.member.species(&db))
            .map(|s| s.money.max(0) as u32)
            .sum();
        for f in self.party.iter_mut() {
            if f.alive() {
                f.member.gain_exp(&db, exp);
            }
        }
        let befriended = if self.wild {
            let roll = self.rng.next();
            self.enemies
                .iter()
                .find(|_| roll < befriend_chance())
                .map(|e| e.member.param)
        } else {
            None
        };
        self.result_party = self.party.iter().map(|f| f.member.clone()).collect();
        self.outcome = Some(Outcome::Won {
            exp,
            money,
            befriended,
        });
    }

    // ---------------------------------------------------------------- desenho

    pub fn draw(&mut self, scale: usize, screens: &mut Screens) -> &Frame {
        let (w, h) = (TOP_W * scale, TOP_H * scale);
        self.frame.resize(w, h);
        self.frame.clear(0x0090_c0e8);
        // [H] câmera de batalha: atrás e acima do time, olhando os inimigos
        let sway = (self.tick as f32 * 0.01).sin() * 1.5;
        let cam = Camera::look_at([sway, 15.0, 32.0], [0.0, 3.0, -12.0], 40.0, 0.5);
        let mut frame = std::mem::replace(&mut self.frame, Frame::new(1, 1));
        if let Some(st) = &self.stage {
            st.draw_world(&mut frame, &cam, false);
        }
        let front = self.front();
        for (slot, &k) in front.iter().enumerate() {
            Self::draw_fighter(
                &mut frame,
                &cam,
                &self.party[k],
                slot_pos(false, slot, front.len()),
                180.0,
            );
        }
        for (i, e) in self.enemies.iter().enumerate() {
            if e.alive() || e.busy > 0 {
                Self::draw_fighter(
                    &mut frame,
                    &cam,
                    e,
                    slot_pos(true, i, self.enemies.len()),
                    0.0,
                );
            }
        }
        if let Some(st) = &self.stage {
            st.draw_world(&mut frame, &cam, true);
        }
        self.frame = frame;
        self.draw_top_ui(&cam, scale);
        self.draw_bottom(screens);
        &self.frame
    }

    fn draw_fighter(frame: &mut Frame, cam: &Camera, f: &Fighter, at: V3, yaw: f32) {
        let Some(body) = &f.body else {
            return;
        };
        let skinned: Vec<Vec<V3>> = match &f.motion {
            Some(m) => body.skin(m, f.frame),
            None => body.meshes.iter().map(|m| m.pos.clone()).collect(),
        };
        let (s, c) = yaw.to_radians().sin_cos();
        let tint = if f.flash > 0 {
            [1.6, 1.2, 1.2]
        } else if f.inspirited > 0 {
            [0.8, 0.7, 1.1]
        } else {
            [1.0; 3]
        };
        for (mesh, verts) in body.meshes.iter().zip(skinned) {
            // `Scene_Material`: caixa envolvente de 24 vértices (volume do personagem), não é desenhada
            if mesh.material.ends_with("Scene_Material") {
                continue;
            }
            let pos: Vec<V3> = verts
                .iter()
                .map(|v| {
                    [
                        v[0] * c + v[2] * s + at[0],
                        v[1] + at[1],
                        -v[0] * s + v[2] * c + at[2],
                    ]
                })
                .collect();
            let colors = vec![[1.0; 4]; pos.len()];
            let tex = mesh
                .texture
                .and_then(|t| body.textures.get(t))
                .and_then(|t| t.as_ref());
            gfx3d::draw_mesh(
                frame,
                cam,
                &MeshRef {
                    pos: &pos,
                    color: &colors,
                    uv: &mesh.uv,
                    tris: &mesh.tris,
                    offset: [0.0; 3],
                    texture: tex,
                    blend: false,
                    tint,
                },
            );
        }
    }

    /// Camada 2D 400×240 sobre o 3D: barras de HP dos inimigos e da frente, mensagens.
    fn draw_top_ui(&mut self, cam: &Camera, scale: usize) {
        // camada 400×240 iniciada com o 3D reduzido: as peças com transparência misturam sobre a cena
        let mut ui = vec![0u32; TOP_W * TOP_H];
        for (i, px) in ui.iter_mut().enumerate() {
            let (x, y) = (i % TOP_W, i / TOP_W);
            *px = self.frame.color[(y * scale) * self.frame.width + x * scale];
        }
        let before = ui.clone();
        let db = self.db.clone();
        if let Some(l) = &self.top_ui {
            let mut t = Target {
                pixels: &mut ui,
                width: TOP_W,
                height: TOP_H,
            };
            // HP dos inimigos acima de cada um
            for (i, e) in self.enemies.iter().enumerate() {
                if !e.alive() {
                    continue;
                }
                let p = slot_pos(true, i, self.enemies.len());
                if let Some((x, y)) =
                    cam.project([p[0], p[1] + 9.0, p[2]], TOP_W as f32, TOP_H as f32)
                {
                    let off = (x - 200.0, y - 120.0);
                    hp_bar(
                        l,
                        &mut t,
                        "en_hp_base",
                        "en_hp_bar",
                        off,
                        e.member.hp,
                        e.max_hp,
                    );
                }
            }
            // HP da frente, embaixo
            for (slot, &k) in self.front().iter().enumerate() {
                let f = &self.party[k];
                let off = (-130.0 + slot as f32 * 130.0, 82.0);
                hp_bar(
                    l,
                    &mut t,
                    "ptu_hp_base",
                    "ptu_hp_bar",
                    off,
                    f.member.hp,
                    f.max_hp,
                );
            }
        }
        if let Some(font) = &self.font {
            for (slot, &k) in self.front().iter().enumerate() {
                let f = &self.party[k];
                let x = 18.0 + slot as f32 * 130.0;
                let soul = if f.soul >= 100.0 { " ★" } else { "" };
                let txt = format!(
                    "{}{soul}\nHP {}/{}",
                    f.member.name(&db),
                    f.member.hp,
                    f.max_hp
                );
                font.draw(
                    &mut ui,
                    TOP_W,
                    TOP_H,
                    (x + 1.0, 177.0),
                    &txt,
                    0x00_0000,
                    0.8,
                    1.0,
                );
                font.draw(&mut ui, TOP_W, TOP_H, (x, 176.0), &txt, 0xff_ffff, 0.8, 1.0);
            }
            if self.log_timer > 0 {
                // faixa escura translúcida atrás das mensagens
                for px in ui[24 * TOP_W..60 * TOP_W].iter_mut() {
                    let c = *px;
                    *px = ((c >> 1) & 0x7f_7f7f) + 0x08_0818;
                }
                let msg = self.log.join("\n");
                font.draw(
                    &mut ui,
                    TOP_W,
                    TOP_H,
                    (37.0, 33.0),
                    &msg,
                    0x00_0000,
                    0.8,
                    1.0,
                );
                font.draw(
                    &mut ui,
                    TOP_W,
                    TOP_H,
                    (36.0, 32.0),
                    &msg,
                    0xff_ffff,
                    0.8,
                    1.0,
                );
            }
            if let Some((i, e)) = self
                .enemies
                .iter()
                .enumerate()
                .find(|(i, e)| *i == self.target && e.alive())
            {
                let p = slot_pos(true, i, self.enemies.len());
                if let Some((x, y)) =
                    cam.project([p[0], p[1] + 11.0, p[2]], TOP_W as f32, TOP_H as f32)
                {
                    let n = e.member.name(&db);
                    let tw = font.width(&n) as f32 * 0.8;
                    font.draw(
                        &mut ui,
                        TOP_W,
                        TOP_H,
                        (x - tw / 2.0, y - 18.0),
                        &format!("▼ {n}"),
                        0xff_e040,
                        0.8,
                        1.0,
                    );
                }
            }
        }
        for (i, &p) in ui.iter().enumerate() {
            if p != before[i] {
                let (x, y) = (i % TOP_W, i / TOP_W);
                for dy in 0..scale {
                    for dx in 0..scale {
                        let idx = (y * scale + dy) * self.frame.width + x * scale + dx;
                        if let Some(px) = self.frame.color.get_mut(idx) {
                            *px = p;
                        }
                    }
                }
            }
        }
    }

    fn draw_bottom(&self, screens: &mut Screens) {
        screens.bottom.fill(0x0020_2a40);
        let db = &self.db;
        if let Some(l) = &self.bottom_ui {
            let mut t = Target {
                pixels: &mut screens.bottom,
                width: BOTTOM_W,
                height: BOTTOM_H,
            };
            let names = [
                "bg_on", "ring_on", "btn_cmd0", "btn_cmd1", "btn_cmd2", "btn_cmd3",
            ];
            let anims: Vec<Anim> = names
                .iter()
                .filter_map(|n| Anim::new(l, n, true).ok())
                .chain(if self.wild {
                    Anim::new(l, "btn_escp", true).ok()
                } else {
                    None
                })
                .collect();
            let refs: Vec<&Anim> = anims.iter().collect();
            l.draw(&mut t, &refs);
        }
        if let Some(font) = &self.font {
            let n = self.party.len();
            let front = self.front();
            // anel: 6 posições em volta do centro (frente em cima)
            for (k, f) in self.party.iter().enumerate() {
                let pos = (k + n - self.ring) % n;
                let ang = (pos as f32 - 1.0) * std::f32::consts::TAU / n.max(1) as f32
                    - std::f32::consts::FRAC_PI_2;
                let (x, y) = (160.0 + ang.cos() * 70.0, 100.0 + ang.sin() * 55.0);
                let name = f.member.name(db);
                let tw = font.width(&name) as f32 * 0.8;
                let col = if !f.alive() {
                    0x80_8080
                } else if front.contains(&k) {
                    0xff_ffff
                } else {
                    0xb0_c0e0
                };
                font.draw(
                    &mut screens.bottom,
                    BOTTOM_W,
                    BOTTOM_H,
                    (x - tw / 2.0, y - 6.0),
                    &name,
                    col,
                    0.8,
                    1.0,
                );
                let g = format!("Lv{} SP {:>3.0}%", f.member.level, f.soul);
                let gw = font.width(&g) as f32 * 0.6;
                font.draw(
                    &mut screens.bottom,
                    BOTTOM_W,
                    BOTTOM_H,
                    (x - gw / 2.0, y + 7.0),
                    &g,
                    col,
                    0.6,
                    1.0,
                );
            }
            let help = match self.phase {
                Phase::SoulMenu(k) => {
                    let who = front
                        .get(k)
                        .map(|&i| self.party[i].member.name(db))
                        .unwrap_or_default();
                    format!("Soultimate: {who}  (←/→ choose, A go, B back)")
                }
                _ => {
                    let esc = if self.wild { "  B: run" } else { "" };
                    format!("L/R: rotate  ↑/↓: target  A: Soultimate  Y: purify{esc}")
                }
            };
            font.draw(
                &mut screens.bottom,
                BOTTOM_W,
                BOTTOM_H,
                (8.0, 222.0),
                &help,
                0xff_ffff,
                0.6,
                1.0,
            );
        }
    }
}

/// Barra de HP: moldura do layout (`*_hp_base`) + preenchimento na fração de HP dentro dela.
/// [H] a peça `*_hp_bar` do layout não casa com a moldura sem o posicionamento do script; pinta-se a barra.
fn hp_bar(l: &Layout, t: &mut Target, base: &str, _bar: &str, off: (f32, f32), hp: i32, max: i32) {
    let Ok(mut b) = Anim::new(l, base, true) else {
        return;
    };
    b.offset = off;
    const KEY: u32 = 0x00ff_00ff;
    let mut tmp = vec![KEY; t.width * t.height];
    let mut tt = Target {
        pixels: &mut tmp,
        width: t.width,
        height: t.height,
    };
    l.draw(&mut tt, &[&b]);
    let (mut x0, mut x1, mut y0, mut y1) = (usize::MAX, 0, usize::MAX, 0);
    // só pixels quase opacos (os semitransparentes ficam puxados para a cor-chave magenta)
    let opaque = |p: u32| {
        let (r, g, b) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
        !(r > 0x90 && b > 0x90 && g < 0x60)
    };
    for (i, p) in tmp.iter().enumerate() {
        if *p != KEY && opaque(*p) {
            x0 = x0.min(i % t.width);
            x1 = x1.max(i % t.width);
            y0 = y0.min(i / t.width);
            y1 = y1.max(i / t.width);
        }
    }
    l.draw(t, &[&b]);
    if x0 >= x1 || y0 >= y1 || x1 - x0 < 10 {
        return;
    }
    let frac = (hp.max(0) as f32 / max.max(1) as f32).clamp(0.0, 1.0);
    let color = if frac > 0.5 {
        0x40_e050
    } else if frac > 0.2 {
        0xf0_c030
    } else {
        0xf0_4030
    };
    let (ix0, ix1) = (x0 + 4, x1.saturating_sub(4));
    let (iy0, iy1) = (y0 + 2, y1.saturating_sub(2).max(y0 + 2));
    let cut = ix0 + ((ix1 - ix0) as f32 * frac) as usize;
    for y in iy0..=iy1 {
        for x in ix0..ix1 {
            t.pixels[y * t.width + x] = if x < cut { color } else { 0x20_2020 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::yokai::hash;

    fn setup() -> (Archive, Rc<Db>) {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let db = Rc::new(Db::load(&fa).unwrap());
        (fa, db)
    }

    /// Batalha do tutorial (fase 23): Cadin nível 5 contra o Buhu nível 2 de `ev01_btl0010`, sem o jogador
    /// fazer nada além de usar o Soultimate quando carregar. Tem que vencer.
    #[test]
    fn tutorial_battle_is_won() {
        let (fa, db) = setup();
        let cadin = Member::new(&db, hash("para_y311"), 5);
        let enemies = db.encounter("ev01_btl0010");
        let mut b = Battle::new(&fa, db.clone(), &[cadin], &enemies, "t100b02", false);
        b.seed(7);
        let mut input = Input::headless();
        for t in 0..20000 {
            input.set(Button::A, t % 20 == 0);
            b.tick(Some(&input));
            if b.outcome.is_some() {
                break;
            }
        }
        println!("log: {:?}", b.log);
        match b.outcome {
            Some(Outcome::Won { exp, .. }) => assert!(exp > 0),
            o => panic!("resultado {o:?}"),
        }
        assert!(b.result_party[0].exp > 0 || b.result_party[0].level > 5);
    }

    #[test]
    #[ignore]
    fn battle_snapshot() {
        let (fa, db) = setup();
        let party = [
            Member::new(&db, hash("para_y311"), 5),
            Member::new(&db, 0x8443_d22d_u32 as i32, 5),
        ];
        let mut b = Battle::new(
            &fa,
            db.clone(),
            &party,
            &db.encounter("ev01_btl0010"),
            "t100b02",
            true,
        );
        if let Some(st) = &b.stage {
            let (lo, hi, pieces) = st.bounds();
            println!("cenário {lo:?}..{hi:?}");
            for p in pieces {
                println!("  peça {:?} r {:.1}", p.0, p.1);
            }
        }
        let mut screens = Screens::new();
        for _ in 0..60 {
            b.tick(None);
        }
        let f = b.draw(2, &mut screens);
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../work/battle");
        std::fs::create_dir_all(&dir).unwrap();
        let raw: Vec<u8> = f
            .color
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
            .collect();
        std::fs::write(dir.join(format!("top_{}x{}.rgba", f.width, f.height)), raw).unwrap();
        let raw: Vec<u8> = screens
            .bottom
            .iter()
            .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
            .collect();
        std::fs::write(dir.join("bottom_320x240.rgba"), raw).unwrap();
    }
}

#[cfg(test)]
mod mesh_probe {
    #[test]
    #[ignore]
    fn yokai_meshes() {
        let romfs = crate::romfs::locate(None).unwrap();
        let fa = crate::assets::arc0::Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        for m in ["y311000", "y321000"] {
            let c = crate::character::Character::load(
                &fa.read(&format!("data/character/{m}/{m}_p00.xc")).unwrap(),
            )
            .unwrap();
            for mesh in &c.meshes {
                let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                for p in &mesh.pos {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                    }
                }
                println!(
                    "{m} {} tex {:?} verts {} bbox {:?}..{:?}",
                    mesh.material,
                    mesh.texture,
                    mesh.pos.len(),
                    lo,
                    hi
                );
            }
            println!(
                "{m} texturas {}",
                c.textures.iter().filter(|t| t.is_some()).count()
            );
        }
    }
}
