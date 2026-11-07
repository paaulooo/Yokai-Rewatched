//! Janela do jogo recompilado: as duas telas (400×240 em cima, 320×240 embaixo, centralizada) e a entrada.
//! Por enquanto apresenta o framebuffer guest com `minifb` (o backend Vulkan/DX12 vem no marco de renderização).
//!
//! Teclado (mesmo do motor antigo): setas = direcional · WASD = analógico (Circle Pad) · Z/Espaço/K = A ·
//! X/Backspace/L = B · C/I = X · V/J = Y · Q = L · E = R · Enter = Start · Tab/Shift direito = Select · Esc sai.
//! Mouse esquerdo na tela de baixo = toque. Gamepads (Xbox/PS/Switch/genéricos, `gilrs`): confirmar no botão de baixo
//! (`YW_PAD_LAYOUT=nintendo` troca A↔B e X↔Y); analógico esquerdo = Circle Pad.
//! Tamanho: `YW_SCALE=1|2|4` (padrão 2).

use super::services::hid::{BUTTONS, STICK, TOUCH};
use super::{crash_message, gpu};
use gilrs::{Axis, Button as Pad, Gilrs};
use minifb::{Key, MouseButton, MouseMode, Scale, Window, WindowOptions};
use std::sync::atomic::Ordering;

const W: usize = 400;
const H: usize = 480;

// bits do HID do 3DS
const A: u32 = 1 << 0;
const B: u32 = 1 << 1;
const SELECT: u32 = 1 << 2;
const START: u32 = 1 << 3;
const RIGHT: u32 = 1 << 4;
const LEFT: u32 = 1 << 5;
const UP: u32 = 1 << 6;
const DOWN: u32 = 1 << 7;
const R: u32 = 1 << 8;
const L: u32 = 1 << 9;
const X: u32 = 1 << 10;
const Y: u32 = 1 << 11;
const ZL: u32 = 1 << 14;
const ZR: u32 = 1 << 15;
const TOUCH_BIT: u32 = 1 << 20;
const C_RIGHT: u32 = 1 << 28;
const C_LEFT: u32 = 1 << 29;
const C_UP: u32 = 1 << 30;
const C_DOWN: u32 = 1 << 31;

const KEYS: &[(Key, u32)] = &[
    (Key::Up, UP),
    (Key::Down, DOWN),
    (Key::Left, LEFT),
    (Key::Right, RIGHT),
    (Key::Z, A),
    (Key::Space, A),
    (Key::K, A),
    (Key::X, B),
    (Key::Backspace, B),
    (Key::L, B),
    (Key::C, X),
    (Key::I, X),
    (Key::V, Y),
    (Key::J, Y),
    (Key::Q, L),
    (Key::E, R),
    (Key::Enter, START),
    (Key::Tab, SELECT),
    (Key::RightShift, SELECT),
];

/// Valor máximo do Circle Pad no HID (±156, como o Citra).
const STICK_MAX: f32 = 156.0;

pub fn run() {
    let scale = match std::env::var("YW_SCALE").ok().as_deref() {
        Some("1") => Scale::X1,
        Some("4") => Scale::X4,
        _ => Scale::X2,
    };
    let mut win = match Window::new(
        "Yo-kai Watch — recompilado (Esc sai)",
        W,
        H,
        WindowOptions {
            scale,
            ..WindowOptions::default()
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[janela] não consegui abrir a janela: {e}");
            return;
        }
    };
    win.set_target_fps(60);
    let mut gil = Gilrs::new().ok();
    let nintendo = std::env::var("YW_PAD_LAYOUT").is_ok_and(|v| v.eq_ignore_ascii_case("nintendo"));
    let mut buf = vec![0u32; W * H];
    let mut crashed = false;
    while win.is_open() && !win.is_key_down(Key::Escape) {
        // entrada
        let mut b = 0u32;
        for &(k, bit) in KEYS {
            if win.is_key_down(k) {
                b |= bit;
            }
        }
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        if win.is_key_down(Key::A) {
            sx -= 1.0;
        }
        if win.is_key_down(Key::D) {
            sx += 1.0;
        }
        if win.is_key_down(Key::W) {
            sy += 1.0;
        }
        if win.is_key_down(Key::S) {
            sy -= 1.0;
        }
        if let Some(g) = gil.as_mut() {
            while g.next_event().is_some() {}
            for (_, pad) in g.gamepads() {
                let p = |x: Pad| pad.is_pressed(x);
                let (south, east, north, west) = (p(Pad::South), p(Pad::East), p(Pad::North), p(Pad::West));
                let (a, bb, xx, yy) = if nintendo { (east, south, west, north) } else { (south, east, north, west) };
                for (on, bit) in [
                    (a, A),
                    (bb, B),
                    (xx, X),
                    (yy, Y),
                    (p(Pad::LeftTrigger), L),
                    (p(Pad::RightTrigger), R),
                    (p(Pad::LeftTrigger2), ZL),
                    (p(Pad::RightTrigger2), ZR),
                    (p(Pad::Start), START),
                    (p(Pad::Select), SELECT),
                    (p(Pad::DPadUp), UP),
                    (p(Pad::DPadDown), DOWN),
                    (p(Pad::DPadLeft), LEFT),
                    (p(Pad::DPadRight), RIGHT),
                ] {
                    if on {
                        b |= bit;
                    }
                }
                let ax = pad.value(Axis::LeftStickX);
                let ay = pad.value(Axis::LeftStickY);
                if ax.abs() > 0.2 {
                    sx = ax;
                }
                if ay.abs() > 0.2 {
                    sy = ay;
                }
            }
        }
        let len = (sx * sx + sy * sy).sqrt();
        if len > 1.0 {
            sx /= len;
            sy /= len;
        }
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
        STICK.store(cx | cy << 16, Ordering::Relaxed);
        // toque: mouse na tela de baixo (x 40..360, y 240..480 da janela lógica)
        let mut touch = 0u32;
        if win.get_mouse_down(MouseButton::Left) {
            if let Some((mx, my)) = win.get_mouse_pos(MouseMode::Discard) {
                let (tx, ty) = (mx as i32 - 40, my as i32 - 240);
                if (0..320).contains(&tx) && (0..240).contains(&ty) {
                    touch = tx as u32 | (ty as u32) << 9 | 1 << 31;
                    b |= TOUCH_BIT;
                }
            }
        }
        TOUCH.store(touch, Ordering::Relaxed);
        BUTTONS.store(b, Ordering::Relaxed);

        // telas
        buf.fill(0);
        if let Some((w, h, px)) = gpu::screen_rgba(0) {
            blit(&mut buf, &px, w as usize, h as usize, 0, 0);
        }
        if let Some((w, h, px)) = gpu::screen_rgba(1) {
            blit(&mut buf, &px, w as usize, h as usize, 40, 240);
        }
        if !crashed {
            if let Some(m) = crash_message() {
                crashed = true;
                win.set_title(&format!("Yo-kai Watch — PAROU: {m}"));
            }
        }
        if win.update_with_buffer(&buf, W, H).is_err() {
            break;
        }
    }
}

fn blit(dst: &mut [u32], px: &[u8], w: usize, h: usize, ox: usize, oy: usize) {
    for y in 0..h.min(H - oy) {
        for x in 0..w.min(W - ox) {
            let i = (y * w + x) * 4;
            dst[(oy + y) * W + ox + x] = (px[i] as u32) << 16 | (px[i + 1] as u32) << 8 | px[i + 2] as u32;
        }
    }
}
