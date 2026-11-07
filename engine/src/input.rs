//! Entrada: teclado, mouse (como touch da tela de baixo) e gamepads (Xbox/XInput, DualShock/DualSense,
//! Switch Pro e genéricos com mapeamento SDL, via `gilrs`), tudo convertido nos botões do 3DS.
//!
//! Gamepad — padrão PC ("confirmar" no botão de baixo):
//!   A ← Sul (Xbox A / PS ✕)   B ← Leste (Xbox B / PS ○)   X ← Norte (Xbox Y / PS △)   Y ← Oeste (Xbox X / PS □)
//!   L/R ← LB/RB (L1/R1)   ZL/ZR ← LT/RT (L2/R2)   Start ← Start/Options/Menu   Select ← Back/Share/View
//!   Direcional ← D-pad e analógico esquerdo (também lido como Circle Pad).
//!   `YW_PAD_LAYOUT=nintendo` troca A↔B e X↔Y (posição do 3DS: confirmar no botão da direita).
//! Teclado: setas/WASD = direcional · Z/Espaço/K = A · X/Backspace/L = B · C/I = X · V/J = Y ·
//!   Q = L · E = R · Enter = Start · Tab/Shift direito = Select. Mouse esquerdo na tela de baixo = toque.

use crate::screen::{BOTTOM_H, BOTTOM_W, TOP_H, WIN_H, WIN_W};
use gilrs::{Axis, Button as Pad, EventType, Gilrs};
use minifb::{Key, MouseButton, MouseMode, Window};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u16)]
pub enum Button {
    A = 1 << 0,
    B = 1 << 1,
    X = 1 << 2,
    Y = 1 << 3,
    L = 1 << 4,
    R = 1 << 5,
    ZL = 1 << 6,
    ZR = 1 << 7,
    Start = 1 << 8,
    Select = 1 << 9,
    Up = 1 << 10,
    Down = 1 << 11,
    Left = 1 << 12,
    Right = 1 << 13,
}

const KEYS: &[(Key, Button)] = &[
    (Key::Up, Button::Up),
    (Key::W, Button::Up),
    (Key::Down, Button::Down),
    (Key::S, Button::Down),
    (Key::Left, Button::Left),
    (Key::A, Button::Left),
    (Key::Right, Button::Right),
    (Key::D, Button::Right),
    (Key::Z, Button::A),
    (Key::Space, Button::A),
    (Key::K, Button::A),
    (Key::X, Button::B),
    (Key::Backspace, Button::B),
    (Key::L, Button::B),
    (Key::C, Button::X),
    (Key::I, Button::X),
    (Key::V, Button::Y),
    (Key::J, Button::Y),
    (Key::Q, Button::L),
    (Key::E, Button::R),
    (Key::Enter, Button::Start),
    (Key::Tab, Button::Select),
    (Key::RightShift, Button::Select),
];

const STICK_DEADZONE: f32 = 0.5;

pub struct Input {
    gilrs: Option<Gilrs>,
    nintendo_layout: bool,
    held: u16,
    prev: u16,
    touch: Option<(i32, i32)>,
    prev_touch: Option<(i32, i32)>,
    /// Circle Pad (analógico esquerdo), -1..1, y para cima.
    pub circle: (f32, f32),
    /// Texto digitado neste quadro (tela de nome): letras, dígitos, espaço; '\u{8}' = apagar.
    pub typed: Vec<char>,
}

impl Input {
    pub fn new() -> Input {
        let gilrs = match Gilrs::new() {
            Ok(g) => {
                for (_, pad) in g.gamepads() {
                    eprintln!("[yw1] controle: {}", pad.name());
                }
                Some(g)
            }
            Err(e) => {
                eprintln!("[yw1] gamepads indisponíveis: {e}");
                None
            }
        };
        let nintendo_layout =
            std::env::var("YW_PAD_LAYOUT").is_ok_and(|v| v.eq_ignore_ascii_case("nintendo"));
        Input {
            gilrs,
            nintendo_layout,
            held: 0,
            prev: 0,
            touch: None,
            prev_touch: None,
            circle: (0.0, 0.0),
            typed: Vec::new(),
        }
    }

    /// Entrada sem janela nem gamepads (testes).
    #[cfg(test)]
    pub fn headless() -> Input {
        Input {
            gilrs: None,
            nintendo_layout: false,
            held: 0,
            prev: 0,
            touch: None,
            prev_touch: None,
            circle: (0.0, 0.0),
            typed: Vec::new(),
        }
    }

    /// Avança um quadro com só `b` no estado `down` (testes).
    #[cfg(test)]
    pub fn set(&mut self, b: Button, down: bool) {
        self.prev = self.held;
        self.held = if down { b as u16 } else { 0 };
    }

    pub fn update(&mut self, window: &Window) {
        self.prev = self.held;
        self.prev_touch = self.touch;
        let mut held = 0u16;
        for &(key, button) in KEYS {
            if window.is_key_down(key) {
                held |= button as u16;
            }
        }
        self.circle = (0.0, 0.0);
        if let Some(g) = &mut self.gilrs {
            while let Some(ev) = g.next_event() {
                match ev.event {
                    EventType::Connected => {
                        eprintln!("[yw1] controle conectado: {}", g.gamepad(ev.id).name())
                    }
                    EventType::Disconnected => eprintln!("[yw1] controle desconectado"),
                    _ => {}
                }
            }
            let (confirm, back, top, left) = if self.nintendo_layout {
                (Pad::East, Pad::South, Pad::West, Pad::North)
            } else {
                (Pad::South, Pad::East, Pad::North, Pad::West)
            };
            let map = [
                (confirm, Button::A),
                (back, Button::B),
                (top, Button::X),
                (left, Button::Y),
                (Pad::LeftTrigger, Button::L),
                (Pad::RightTrigger, Button::R),
                (Pad::LeftTrigger2, Button::ZL),
                (Pad::RightTrigger2, Button::ZR),
                (Pad::Start, Button::Start),
                (Pad::Select, Button::Select),
                (Pad::DPadUp, Button::Up),
                (Pad::DPadDown, Button::Down),
                (Pad::DPadLeft, Button::Left),
                (Pad::DPadRight, Button::Right),
            ];
            for (_, pad) in g.gamepads() {
                for (p, b) in map {
                    if pad.is_pressed(p) {
                        held |= b as u16;
                    }
                }
                let (x, y) = (pad.value(Axis::LeftStickX), pad.value(Axis::LeftStickY));
                if x.abs() > self.circle.0.abs() {
                    self.circle.0 = x;
                }
                if y.abs() > self.circle.1.abs() {
                    self.circle.1 = y;
                }
            }
        }
        let (cx, cy) = self.circle;
        if cy > STICK_DEADZONE {
            held |= Button::Up as u16;
        }
        if cy < -STICK_DEADZONE {
            held |= Button::Down as u16;
        }
        if cx < -STICK_DEADZONE {
            held |= Button::Left as u16;
        }
        if cx > STICK_DEADZONE {
            held |= Button::Right as u16;
        }
        self.held = held;
        self.typed.clear();
        let shift = window.is_key_down(Key::LeftShift) || window.is_key_down(Key::RightShift);
        for k in window.get_keys_pressed(minifb::KeyRepeat::Yes) {
            let c = match k {
                Key::Backspace => Some('\u{8}'),
                Key::Space => Some(' '),
                _ => {
                    let n = format!("{k:?}");
                    match n.len() {
                        1 => n
                            .chars()
                            .next()
                            .map(|c| if shift { c } else { c.to_ascii_lowercase() }),
                        4 if n.starts_with("Key") => n.chars().nth(3),
                        _ => None,
                    }
                }
            };
            self.typed.extend(c);
        }
        self.touch = if window.get_mouse_down(MouseButton::Left) {
            bottom_screen_point(window)
        } else {
            None
        };
    }

    /// Algum botão foi apertado neste quadro (o `op1000` dos scripts, interpretado).
    pub fn any_pressed(&self) -> bool {
        self.held & !self.prev != 0
    }

    /// Botão mantido apertado neste quadro.
    pub fn down(&self, b: Button) -> bool {
        self.held & b as u16 != 0
    }

    pub fn pressed(&self, b: Button) -> bool {
        self.held & !self.prev & b as u16 != 0
    }

    /// Ponto tocado agora (coordenadas da tela de baixo, 320×240).
    pub fn touch(&self) -> Option<(i32, i32)> {
        self.touch
    }

    /// Onde o toque foi solto neste quadro.
    pub fn touch_released(&self) -> Option<(i32, i32)> {
        if self.touch.is_none() {
            self.prev_touch
        } else {
            None
        }
    }
}

/// Converte o mouse (janela redimensionável, escala com proporção mantida e centralizada) para a tela de baixo.
fn bottom_screen_point(window: &Window) -> Option<(i32, i32)> {
    let (mx, my) = window.get_unscaled_mouse_pos(MouseMode::Discard)?;
    let (ww, wh) = window.get_size();
    let s = (ww as f32 / WIN_W as f32).min(wh as f32 / WIN_H as f32);
    let ox = (ww as f32 - WIN_W as f32 * s) / 2.0;
    let oy = (wh as f32 - WIN_H as f32 * s) / 2.0;
    let x = ((mx - ox) / s) as i32 - ((WIN_W - BOTTOM_W) / 2) as i32;
    let y = ((my - oy) / s) as i32 - TOP_H as i32;
    ((0..BOTTOM_W as i32).contains(&x) && (0..BOTTOM_H as i32).contains(&y)).then_some((x, y))
}
