//! `y2r:u` — conversão YUV → RGB do hardware (usada pelo decodificador Mobiclip do próprio jogo para os vídeos).
//!
//! Comandos (3dbrew "Y2R Services", nomes confirmados no gravador de IPC do Azahar — data/ipc/ipc_names_confirmed.csv):
//! 0x01 SetInputFormat, 0x03 SetOutputFormat, 0x05 SetRotation, 0x07 SetBlockAlignment, 0x0D SetTransferEndInterrupt,
//! 0x0F GetTransferEndEvent, 0x10/0x11/0x12/0x13 SetSendingY/U/V/YUYV, 0x18 SetReceiving, 0x1A SetInputLineWidth,
//! 0x1C SetInputLines, 0x1E SetCoefficient, 0x20 SetStandardCoefficient, 0x22 SetAlpha, 0x26 StartConversion,
//! 0x27 StopConversion, 0x28 IsBusyConversion, 0x2A PingProcess, 0x2B DriverInitialize, 0x2C DriverFinalize.
//!
//! Conversão como o Citra (`y2r_u.cpp`, `ConvertYUVToRGB`): coeficientes em ponto fixo, arredondamento 0x18,
//! saída em blocos de 8 linhas, alinhamento por linha ou em blocos 8×8 (Morton). [H] rotação (o jogo usa 0).

use super::{new_event, signal, unknown, Ipc};
use crate::hle::gpu::morton8;
use crate::hle::kernel::Reset;
use std::sync::Mutex;

#[derive(Clone, Copy, Default)]
struct Dma {
    addr: u32,
    size: u32,
    unit: u32,
    gap: u32,
}

struct Y2r {
    in_fmt: u32,
    out_fmt: u32,
    rotation: u32,
    block: bool,
    width: u32,
    lines: u32,
    coef: [i32; 8],
    alpha: u32,
    y: Dma,
    u: Dma,
    v: Dma,
    yuyv: Dma,
    dst: Dma,
    event: Option<(u32, usize)>,
}

static Y2R: Mutex<Option<Y2r>> = Mutex::new(None);

const STANDARD: [[i32; 8]; 4] = [
    [0x100, 0x166, 0xB6, 0x58, 0x1C5, -0x166F, 0x10EE, -0x1C5B],
    [0x100, 0x193, 0x77, 0x2F, 0x1DB, -0x1933, 0xA7C, -0x1D51],
    [0x12A, 0x198, 0xD0, 0x64, 0x204, -0x1BDE, 0x10F2, -0x229B],
    [0x12A, 0x1CA, 0x88, 0x36, 0x21C, -0x1F04, 0x99C, -0x2421],
];

fn with<R>(f: impl FnOnce(&mut Y2r) -> R) -> R {
    let mut g = Y2R.lock().unwrap();
    f(g.get_or_insert_with(|| Y2r {
        in_fmt: 0,
        out_fmt: 0,
        rotation: 0,
        block: false,
        width: 0,
        lines: 0,
        coef: STANDARD[0],
        alpha: 0xff,
        y: Dma::default(),
        u: Dma::default(),
        v: Dma::default(),
        yuyv: Dma::default(),
        dst: Dma::default(),
        event: None,
    }))
}

/// Lê `n` bytes de um buffer de envio respeitando unidade de transferência e espaço entre unidades.
fn read_dma(d: &Dma, n: u32) -> Vec<u8> {
    let mut out = vec![0u8; n as usize];
    let unit = if d.unit == 0 { n } else { d.unit };
    let (mut src, mut done) = (d.addr, 0u32);
    while done < n {
        let k = unit.min(n - done);
        yw1_rt::read_bytes(src, &mut out[done as usize..(done + k) as usize]);
        done += k;
        src += k + d.gap;
    }
    out
}

fn write_dma(d: &Dma, data: &[u8]) {
    let n = data.len() as u32;
    let unit = if d.unit == 0 { n } else { d.unit };
    let (mut dst, mut done) = (d.addr, 0u32);
    while done < n {
        let k = unit.min(n - done);
        yw1_rt::write_bytes(dst, &data[done as usize..(done + k) as usize]);
        done += k;
        dst += k + d.gap;
    }
}

fn convert(s: &Y2r) {
    let (w, h) = (s.width, s.lines);
    if w == 0 || h == 0 || s.dst.addr == 0 {
        return;
    }
    // planos de entrada
    let (ys, us, vs, yuyv);
    let (cw, ch) = match s.in_fmt {
        0 | 2 => (w / 2, h),     // 4:2:2 individual
        1 | 3 => (w / 2, h / 2), // 4:2:0 individual
        _ => (w / 2, h),
    };
    let wide = matches!(s.in_fmt, 2 | 3);
    let bpc = if wide { 2 } else { 1 };
    if s.in_fmt == 4 {
        yuyv = read_dma(&s.yuyv, w * h * 2);
        ys = Vec::new();
        us = Vec::new();
        vs = Vec::new();
    } else {
        ys = read_dma(&s.y, w * h * bpc);
        us = read_dma(&s.u, cw * ch * bpc);
        vs = read_dma(&s.v, cw * ch * bpc);
        yuyv = Vec::new();
    }
    let sample = |buf: &[u8], i: u32| -> i32 {
        if wide {
            buf.get((2 * i) as usize).copied().unwrap_or(0) as i32
        } else {
            buf.get(i as usize).copied().unwrap_or(0) as i32
        }
    };
    let bpp = match s.out_fmt {
        0 => 4,
        1 => 3,
        _ => 2,
    };
    let c = s.coef;
    let mut out = vec![0u8; (w * h * bpp) as usize];
    for y in 0..h {
        for x in 0..w {
            let (yy, uu, vv) = if s.in_fmt == 4 {
                let base = ((y * w + (x & !1)) * 2) as usize;
                let g = |k: usize| yuyv.get(base + k).copied().unwrap_or(0) as i32;
                (if x & 1 == 0 { g(0) } else { g(2) }, g(1), g(3))
            } else {
                let ci = (y * ch / h) * cw + x / 2;
                (sample(&ys, y * w + x), sample(&us, ci), sample(&vs, ci))
            };
            let cy = c[0] * yy;
            let mut r = cy + c[1] * vv;
            let mut g = cy - c[2] * vv - c[3] * uu;
            let mut b = cy + c[4] * uu;
            r = (r >> 3) + c[5] + 0x18;
            g = (g >> 3) + c[6] + 0x18;
            b = (b >> 3) + c[7] + 0x18;
            let (r, g, b) = ((r >> 5).clamp(0, 255) as u32, (g >> 5).clamp(0, 255) as u32, (b >> 5).clamp(0, 255) as u32);
            // posição na saída: por linha ou em blocos 8×8 (dentro de cada faixa de 8 linhas)
            let o = if s.block {
                let band = y / 8;
                (band * w * 8 + (x / 8) * 64 + morton8(x & 7, y & 7)) * bpp
            } else {
                (y * w + x) * bpp
            } as usize;
            let a = s.alpha & 0xff;
            match s.out_fmt {
                0 => out[o..o + 4].copy_from_slice(&(r << 24 | g << 16 | b << 8 | a).to_le_bytes()),
                1 => {
                    out[o] = b as u8;
                    out[o + 1] = g as u8;
                    out[o + 2] = r as u8;
                }
                2 => {
                    let v = (r >> 3) << 11 | (g >> 3) << 6 | (b >> 3) << 1 | (a >> 7);
                    out[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
                }
                _ => {
                    let v = (r >> 3) << 11 | (g >> 2) << 5 | (b >> 3);
                    out[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
                }
            }
        }
    }
    let n = (s.dst.size as usize).min(out.len());
    write_dma(&s.dst, &out[..n]);
}

fn dma(ipc: &Ipc) -> Dma {
    Dma {
        addr: ipc.get(1),
        size: ipc.get(2),
        unit: ipc.get(3) & 0xffff,
        gap: ipc.get(4) & 0xffff,
    }
}

pub fn request(ipc: &Ipc) {
    let cmd = ipc.cmd();
    match cmd {
        0x0001 => with(|s| s.in_fmt = ipc.get(1)),
        0x0003 => with(|s| s.out_fmt = ipc.get(1)),
        0x0005 => with(|s| s.rotation = ipc.get(1)),
        0x0007 => with(|s| s.block = ipc.get(1) != 0),
        0x0010 => with(|s| s.y = dma(ipc)),
        0x0011 => with(|s| s.u = dma(ipc)),
        0x0012 => with(|s| s.v = dma(ipc)),
        0x0013 => with(|s| s.yuyv = dma(ipc)),
        0x0018 => with(|s| s.dst = dma(ipc)),
        0x001a => with(|s| s.width = ipc.get(1)),
        0x001c => with(|s| s.lines = ipc.get(1)),
        0x001e => with(|s| {
            for i in 0..4 {
                let wd = ipc.get(1 + i);
                s.coef[2 * i as usize] = (wd & 0xffff) as i16 as i32;
                s.coef[2 * i as usize + 1] = (wd >> 16) as i16 as i32;
            }
        }),
        0x0020 => with(|s| s.coef = STANDARD[(ipc.get(1) & 3) as usize]),
        0x0022 => with(|s| s.alpha = ipc.get(1)),
        0x000d | 0x0024 | 0x0027 | 0x002b | 0x002c => {}
        0x000f => {
            let h = with(|s| s.event.get_or_insert_with(|| new_event(Reset::OneShot, "y2r")).0);
            ipc.ok(1, 2);
            ipc.set(2, 0);
            ipc.set(3, h);
            return;
        }
        0x0026 => {
            let ev = Y2R.lock().unwrap().as_ref().map(|s| {
                convert(s);
                s.event.map(|e| e.1)
            });
            if let Some(Some(id)) = ev {
                signal(id);
            }
        }
        // IsBusyConversion → não; IsFinishedSending*/Receiving → sim; PingProcess → 0
        0x0028 | 0x002a => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
            return;
        }
        0x0014..=0x0017 | 0x0019 => {
            ipc.ok(2, 0);
            ipc.set(2, 1);
            return;
        }
        _ => {
            unknown("y2r:u", ipc);
            return;
        }
    }
    ipc.ok(1, 0);
}
