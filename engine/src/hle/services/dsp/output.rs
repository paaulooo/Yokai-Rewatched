//! Saída de som do DSP (cpal): os quadros de 160 amostras a 32728 Hz entram numa fila curta e o callback do
//! dispositivo reamostra (linear) para a taxa dele. A taxa de leitura é ajustada em ±0,5% conforme o nível da
//! fila, para absorver a diferença entre o relógio do DSP (thread do host) e o do dispositivo.
//! Sem dispositivo, segue mudo. `YW_VOLUME` = volume mestre (0–100, padrão 100); `YW_AUDIO=none` não abre o
//! dispositivo.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub const DSP_RATE: f64 = 32728.498;
/// Alvo e teto da fila (amostras a 32728 Hz): ≈40 ms e ≈120 ms.
const TARGET: usize = 1310;
const MAX: usize = 3 * TARGET;

struct Shared {
    queue: Mutex<VecDeque<[i16; 2]>>,
    gain: f32,
    underruns: AtomicU64,
    dropped: AtomicU64,
}

/// `None` = sem saída (desligada ou sem dispositivo).
static OUT: OnceLock<Option<Arc<Shared>>> = OnceLock::new();

fn volume() -> f32 {
    std::env::var("YW_VOLUME")
        .ok()
        .and_then(|v| v.trim().parse::<f32>().ok())
        .map_or(1.0, |v| (v / 100.0).clamp(0.0, 1.0))
}

fn start() -> Option<Arc<Shared>> {
    if std::env::var("YW_AUDIO").is_ok_and(|v| v.eq_ignore_ascii_case("none") || v == "0") {
        eprintln!("[dsp] saída de som desligada (YW_AUDIO)");
        return None;
    }
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::with_capacity(MAX + 160)),
        gain: volume(),
        underruns: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let s2 = shared.clone();
    // o cpal::Stream não é Send: vive numa thread própria, parada para sempre
    std::thread::Builder::new()
        .name("dsp-out".into())
        .spawn(move || match open(s2) {
            Ok((stream, desc)) => {
                let _ = tx.send(Ok(desc));
                let _keep = stream;
                loop {
                    std::thread::park();
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        })
        .ok()?;
    match rx.recv() {
        Ok(Ok(desc)) => {
            eprintln!("[dsp] saída de som: {desc}, volume {:.0}%", shared.gain * 100.0);
            Some(shared)
        }
        Ok(Err(e)) => {
            eprintln!("[dsp] sem saída de som ({e}): seguindo mudo");
            None
        }
        Err(_) => None,
    }
}

fn open(shared: Arc<Shared>) -> Result<(cpal::Stream, String), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("nenhum dispositivo de saída")?;
    let supported = device.default_output_config().map_err(|e| e.to_string())?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let desc = format!(
        "{} · {} Hz · {} canais · {format:?}",
        device.name().unwrap_or_default(),
        config.sample_rate.0,
        config.channels
    );
    let stream = match format {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, shared),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, shared),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, shared),
        other => return Err(format!("formato de amostra {other:?} não suportado")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, desc))
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    let out_ch = config.channels as usize;
    let base = DSP_RATE / config.sample_rate.0 as f64;
    // estado do reamostrador: amostras atual e próxima, fração entre elas
    let mut cur = [0f32; 2];
    let mut next = [0f32; 2];
    let mut frac = 0f64;
    let mut waiting = true; // enchendo a fila (início ou depois de faltar amostra)
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let mut q = shared.queue.lock().unwrap();
                if waiting && q.len() >= TARGET {
                    waiting = false;
                }
                let fill = q.len() as f64;
                let step = base * (1.0 + 0.005 * ((fill - TARGET as f64) / TARGET as f64).clamp(-1.0, 1.0));
                let g = shared.gain / 32768.0;
                for out in data.chunks_mut(out_ch) {
                    let v = if waiting {
                        [0.0, 0.0]
                    } else {
                        let t = frac as f32;
                        [cur[0] + (next[0] - cur[0]) * t, cur[1] + (next[1] - cur[1]) * t]
                    };
                    for (c, o) in out.iter_mut().enumerate() {
                        let s = if out_ch == 1 { (v[0] + v[1]) * 0.5 } else { v[c.min(1)] };
                        *o = T::from_sample((s * g).clamp(-1.0, 1.0));
                    }
                    if waiting {
                        continue;
                    }
                    frac += step;
                    while frac >= 1.0 {
                        frac -= 1.0;
                        cur = next;
                        match q.pop_front() {
                            Some(s) => next = [s[0] as f32, s[1] as f32],
                            None => {
                                shared.underruns.fetch_add(1, Ordering::Relaxed);
                                waiting = true;
                                next = [0.0; 2];
                                break;
                            }
                        }
                    }
                }
            },
            |e| eprintln!("[dsp] erro no stream de áudio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Entrega um quadro mixado ao dispositivo (abre a saída na primeira chamada).
pub fn push(frame: &[[i16; 2]]) {
    let Some(s) = OUT.get_or_init(start) else { return };
    let mut q = s.queue.lock().unwrap();
    q.extend(frame.iter().copied());
    if q.len() > MAX {
        let n = q.len() - TARGET;
        q.drain(..n);
        s.dropped.fetch_add(n as u64, Ordering::Relaxed);
    }
}

/// (fila em ms, faltas de amostra, amostras descartadas) — `None` sem saída.
pub fn stats() -> Option<(f64, u64, u64)> {
    let s = OUT.get()?.as_ref()?;
    let n = s.queue.lock().unwrap().len();
    Some((
        n as f64 * 1000.0 / DSP_RATE,
        s.underruns.load(Ordering::Relaxed),
        s.dropped.load(Ordering::Relaxed),
    ))
}
