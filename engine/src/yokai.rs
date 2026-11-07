//! Banco de dados dos Yo-kai (do RomFS do usuário) e o Yo-kai como membro do time.
//!
//! Tabelas (layout dos registros conferido com o Albatross de Tiniifan — github.com/Tiniifan/Albatross,
//! `Games/YW1/Logic/*.cs` — e validado aqui contra os dados: nomes, modelos e golpes batem com o jogo):
//! - `data/res/character/chara_param_0.02.cfg.bin` `CHARA_PARAM_INFO`: 0 param, 1 base, 2 tribo, 3..7 HP/FOR/ESP/DEF/VEL,
//!   9/11/13/15 golpes (ataque/técnica/inspirar/guarda) com 10/12/14/16 = chance (%), 18..23 multiplicadores
//!   de elemento (fogo, gelo, terra, raio, água, vento), 24 Soultimate, 28 habilidade, 29 dinheiro, 30 experiência.
//! - `data/res/character/chara_base_0.04j.cfg.bin`: 0 base, 1 prefixo (0 c, 5 x, 6 y, 7 z), 2 número, 3 variante, 4 nome.
//! - `data/res/text/chara_text_en.cfg.bin` `NOUN_INFO`: 0 hash, 5 nome.
//! - `data/res/battle/battle_command_0.02f.cfg.bin` `BATTLE_COMMAND_INFO`: 0 hash, 1 nome (skill/battle_text),
//!   2 categoria (3 golpe, 2 guarda, 5 Soultimate), 3 tipo (0 ataque, 1 técnica, 2 inspirar, 5 guarda), 6 nº de acertos [H].
//! - `data/res/battle/common_enc_0.02m.cfg.bin`: `ENCOUNT_TABLE(crc32(nome), i1..i4)` → `ENCOUNT_CHARA(param, nível, …)`.
//!
//! Crescimento por nível, curva de experiência e fórmula de dano ainda não foram lidos do `code.bin`: estão
//! marcados [H] e concentrados em funções pequenas para trocar quando forem decifrados.

use crate::assets::arc0::Archive;
use crate::assets::cfgbin::{self, Entry, Param};
use crate::assets::{crc32, Result};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Attack,
    Technique,
    Inspirit,
    Guard,
    Soultimate,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub name: String,
    pub kind: Kind,
    pub hits: u32,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // tribo/elementos: ainda sem uso na batalha [H]
pub struct Species {
    pub param: i32,
    pub name: String,
    /// Modelo em `data/character/<model>/` (ex.: y311000).
    pub model: String,
    pub tribe: usize,
    /// Rank (0 E, 1 D, 2 C, 3 B, 4 A, 5 S) — `chara_base` campo 11. O relógio só acha Yo-kai até o rank dele.
    pub rank: i32,
    /// HP, força, espírito, defesa, velocidade no nível 1 [H: interpretados como base do nível 1].
    pub base: [i32; 5],
    /// Golpes: ataque, técnica, inspirar, guarda (hash) e chance de cada um (%).
    pub moves: [(i32, i32); 4],
    pub soultimate: i32,
    /// Multiplicador recebido por elemento: fogo, gelo, terra, raio, água, vento.
    pub resist: [f32; 6],
    pub money: i32,
    pub exp: i32,
}

pub struct Db {
    pub species: HashMap<i32, Species>,
    pub commands: HashMap<i32, Command>,
    encounters: HashMap<i32, Vec<(i32, u32)>>,
}

fn int(e: &Entry, i: usize) -> i32 {
    match e.params.get(i) {
        Some(Param::Int(v)) => *v,
        Some(Param::Float(f)) => *f as i32,
        _ => 0,
    }
}

fn float(e: &Entry, i: usize) -> f32 {
    match e.params.get(i) {
        Some(Param::Float(f)) => *f,
        Some(Param::Int(v)) => *v as f32,
        _ => 0.0,
    }
}

fn text(e: &Entry, i: usize) -> Option<String> {
    match e.params.get(i) {
        Some(Param::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn variant(x: i32) -> String {
    match x {
        0..=9 => format!("0{x}0"),
        10..=99 => format!("{x}0"),
        _ => x.to_string().chars().rev().collect(),
    }
}

pub fn hash(name: &str) -> i32 {
    crc32(name.as_bytes()) as i32
}

impl Db {
    pub fn load(fa: &Archive) -> Result<Db> {
        let read = |p: &str| -> Result<Vec<Entry>> { cfgbin::parse(&fa.read(p)?) };
        let mut names = HashMap::new();
        for e in read("data/res/text/chara_text_en.cfg.bin")? {
            if e.name == "NOUN_INFO" {
                if let Some(t) = text(&e, 5) {
                    names.insert(int(&e, 0), t);
                }
            }
        }
        let mut words = HashMap::new();
        for p in [
            "data/res/text/skill_text_en.cfg.bin",
            "data/res/text/battle_text_en.cfg.bin",
        ] {
            for e in read(p).unwrap_or_default() {
                if e.name == "NOUN_INFO" {
                    if let Some(t) = text(&e, 5) {
                        words.entry(int(&e, 0)).or_insert(t);
                    }
                }
            }
        }
        let mut base = HashMap::new();
        for e in read("data/res/character/chara_base_0.04j.cfg.bin")? {
            if e.name == "CHARA_BASE_INFO" || e.name == "CHARA_BASE_YOKAI_INFO" {
                let prefix = match int(&e, 1) {
                    0 => 'c',
                    5 => 'x',
                    6 => 'y',
                    7 => 'z',
                    _ => '?',
                };
                let model = format!("{prefix}{:03}{}", int(&e, 2), variant(int(&e, 3)));
                let name = names
                    .get(&int(&e, 4))
                    .cloned()
                    .unwrap_or_else(|| model.clone());
                base.insert(int(&e, 0), (model, name, int(&e, 11)));
            }
        }
        let mut species = HashMap::new();
        for e in read("data/res/character/chara_param_0.02.cfg.bin")? {
            if e.name != "CHARA_PARAM_INFO" {
                continue;
            }
            let (model, name, rank) = base.get(&int(&e, 1)).cloned().unwrap_or_default();
            let p = int(&e, 0);
            species.insert(
                p,
                Species {
                    param: p,
                    name,
                    model,
                    tribe: int(&e, 2).clamp(0, 10) as usize,
                    rank,
                    base: [int(&e, 3), int(&e, 4), int(&e, 5), int(&e, 6), int(&e, 7)],
                    moves: [
                        (int(&e, 9), int(&e, 10)),
                        (int(&e, 11), int(&e, 12)),
                        (int(&e, 13), int(&e, 14)),
                        (int(&e, 15), int(&e, 16)),
                    ],
                    soultimate: int(&e, 24),
                    resist: [
                        float(&e, 18),
                        float(&e, 19),
                        float(&e, 20),
                        float(&e, 21),
                        float(&e, 22),
                        float(&e, 23),
                    ],
                    money: int(&e, 29),
                    exp: int(&e, 30),
                },
            );
        }
        let mut commands = HashMap::new();
        for e in read("data/res/battle/battle_command_0.02f.cfg.bin")? {
            if e.name != "BATTLE_COMMAND_INFO" {
                continue;
            }
            let kind = match (int(&e, 2), int(&e, 3)) {
                (5, _) => Kind::Soultimate,
                (_, 0) => Kind::Attack,
                (_, 1) => Kind::Technique,
                (_, 2) => Kind::Inspirit,
                _ => Kind::Guard,
            };
            let name = words
                .get(&int(&e, 1))
                .cloned()
                .unwrap_or_else(|| "???".into());
            commands.insert(
                int(&e, 0),
                Command {
                    name: if kind == Kind::Guard {
                        "Guard".into()
                    } else {
                        name
                    },
                    kind,
                    hits: float(&e, 6).max(1.0) as u32,
                },
            );
        }
        let mut encounters = HashMap::new();
        let enc = read("data/res/battle/common_enc_0.02m.cfg.bin")?;
        let charas: Vec<&Entry> = enc.iter().filter(|e| e.name == "ENCOUNT_CHARA").collect();
        for e in enc.iter().filter(|e| e.name == "ENCOUNT_TABLE") {
            let list = (1..5)
                .map(|i| int(e, i))
                .filter(|&o| o >= 0)
                .filter_map(|o| charas.get(o as usize))
                .map(|c| (int(c, 0), int(c, 1).max(1) as u32))
                .collect();
            encounters.insert(int(e, 0), list);
        }
        Ok(Db {
            species,
            commands,
            encounters,
        })
    }

    /// Encontros na natureza de um mapa: tabelas `data/res/map/<m>/<m>_enc_*.cfg.bin` (somadas às comuns) e
    /// a configuração do relógio `<m>_watchmap*.cfg.bin` (`WM_YOKAI_PRESET_*`). None se o mapa não tiver Yo-kai.
    pub fn load_map(&mut self, fa: &Archive, map: &str) -> Option<Watchmap> {
        let enc = ["0.02m", "0.02l", "0.01"]
            .iter()
            .find_map(|v| {
                fa.read(&format!("data/res/map/{map}/{map}_enc_{v}.cfg.bin"))
                    .ok()
            })
            .and_then(|b| cfgbin::parse(&b).ok())?;
        let charas: Vec<&Entry> = enc.iter().filter(|e| e.name == "ENCOUNT_CHARA").collect();
        for e in enc.iter().filter(|e| e.name == "ENCOUNT_TABLE") {
            let list = (1..5)
                .map(|i| int(e, i))
                .filter(|&o| o >= 0)
                .filter_map(|o| charas.get(o as usize))
                .map(|c| (int(c, 0), int(c, 1).max(1) as u32))
                .collect();
            self.encounters.insert(int(e, 0), list);
        }
        let wm = ["_0.02j", ""]
            .iter()
            .find_map(|v| {
                fa.read(&format!("data/res/map/{map}/{map}_watchmap{v}.cfg.bin"))
                    .ok()
            })
            .and_then(|b| cfgbin::parse(&b).ok())?;
        let mut w = Watchmap::default();
        for e in &wm {
            match e.name.as_str() {
                "WM_YOKAI_PRESET_CONFIG" => {
                    w.configs.push([int(e, 0), int(e, 1), int(e, 2), int(e, 3)])
                }
                "WM_YOKAI_PRESET_BLOCK" => w.blocks.push([int(e, 0), int(e, 1), int(e, 2)]),
                "WM_YOKAI_PRESET_DATA" => w.data.push((int(e, 0), int(e, 1))),
                _ => {}
            }
        }
        (!w.data.is_empty()).then_some(w)
    }

    /// Inimigos (param, nível) de uma batalha pelo nome (ex.: `ev01_btl0010`).
    pub fn encounter(&self, name: &str) -> Vec<(i32, u32)> {
        self.encounters
            .get(&hash(name))
            .cloned()
            .unwrap_or_default()
    }

    pub fn command(&self, h: i32) -> Option<&Command> {
        self.commands.get(&h)
    }

    pub fn encounter_by_hash(&self, h: i32) -> Vec<(i32, u32)> {
        self.encounters.get(&h).cloned().unwrap_or_default()
    }
}

/// Relógio de Yo-kai do mapa (`WM_YOKAI_PRESET_*`):
/// - CONFIG(período, estágio, 1º bloco, nº de blocos): período 0 = dia, 3 = noite (o mesmo valor de `op14041(3)`);
///   estágio 0/1 [H: progresso da história — 0 no começo do jogo].
/// - BLOCK(ponto 1..8, 1º dado, nº de dados): os 8 pontos de busca do mapa.
/// - DATA(tabela de encontro, peso).
#[derive(Default, Debug)]
pub struct Watchmap {
    pub configs: Vec<[i32; 4]>,
    pub blocks: Vec<[i32; 3]>,
    pub data: Vec<(i32, i32)>,
}

impl Watchmap {
    /// Tabela de encontro sorteada para (período, estágio, ponto 1..8); `roll` em [0, 1). Só entram tabelas cujos
    /// Yo-kai sejam todos de rank ≤ `watch_rank` (o relógio não enxerga os de rank maior).
    pub fn pick(
        &self,
        db: &Db,
        watch_rank: i32,
        period: i32,
        stage: i32,
        spot: i32,
        roll: f32,
    ) -> Option<i32> {
        let cfg = self
            .configs
            .iter()
            .find(|c| c[0] == period && c[1] == stage)
            .or_else(|| self.configs.iter().find(|c| c[0] == period))
            .or(self.configs.first())?;
        let blocks = self
            .blocks
            .get(cfg[2].max(0) as usize..(cfg[2] + cfg[3]).max(0) as usize)?;
        let b = blocks.iter().find(|b| b[0] == spot).or(blocks.first())?;
        let data: Vec<(i32, i32)> = self
            .data
            .get(b[1].max(0) as usize..(b[1] + b[2]).max(0) as usize)?
            .iter()
            .copied()
            .filter(|d| {
                let list = db.encounter_by_hash(d.0);
                !list.is_empty()
                    && list
                        .iter()
                        .all(|(p, _)| db.species.get(p).is_some_and(|s| s.rank <= watch_rank))
            })
            .collect();
        let total: i32 = data.iter().map(|d| d.1.max(0)).sum();
        if total <= 0 {
            return data.first().map(|d| d.0);
        }
        let mut r = (roll * total as f32) as i32;
        for d in &data {
            if r < d.1.max(0) {
                return Some(d.0);
            }
            r -= d.1.max(0);
        }
        data.last().map(|d| d.0)
    }
}

/// Valor de um atributo no nível `lvl`. [H] base × (1 + 0,08·(nível−1)) até o crescimento ser lido do code.bin.
pub fn stat_at(base: i32, lvl: u32) -> i32 {
    (base as f32 * (1.0 + 0.08 * (lvl.max(1) - 1) as f32)).round() as i32
}

/// Experiência para passar do nível `lvl` ao seguinte. [H]
pub fn exp_to_next(lvl: u32) -> u32 {
    let l = lvl.max(1);
    8 + l * l * 3
}

/// Um Yo-kai do time (ou inimigo) com nível e HP atuais.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub param: i32,
    pub level: u32,
    pub hp: i32,
    pub exp: u32,
}

impl Member {
    pub fn new(db: &Db, param: i32, level: u32) -> Member {
        let mut m = Member {
            param,
            level: level.max(1),
            hp: 0,
            exp: 0,
        };
        m.hp = m.stats(db)[0];
        m
    }

    pub fn species<'a>(&self, db: &'a Db) -> Option<&'a Species> {
        db.species.get(&self.param)
    }

    pub fn stats(&self, db: &Db) -> [i32; 5] {
        let b = self.species(db).map_or([10; 5], |s| s.base);
        b.map(|v| stat_at(v, self.level).max(1))
    }

    pub fn name(&self, db: &Db) -> String {
        self.species(db)
            .map_or_else(|| format!("#{:08x}", self.param), |s| s.name.clone())
    }

    /// Soma experiência; devolve quantos níveis subiu.
    pub fn gain_exp(&mut self, db: &Db, exp: u32) -> u32 {
        let mut ups = 0;
        self.exp += exp;
        while self.level < 99 && self.exp >= exp_to_next(self.level) {
            self.exp -= exp_to_next(self.level);
            let before = self.stats(db)[0];
            self.level += 1;
            self.hp += self.stats(db)[0] - before;
            ups += 1;
        }
        ups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_matches_the_game() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let db = Db::load(&fa).unwrap();
        // aliado do tutorial (fase 23 do capítulo 1) e primeiro Yo-kai do time (fase 27)
        let cadin = &db.species[&hash("para_y311")];
        assert_eq!(
            (cadin.name.as_str(), cadin.model.as_str()),
            ("Cadin", "y311000")
        );
        assert_eq!(db.command(cadin.moves[0].0).unwrap().name, "Stepping Slice");
        assert_eq!(db.command(cadin.soultimate).unwrap().name, "Cicada Cut");
        let buhu = &db.species[&(0x8443_d22d_u32 as i32)];
        assert_eq!(buhu.name, "Buhu");
        assert_eq!(db.command(buhu.soultimate).unwrap().name, "Boohoo Blast");
        // batalha do tutorial: um Buhu nível 2
        let enc = db.encounter("ev01_btl0010");
        assert_eq!(enc.len(), 1);
        assert_eq!(db.species[&enc[0].0].name, "Buhu");
        assert_eq!(enc[0].1, 2);
    }

    #[test]
    fn mount_wildwood_wild_yokai() {
        let romfs = crate::romfs::locate(None).expect("RomFS");
        let fa = Archive::open(&romfs.join("yw1_a.fa")).unwrap();
        let mut db = Db::load(&fa).unwrap();
        let wm = db.load_map(&fa, "t102g00").expect("watchmap de t102g00");
        let mut seen = std::collections::BTreeSet::new();
        for spot in 1..=8 {
            for k in 0..20 {
                let t = wm.pick(&db, 0, 0, 0, spot, k as f32 / 20.0).unwrap();
                for (p, lvl) in db.encounter_by_hash(t) {
                    assert!(lvl <= 20, "nível {lvl} alto demais para o começo");
                    seen.insert(db.species[&p].name.clone());
                }
            }
        }
        println!("Yo-kai de dia no monte: {seen:?}");
        assert!(seen.contains("Buhu"));
        assert_eq!(db.species[&hash("para_y311")].rank, 0);
    }
}
