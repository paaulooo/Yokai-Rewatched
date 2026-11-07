//! Configurações do port (não confundir com as opções do jogo no `ConfigMenu`, que ficam no save):
//! escala de renderização 3D, tamanho da janela e volumes. Persistidas em `settings.ini`
//! (`YW_SETTINGS` sobrescreve o caminho; padrão `%APPDATA%/yw1/settings.ini`, senão a pasta atual).
//! Formato: linhas `chave = valor`; `#` comenta. Chaves desconhecidas são ignoradas.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Multiplicador da resolução interna do 3D: 1 = 400×240 (como o 3DS), 2 = 800×480, 3 = 1200×720...
    pub render_scale: usize,
    /// Tamanho inicial da janela em múltiplos de 400×480.
    pub window_scale: usize,
    /// 0..1
    pub music_volume: f32,
    /// 0..1 (efeitos sonoros: ainda não há `.bcsar`; reservado)
    pub sfx_volume: f32,
    /// Giro de câmera no campo (L/R): sensibilidade em graus por quadro.
    pub camera_speed: f32,
    /// Opções do PRÓPRIO jogo (tela Settings, `op14150/op14151`): 0 volume da música (0..5), 1 volume dos
    /// efeitos (0..5), 2 e 3 AUTO/FIXED, 4 ON/OFF. Padrões do `Menu_Reset` do ConfigMenu: 4 4 0 0 0.
    pub game: [i32; 5],
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            render_scale: 2,
            window_scale: 2,
            music_volume: 1.0,
            sfx_volume: 1.0,
            camera_speed: 1.5,
            game: [4, 4, 0, 0, 0],
        }
    }
}

pub const MAX_RENDER_SCALE: usize = 6;

fn path() -> PathBuf {
    if let Some(p) = std::env::var_os("YW_SETTINGS") {
        return p.into();
    }
    match std::env::var_os("APPDATA") {
        Some(a) => PathBuf::from(a).join("yw1").join("settings.ini"),
        None => PathBuf::from("yw1_settings.ini"),
    }
}

impl Settings {
    /// Volume final da música: mestre do port × opção do jogo (0..5).
    pub fn bgm_volume(&self) -> f32 {
        self.music_volume * (self.game[0].clamp(0, 5) as f32 / 5.0)
    }

    pub fn load() -> Settings {
        std::fs::read_to_string(path())
            .map_or_else(|_| Settings::default(), |t| Settings::parse(&t))
    }

    pub fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "render_scale" => {
                    if let Ok(n) = v.parse::<usize>() {
                        s.render_scale = n.clamp(1, MAX_RENDER_SCALE);
                    }
                }
                "window_scale" => {
                    if let Ok(n) = v.parse::<usize>() {
                        s.window_scale = n.clamp(1, 4);
                    }
                }
                "music_volume" => {
                    if let Ok(n) = v.parse::<f32>() {
                        s.music_volume = n.clamp(0.0, 1.0);
                    }
                }
                "sfx_volume" => {
                    if let Ok(n) = v.parse::<f32>() {
                        s.sfx_volume = n.clamp(0.0, 1.0);
                    }
                }
                "game_options" => {
                    let v: Vec<i32> = v
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if v.len() == 5 {
                        s.game.copy_from_slice(&v);
                    }
                }
                "camera_speed" => {
                    if let Ok(n) = v.parse::<f32>() {
                        s.camera_speed = n.clamp(0.1, 6.0);
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn to_text(&self) -> String {
        format!(
            "# Configurações do port yw1 (editáveis; F1/F2 mudam render_scale durante o jogo)\n\
             render_scale = {}\nwindow_scale = {}\nmusic_volume = {}\nsfx_volume = {}\ncamera_speed = {}\n\
             # opções do jogo (tela Settings): música efeitos op2 op3 op4\ngame_options = {} {} {} {} {}\n",
            self.render_scale, self.window_scale, self.music_volume, self.sfx_volume, self.camera_speed,
            self.game[0], self.game[1], self.game[2], self.game[3], self.game[4]
        )
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&p, self.to_text()) {
            eprintln!("[yw1] não foi possível salvar {}: {e}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_clamp() {
        let s = Settings {
            render_scale: 3,
            music_volume: 0.5,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&s.to_text()), s);
        let c = Settings::parse("render_scale = 99\nmusic_volume = -1 # x\nlixo\n");
        assert_eq!(c.render_scale, MAX_RENDER_SCALE);
        assert_eq!(c.music_volume, 0.0);
    }
}
