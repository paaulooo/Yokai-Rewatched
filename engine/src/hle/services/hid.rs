//! `hid:USER` — botões, direcional analógico e toque, via memória compartilhada (layout do Citra `hid.h`):
//! - pad: ticks @0x00, ticks anteriores @0x08, índice @0x10, slider 3D @0x18, estado @0x1C, analógico cru @0x20,
//!   8 entradas de 0x10 B @0x28 {estado, adicionados, removidos, x s16, y s16}
//! - toque @0xA8: ticks @+0, anteriores @+8, índice @+0x10, 8 entradas de 8 B @+0x18 {x u16, y u16, válido u32}
//!
//! Bits dos botões: A 0, B 1, Select 2, Start 3, → 4, ← 5, ↑ 6, ↓ 7, R 8, L 9, X 10, Y 11, toque 20,
//! analógico → 28, ← 29, ↑ 30, ↓ 31.

use super::{new_event, new_shared, Ipc};
use crate::hle::kernel::{Reset, K};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use yw1_rt::{r32, w16, w32};

/// Estado vindo da janela (thread do host): botões, analógico (x, y em ±156) e toque (x, y, apertado).
pub static BUTTONS: AtomicU32 = AtomicU32::new(0);
pub static STICK: AtomicU32 = AtomicU32::new(0);
pub static TOUCH: AtomicU32 = AtomicU32::new(0);

struct Hid {
    shared: u32,
    addr: u32,
    events: Vec<(u32, usize)>,
    old: u32,
    pad_index: u32,
    touch_index: u32,
}

static HID: Mutex<Option<Hid>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Hid) -> R) -> R {
    let mut g = HID.lock().unwrap();
    f(g.get_or_insert_with(|| Hid {
        shared: new_shared("hid", 0x2b0),
        addr: 0,
        events: (0..5).map(|_| new_event(Reset::OneShot, "hid")).collect(),
        old: 0,
        pad_index: 0,
        touch_index: 0,
    }))
}

pub fn mapped(addr: u32) {
    with(|h| h.addr = addr);
}

pub fn request(ipc: &Ipc) {
    match ipc.cmd() {
        // GetIPCHandles → bloco + 5 eventos (pad 1, pad 2, acelerômetro, giroscópio, debug pad)
        0x000a => {
            let (sh, ev) = with(|h| (h.shared, h.events.iter().map(|e| e.0).collect::<Vec<_>>()));
            ipc.ok(1, 7);
            ipc.set(2, 0x1400_0000);
            ipc.set(3, sh);
            for (i, e) in ev.iter().enumerate() {
                ipc.set(4 + i as u32, *e);
            }
        }
        // Enable/Disable acelerômetro/giroscópio
        0x0011..=0x0014 => ipc.ok(1, 0),
        // GetGyroscopeLowRawToDpsCoefficient → 14.375 (f32)
        0x0015 => {
            ipc.ok(2, 0);
            ipc.set(2, 14.375f32.to_bits());
        }
        // GetGyroscopeLowCalibrateParam → zeros no buffer estático
        0x0016 => {
            let out = ipc.static_buf(0);
            for i in 0..5 {
                w32(out + 4 * i, 0);
            }
            ipc.ok(1, 2);
            ipc.set(2, (20 << 14) | 2);
            ipc.set(3, out);
        }
        // GetSoundVolume
        0x0017 => {
            ipc.ok(2, 0);
            ipc.set(2, 0x3f);
        }
        _ => super::unknown("hid:USER", ipc),
    }
}

/// Chamado a cada atualização de entrada (thread do host, ~60 Hz).
pub fn update(tick: u64) {
    let (addr, evs) = {
        let g = HID.lock().unwrap();
        match g.as_ref() {
            Some(h) if h.addr != 0 => (h.addr, h.events.iter().map(|e| e.1).collect::<Vec<_>>()),
            _ => return,
        }
    };
    let state = BUTTONS.load(Ordering::Relaxed);
    let stick = STICK.load(Ordering::Relaxed);
    let touch = TOUCH.load(Ordering::Relaxed);
    with(|h| {
        let p = addr;
        h.pad_index = (h.pad_index + 1) % 8;
        let i = h.pad_index;
        w32(p + 0x1c, state);
        w32(p + 0x20, stick);
        w32(p + 0x10, i);
        let e = p + 0x28 + 0x10 * i;
        w32(e, state);
        w32(e + 4, (state ^ h.old) & state);
        w32(e + 8, (state ^ h.old) & h.old);
        w32(e + 12, stick);
        if i == 0 {
            w32(p + 8, r32(p));
            w32(p + 12, r32(p + 4));
            w32(p, tick as u32);
            w32(p + 4, (tick >> 32) as u32);
        }
        h.old = state;
        let t = p + 0xa8;
        h.touch_index = (h.touch_index + 1) % 8;
        let j = h.touch_index;
        w32(t + 0x10, j);
        let te = t + 0x18 + 8 * j;
        w16(te, touch & 0x1ff);
        w16(te + 2, (touch >> 9) & 0xff);
        w32(te + 4, (touch >> 31) & 1);
        if j == 0 {
            w32(t + 8, r32(t));
            w32(t + 12, r32(t + 4));
            w32(t, tick as u32);
            w32(t + 4, (tick >> 32) as u32);
        }
    });
    for e in evs.iter().take(2) {
        K.signal_external(*e);
    }
}
