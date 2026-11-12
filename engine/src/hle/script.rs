//! Roteiro de entrada para rodar sem janela (`YW_INPUT`): aperta botões e toca a tela em instantes do relógio do
//! console (VBlanks / 59,83 Hz, não o relógio do PC), para navegar pelo jogo de forma repetível e comparar telas.
//!
//! Formato: eventos separados por `;` → `TEMPO:AÇÃO[/DURAÇÃO]` (segundos; duração padrão 0,15 s).
//! - AÇÃO = botões unidos por `+`: `a b x y l r start select up down left right`
//! - ou `touch(x,y)` na tela de baixo (0–319, 0–239)
//! - ou `stick(x,y)` no direcional analógico (−1…1; y positivo = para cima), como o teclado/gamepad da janela
//! - `TEMPO` pode ser `+N` = N segundos depois do evento anterior
//!
//! Ex.: `YW_INPUT="20:start; +2:a; +1.5:down/0.3; +1:touch(160,200)"`.

use super::services::hid;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at: f64,
    pub dur: f64,
    pub buttons: u32,
    pub touch: Option<(u32, u32)>,
    pub stick: Option<(f32, f32)>,
}

/// Estado de entrada num instante: botões, toque, analógico.
pub type State = (u32, Option<(u32, u32)>, Option<(f32, f32)>);

fn button(name: &str) -> Option<u32> {
    Some(match name {
        "a" => 1 << 0,
        "b" => 1 << 1,
        "select" => 1 << 2,
        "start" => 1 << 3,
        "right" => 1 << 4,
        "left" => 1 << 5,
        "up" => 1 << 6,
        "down" => 1 << 7,
        "r" => 1 << 8,
        "l" => 1 << 9,
        "x" => 1 << 10,
        "y" => 1 << 11,
        _ => return None,
    })
}

pub fn parse(s: &str) -> Result<Vec<Event>, String> {
    let mut out = Vec::new();
    let mut last = 0.0;
    for part in s.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let (t, rest) = part.split_once(':').ok_or_else(|| format!("evento sem ':' — {part:?}"))?;
        let t = t.trim();
        let at = if let Some(d) = t.strip_prefix('+') {
            last + d.parse::<f64>().map_err(|e| format!("{part:?}: {e}"))?
        } else {
            t.parse::<f64>().map_err(|e| format!("{part:?}: {e}"))?
        };
        let (act, dur) = match rest.rsplit_once('/') {
            Some((a, d)) => (a.trim(), d.trim().parse::<f64>().map_err(|e| format!("{part:?}: {e}"))?),
            None => (rest.trim(), 0.15),
        };
        let act = act.to_ascii_lowercase();
        let mut ev = Event { at, dur, buttons: 0, touch: None, stick: None };
        if let Some(xy) = act.strip_prefix("touch(").and_then(|r| r.strip_suffix(')')) {
            let (x, y) = xy.split_once(',').ok_or_else(|| format!("{part:?}: touch(x,y)"))?;
            let x: u32 = x.trim().parse().map_err(|e| format!("{part:?}: {e}"))?;
            let y: u32 = y.trim().parse().map_err(|e| format!("{part:?}: {e}"))?;
            ev.touch = Some((x.min(319), y.min(239)));
        } else if let Some(xy) = act.strip_prefix("stick(").and_then(|r| r.strip_suffix(')')) {
            let (x, y) = xy.split_once(',').ok_or_else(|| format!("{part:?}: stick(x,y)"))?;
            let x: f32 = x.trim().parse().map_err(|e| format!("{part:?}: {e}"))?;
            let y: f32 = y.trim().parse().map_err(|e| format!("{part:?}: {e}"))?;
            ev.stick = Some((x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0)));
        } else {
            for b in act.split('+').map(str::trim) {
                ev.buttons |= button(b).ok_or_else(|| format!("{part:?}: botão desconhecido {b:?}"))?;
            }
        }
        last = at;
        out.push(ev);
    }
    Ok(out)
}

/// Estado de entrada no instante `t` (segundos do console): botões, toque e analógico dos eventos ativos.
pub fn state_at(events: &[Event], t: f64) -> State {
    let mut b = 0;
    let mut touch = None;
    let mut stick = None;
    for e in events.iter().filter(|e| t >= e.at && t < e.at + e.dur) {
        b |= e.buttons;
        if e.touch.is_some() {
            touch = e.touch;
        }
        if e.stick.is_some() {
            stick = e.stick;
        }
    }
    (b, touch, stick)
}

/// Analógico → (palavra crua x|y<<16 em ±156, bits de direção 28–31), como `window.rs`.
fn stick_word(s: Option<(f32, f32)>) -> (u32, u32) {
    let Some((x, y)) = s else { return (0, 0) };
    let mut b = 0;
    if x > 0.4 {
        b |= 1 << 28;
    }
    if x < -0.4 {
        b |= 1 << 29;
    }
    if y > 0.4 {
        b |= 1 << 30;
    }
    if y < -0.4 {
        b |= 1 << 31;
    }
    let cx = (x * 156.0) as i16 as u16 as u32;
    let cy = (y * 156.0) as i16 as u16 as u32;
    (cx | cy << 16, b)
}

/// Entrada "ao vivo" (`YW_INPUT_FILE`): arquivo de texto com UMA linha de itens separados por espaço
/// (`a`, `up`, `stick(0,1)`, `touch(160,200)`…), relido a cada poucos ms. Linha vazia = nada apertado. Serve para
/// controlar o jogo em malha fechada a partir de um programa externo que olha os quadros gravados.
fn live_state(path: &str) -> State {
    let Ok(txt) = std::fs::read_to_string(path) else { return (0, None, None) };
    let ev: String = txt
        .split_whitespace()
        .map(|t| format!("0:{t}/1000000000"))
        .collect::<Vec<_>>()
        .join(";");
    match parse(&ev) {
        Ok(e) => state_at(&e, 1.0),
        Err(_) => (0, None, None), // arquivo lido pela metade: ignora este instante
    }
}

/// Thread que aplica o roteiro de `YW_INPUT` (se houver) seguindo o contador de VBlanks, somado à entrada ao vivo
/// de `YW_INPUT_FILE` (se houver).
/// Há um roteiro (`YW_INPUT`/`YW_INPUT_FILE`) mandando na entrada: a janela não sobrescreve botões/toque.
pub static ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn start_from_env() {
    let script = std::env::var("YW_INPUT").ok();
    let live = std::env::var("YW_INPUT_FILE").ok();
    if script.is_none() && live.is_none() {
        return;
    }
    let events = match script.as_deref().map(parse).unwrap_or(Ok(Vec::new())) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[input] YW_INPUT inválido: {e}");
            return;
        }
    };
    eprintln!("[input] roteiro com {} eventos{}", events.len(), if live.is_some() { " + entrada ao vivo" } else { "" });
    ACTIVE.store(true, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("input-script".into())
        .spawn(move || {
            let mut prev: State = (0, None, None);
            loop {
                let t = super::VBLANKS.load(Ordering::Relaxed) as f64 / super::vblank_hz();
                let mut now = state_at(&events, t);
                if let Some(p) = &live {
                    let l = live_state(p);
                    now = (now.0 | l.0, l.1.or(now.1), l.2.or(now.2));
                }
                if now != prev {
                    // mesma codificação da janela (window.rs): toque = x | y << 9 | válido << 31, e bit 20 nos botões
                    hid::TOUCH.store(now.1.map_or(0, |(x, y)| x | y << 9 | 1 << 31), Ordering::Relaxed);
                    let (sw, sb) = stick_word(now.2);
                    hid::STICK.store(sw, Ordering::Relaxed);
                    hid::BUTTONS.store(now.0 | sb | if now.1.is_some() { 1 << 20 } else { 0 }, Ordering::Relaxed);
                    if now.0 != 0 || now.1.is_some() || now.2.is_some() {
                        eprintln!("[input] {t:.2}s botões {:#x} toque {:?} analógico {:?}", now.0, now.1, now.2);
                    }
                    prev = now;
                }
                std::thread::sleep(std::time::Duration::from_millis(4));
            }
        })
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_script_parses() {
        let e = parse("20:start; +2:a; +1.5:down+b/0.3; 30:touch(160, 200)").unwrap();
        assert_eq!(e.len(), 4);
        assert_eq!((e[0].at, e[0].buttons), (20.0, 8));
        assert_eq!(e[1].at, 22.0);
        assert_eq!((e[2].at, e[2].dur, e[2].buttons), (23.5, 0.3, 0x82));
        assert_eq!(e[3].touch, Some((160, 200)));
        assert_eq!(state_at(&e, 23.6), (0x82, None, None));
        assert_eq!(state_at(&e, 23.9), (0, None, None));
        let s = parse("1:stick(0.5,-1)/2").unwrap();
        assert_eq!(state_at(&s, 2.0).2, Some((0.5, -1.0)));
        assert_eq!(stick_word(Some((0.5, -1.0))).1, (1 << 28) | (1 << 31));
        assert!(parse("1:zz").is_err());
    }
}
