//! Saves do port: 3 slots ("diários") como no jogo, em `%APPDATA%/yw1/saves/slotN.ini` (`YW_SAVES` sobrescreve
//! a pasta). NÃO é o formato do save do 3DS (não decifrado): guarda o que o port já usa — nome, tipo
//! (1 menino / 2 menina), mapa, posição, ângulo, tempo de jogo — e responde às consultas do SaveLoadMenu:
//! `op12054(slot, campo)` (0 = bits: 1 usado, 2 menina, 4 tem relógio, 256 zerado, 1024 Lens; 2 = mapa;
//! 3 = rank; 4+n = Yo-kai n; 10 = segundos de jogo) e `op12053(slot)` = nome.

use std::path::PathBuf;

pub const SLOTS: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    pub name: String,
    pub player_type: i32,
    pub map: String,
    pub pos: [f32; 3],
    pub yaw: f32,
    pub play_seconds: u64,
    pub has_watch: bool,
    /// Prólogo (ev01_0003 → ev01_0020) já visto.
    pub prologue_done: bool,
    /// Data/hora do PC em que o diário foi gravado: ano, mês, dia, hora, minuto (`op12055`).
    pub saved_at: [u16; 5],
    /// Passo do capítulo 1 depois do prólogo (`chapter1::STEPS`); `chapter1::DONE` = jogo livre.
    pub chapter: u32,
    /// Time de Yo-kai na ordem do anel: (param, nível, experiência).
    pub party: Vec<(i32, u32, u32)>,
    pub money: u32,
}

impl Save {
    pub fn new_game(name: &str, player_type: i32) -> Save {
        let girl = player_type == 2;
        Save {
            name: name.to_string(),
            player_type,
            // Phase1/2 do capítulo 1: SetPosLeaderChara(-10,0,15.76,90) no quarto do menino; menina (2.5,0,17.36)
            map: if girl { "t101i04" } else { "t101i02" }.into(),
            pos: if girl {
                [2.5, 0.0, 17.36]
            } else {
                [-10.0, 0.0, 15.76]
            },
            yaw: 90.0,
            play_seconds: 0,
            has_watch: false,
            prologue_done: false,
            saved_at: crate::clock::local_now().save_fields(),
            chapter: 0,
            party: Vec::new(),
            money: 0,
        }
    }

    /// Bits do campo 0 de `op12054`.
    pub fn flags(&self) -> i32 {
        1 | if self.player_type == 2 { 2 } else { 0 } | if self.has_watch { 4 } else { 0 }
    }

    pub fn to_text(&self) -> String {
        format!(
            "# save do port yw1 (não é o formato do 3DS)\nname = {}\ntype = {}\nmap = {}\npos = {} {} {}\nyaw = {}\nplay_seconds = {}\nhas_watch = {}\nprologue_done = {}\nsaved_at = {} {} {} {} {}\nchapter = {}\nmoney = {}\nparty = {}\n",
            self.name, self.player_type, self.map, self.pos[0], self.pos[1], self.pos[2], self.yaw,
            self.play_seconds, self.has_watch as u8, self.prologue_done as u8,
            self.saved_at[0], self.saved_at[1], self.saved_at[2], self.saved_at[3], self.saved_at[4],
            self.chapter,
            self.money,
            self.party
                .iter()
                .map(|(p, l, e)| format!("{p}:{l}:{e}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }

    pub fn parse(text: &str) -> Option<Save> {
        let mut s = Save::new_game("", 1);
        let mut named = false;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "name" => {
                    s.name = v.to_string();
                    named = true;
                }
                "type" => s.player_type = v.parse().ok()?,
                "map" => s.map = v.to_string(),
                "pos" => {
                    let p: Vec<f32> = v
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if p.len() == 3 {
                        s.pos = [p[0], p[1], p[2]];
                    }
                }
                "yaw" => s.yaw = v.parse().unwrap_or(0.0),
                "play_seconds" => s.play_seconds = v.parse().unwrap_or(0),
                "has_watch" => s.has_watch = v == "1",
                "prologue_done" => s.prologue_done = v == "1",
                "chapter" => s.chapter = v.parse().unwrap_or(0),
                "money" => s.money = v.parse().unwrap_or(0),
                "party" => {
                    s.party = v
                        .split_whitespace()
                        .filter_map(|t| {
                            let mut it = t.split(':');
                            Some((
                                it.next()?.parse().ok()?,
                                it.next()?.parse().ok()?,
                                it.next()?.parse().ok()?,
                            ))
                        })
                        .collect();
                }
                "saved_at" => {
                    let p: Vec<u16> = v
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if p.len() == 5 {
                        s.saved_at.copy_from_slice(&p);
                    }
                }
                _ => {}
            }
        }
        named.then_some(s)
    }
}

fn dir() -> PathBuf {
    if let Some(p) = std::env::var_os("YW_SAVES") {
        return p.into();
    }
    match std::env::var_os("APPDATA") {
        Some(a) => PathBuf::from(a).join("yw1").join("saves"),
        None => PathBuf::from("yw1_saves"),
    }
}

/// Slots 1..=3.
pub fn path(slot: usize) -> PathBuf {
    dir().join(format!("slot{slot}.ini"))
}

pub fn load(slot: usize) -> Option<Save> {
    std::fs::read_to_string(path(slot))
        .ok()
        .and_then(|t| Save::parse(&t))
}

pub fn store(slot: usize, s: &Save) {
    let p = path(slot);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // a data do diário vem do relógio do PC
    let mut s = s.clone();
    s.saved_at = crate::clock::local_now().save_fields();
    // flags de progresso e variáveis (estado global do jogo) vão junto no mesmo arquivo
    let flags = crate::flags::snapshot()
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!(
        "{}flags = {flags}
",
        s.to_text()
    );
    match std::fs::write(&p, text) {
        Ok(()) => eprintln!(
            "[yw1] salvo no diário {slot}: {} em {} {:?}",
            s.name, s.map, s.pos
        ),
        Err(e) => eprintln!("[yw1] falha ao salvar {}: {e}", p.display()),
    }
}

/// Carrega as flags e variáveis gravadas no diário `slot` para o estado global (Continue).
pub fn restore_flags(slot: usize) {
    let Ok(t) = std::fs::read_to_string(path(slot)) else {
        crate::flags::reset();
        return;
    };
    let mut items = Vec::new();
    for line in t.lines() {
        if let Some(("flags ", v)) = line.split_once('=') {
            items = v
                .split_whitespace()
                .filter_map(|x| x.split_once(':'))
                .filter_map(|(k, v)| Some((k.parse().ok()?, v.parse().ok()?)))
                .collect();
        }
    }
    crate::flags::restore(&items);
}

pub fn any() -> bool {
    (1..=SLOTS).any(|i| load(i).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut s = Save::new_game("Nate", 1);
        s.pos = [1.5, 0.0, -3.25];
        s.play_seconds = 125;
        s.chapter = 7;
        s.money = 30;
        s.party = vec![(-2075930067, 3, 12), (-1351712274, 1, 0)];
        assert_eq!(Save::parse(&s.to_text()), Some(s.clone()));
        assert_eq!(s.flags(), 1);
        assert_eq!(Save::new_game("Katie", 2).flags(), 3);
    }
}
