//! Entrada da janela → estado do HID (`services::hid::{BUTTONS, STICK, TOUCH}`).
//!
//! Codificação (a mesma de `services/hid.rs`): botões A0 B1 Select2 Start3 →4 ←5 ↑6 ↓7 R8 L9 X10 Y11 ZL14 ZR15,
//! toque 20, direções do analógico 28–31; `STICK` = x | y<<16 em ±156 (i16); `TOUCH` = x | y<<9 | 1<<31.

use super::settings::{Botao, Controle, Teclado};
use std::collections::HashSet;
use winit::keyboard::KeyCode;

const TOUCH_BIT: u32 = 1 << 20;
const C_RIGHT: u32 = 1 << 28;
const C_LEFT: u32 = 1 << 29;
const C_UP: u32 = 1 << 30;
const C_DOWN: u32 = 1 << 31;
/// Valor máximo do Circle Pad no HID (±156, como o Citra).
pub const STICK_MAX: f32 = 156.0;

/// Estado combinado de uma amostra de entrada.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub buttons: u32,
    /// analógico em −1…1 (y positivo = para cima)
    pub stick: (f32, f32),
    pub touch: Option<(u16, u16)>,
}

/// Teclas apertadas → botões e analógico.
pub fn from_keys(held: &HashSet<KeyCode>, keys: &Teclado) -> Sample {
    let mut s = Sample::default();
    let on = |b: Botao| keys.get(b).iter().any(|k| held.contains(k));
    for b in Botao::ALL {
        if let Some(bit) = b.bit() {
            if on(b) {
                s.buttons |= bit;
            }
        }
    }
    // teclas opostas se anulam, como num direcional de verdade
    let axis = |plus: bool, minus: bool| plus as i8 as f32 - minus as i8 as f32;
    s.stick = (
        axis(on(Botao::AnalogicoDireita), on(Botao::AnalogicoEsquerda)),
        axis(on(Botao::AnalogicoCima), on(Botao::AnalogicoBaixo)),
    );
    s
}

/// Soma um controle: `pressed(botão)` diz se está apertado; `axes` = analógico esquerdo (−1…1, y para cima).
pub fn add_pad(s: &mut Sample, pad: &Controle, pressed: impl Fn(gilrs::Button) -> bool, axes: (f32, f32)) {
    for b in Botao::PAD {
        if let (Some(p), Some(bit)) = (pad.get(b), b.bit()) {
            if pressed(p) {
                s.buttons |= bit;
            }
        }
    }
    let dz = pad.zona_morta;
    if axes.0.abs() > dz {
        s.stick.0 = axes.0;
    }
    if axes.1.abs() > dz {
        s.stick.1 = axes.1;
    }
}

/// Valores dos três atômicos do HID.
pub fn encode(s: &Sample) -> (u32, u32, u32) {
    let (mut sx, mut sy) = s.stick;
    let len = (sx * sx + sy * sy).sqrt();
    if len > 1.0 {
        sx /= len;
        sy /= len;
    }
    let mut b = s.buttons;
    if sx > 0.4 {
        b |= C_RIGHT;
    }
    if sx < -0.4 {
        b |= C_LEFT;
    }
    if sy > 0.4 {
        b |= C_UP;
    }
    if sy < -0.4 {
        b |= C_DOWN;
    }
    let cx = (sx * STICK_MAX) as i16 as u16 as u32;
    let cy = (sy * STICK_MAX) as i16 as u16 as u32;
    let touch = match s.touch {
        Some((x, y)) => {
            b |= TOUCH_BIT;
            x as u32 | (y as u32) << 9 | 1 << 31
        }
        None => 0,
    };
    (b, cx | cy << 16, touch)
}

/// Nome curto de uma tecla para o menu.
pub fn key_name(k: KeyCode) -> String {
    use KeyCode::*;
    let fixed = match k {
        Space => "Espaço",
        Enter => "Enter",
        Backspace => "Backspace",
        Tab => "Tab",
        ShiftLeft => "Shift esq.",
        ShiftRight => "Shift dir.",
        ControlLeft => "Ctrl esq.",
        ControlRight => "Ctrl dir.",
        AltLeft => "Alt esq.",
        AltRight => "Alt dir.",
        ArrowUp => "↑",
        ArrowDown => "↓",
        ArrowLeft => "←",
        ArrowRight => "→",
        Escape => "Esc",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.into();
    }
    let s = format!("{k:?}");
    for p in ["Key", "Digit"] {
        if let Some(r) = s.strip_prefix(p) {
            return r.into();
        }
    }
    if let Some(r) = s.strip_prefix("Numpad") {
        return format!("Num {r}");
    }
    s
}

/// Nome de um botão do controle (posição, para valer em qualquer marca).
pub fn pad_name(b: gilrs::Button) -> String {
    use gilrs::Button::*;
    match b {
        South => "Botão de baixo (A Xbox / B Switch)",
        East => "Botão da direita (B Xbox / A Switch)",
        North => "Botão de cima (Y Xbox / X Switch)",
        West => "Botão da esquerda (X Xbox / Y Switch)",
        LeftTrigger => "LB / L",
        RightTrigger => "RB / R",
        LeftTrigger2 => "LT / ZL",
        RightTrigger2 => "RT / ZR",
        Start => "Start / Menu",
        Select => "Select / View",
        Mode => "Guide",
        LeftThumb => "Clique analógico esq.",
        RightThumb => "Clique analógico dir.",
        DPadUp => "Direcional ↑",
        DPadDown => "Direcional ↓",
        DPadLeft => "Direcional ←",
        DPadRight => "Direcional →",
        other => return format!("{other:?}"),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hle::settings::PadLayout;

    fn held(k: &[KeyCode]) -> HashSet<KeyCode> {
        k.iter().copied().collect()
    }

    #[test]
    fn teclas_padrao_viram_botoes_do_hid() {
        let t = Teclado::default();
        let cases: &[(KeyCode, u32)] = &[
            (KeyCode::KeyZ, 1 << 0),
            (KeyCode::Space, 1 << 0),
            (KeyCode::KeyK, 1 << 0),
            (KeyCode::KeyX, 1 << 1),
            (KeyCode::Backspace, 1 << 1),
            (KeyCode::Tab, 1 << 2),
            (KeyCode::ShiftRight, 1 << 2),
            (KeyCode::Enter, 1 << 3),
            (KeyCode::ArrowRight, 1 << 4),
            (KeyCode::ArrowLeft, 1 << 5),
            (KeyCode::ArrowUp, 1 << 6),
            (KeyCode::ArrowDown, 1 << 7),
            (KeyCode::KeyE, 1 << 8),
            (KeyCode::KeyQ, 1 << 9),
            (KeyCode::KeyC, 1 << 10),
            (KeyCode::KeyV, 1 << 11),
        ];
        for &(k, bit) in cases {
            let s = from_keys(&held(&[k]), &t);
            assert_eq!(s.buttons, bit, "{k:?}");
            assert_eq!(s.stick, (0.0, 0.0));
        }
        assert_eq!(from_keys(&held(&[KeyCode::Escape, KeyCode::F1]), &t), Sample::default());
    }

    #[test]
    fn wasd_e_o_circle_pad() {
        let t = Teclado::default();
        let s = from_keys(&held(&[KeyCode::KeyW]), &t);
        assert_eq!(s.stick, (0.0, 1.0));
        let (b, stick, touch) = encode(&s);
        assert_eq!(b, 1 << 30);
        assert_eq!(stick, 156 << 16);
        assert_eq!(touch, 0);
        let (b, stick, _) = encode(&from_keys(&held(&[KeyCode::KeyA]), &t));
        assert_eq!(b, 1 << 29);
        assert_eq!(stick, (-156i16) as u16 as u32);
        // A e D juntos se anulam
        assert_eq!(from_keys(&held(&[KeyCode::KeyA, KeyCode::KeyD]), &t).stick, (0.0, 0.0));
        // diagonal normalizada
        let (_, stick, _) = encode(&from_keys(&held(&[KeyCode::KeyW, KeyCode::KeyD]), &t));
        assert_eq!(stick & 0xffff, 110);
        assert_eq!(stick >> 16, 110);
    }

    #[test]
    fn toque_codificado() {
        let s = Sample { touch: Some((319, 239)), ..Sample::default() };
        let (b, _, t) = encode(&s);
        assert_eq!(b, 1 << 20);
        assert_eq!(t, 319 | 239 << 9 | 1 << 31);
    }

    #[test]
    fn controle_nos_dois_layouts() {
        let mut s = Sample::default();
        add_pad(&mut s, &Controle::preset(PadLayout::Xbox), |b| b == gilrs::Button::South, (0.1, -0.9));
        assert_eq!(s.buttons, 1 << 0);
        assert_eq!(s.stick, (0.0, -0.9)); // x dentro da zona morta
        let mut s = Sample::default();
        add_pad(&mut s, &Controle::preset(PadLayout::Nintendo), |b| b == gilrs::Button::South, (0.0, 0.0));
        assert_eq!(s.buttons, 1 << 1);
    }

    #[test]
    fn nomes_curtos() {
        assert_eq!(key_name(KeyCode::KeyZ), "Z");
        assert_eq!(key_name(KeyCode::Digit1), "1");
        assert_eq!(key_name(KeyCode::Space), "Espaço");
        assert_eq!(key_name(KeyCode::F5), "F5");
        assert_eq!(key_name(KeyCode::Numpad4), "Num 4");
    }
}
