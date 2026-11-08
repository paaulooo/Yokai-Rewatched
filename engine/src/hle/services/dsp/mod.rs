//! `dsp::DSP` — HLE de áudio do DSP, seguindo o HLE do Citra/Azahar (`audio_core/hle/*` e
//! `core/hle/service/dsp/dsp_dsp.cpp`).
//!
//! - Pipe de áudio (canal 2): o jogo escreve um comando de 4 bytes (0 inicializar, 1 desligar, 2 acordar,
//!   3 dormir); na inicialização o DSP responde com a quantidade (15) e os endereços DSP das 15 estruturas da
//!   memória compartilhada ([`layout::STRUCT_ADDRESSES`]) e sinaliza a interrupção do pipe.
//! - A cada quadro de áudio (160 amostras a 32728 Hz; a thread `dsp` do host chama [`audio_frame`]): lê a região
//!   com o `frame_counter` mais novo, processa as 24 fontes e os mixers, escreve status, amostras finais e
//!   status do DSP na outra região, manda o som para o dispositivo e sinaliza a interrupção do pipe 2.
//!   O `frame_counter` é do jogo (ele incrementa depois de preencher a região); o DSP só lê, como no Citra.
//!
//! RAM do DSP vista pelo ARM: 0x1FF00000 (512 KB); `ConvertProcessAddressFromDspDram(a)` = `(a << 1) + 0x1FF40000`.
//!
//! Variáveis: `YW_VOLUME` (0–100), `YW_AUDIO=none` (sem dispositivo), `YW_DSP_STATS=0` (sem a linha por segundo),
//! `YW_DSP_WAV=arquivo.wav` (grava a mixagem final, 32728 Hz estéreo, para conferir o som sem ouvir).

pub mod codec;
pub mod layout;
pub mod mixer;
pub mod output;
pub mod source;

use super::{new_event, unknown, Ipc};
use crate::hle::kernel::{Reset, K};
use layout::*;
use source::{Frame, QuadFrame, SampleMemory, Source};
use std::sync::Mutex;

pub const DSP_RAM: u32 = 0x1ff0_0000;
pub const DSP_RAM_SIZE: usize = 0x8_0000;
const NUM_PIPES: usize = 8;
const PIPE_AUDIO: usize = 2;
const PIPE_BINARY: usize = 3;
/// Máximo de eventos de interrupção registrados (`max_number_of_interrupt_events` do Citra).
const MAX_EVENTS: usize = 6;
/// Result(InvalidResultValue, DSP, OutOfResource, Status) [H: codificação montada a partir dos enums do Citra]
const RESULT_OUT_OF_EVENTS: u32 = 0xc8a0_a7ff;

// ------------------------------------------------------------ memória

/// A RAM do DSP (512 KB), por deslocamento.
pub trait DspRam {
    fn read(&self, off: usize, out: &mut [u8]);
    fn write(&mut self, off: usize, data: &[u8]);
}

/// RAM do DSP na memória guest (0x1FF00000).
struct GuestDspRam;

impl DspRam for GuestDspRam {
    fn read(&self, off: usize, out: &mut [u8]) {
        yw1_rt::read_bytes(DSP_RAM + off as u32, out);
    }
    fn write(&mut self, off: usize, data: &[u8]) {
        yw1_rt::write_bytes(DSP_RAM + off as u32, data);
    }
}

/// Memória física do console vista pelo DMA do DSP: FCRAM (linear), VRAM e a própria RAM do DSP.
struct GuestMem;

fn pa_to_va(pa: u32) -> Option<u32> {
    if (DSP_RAM..DSP_RAM + DSP_RAM_SIZE as u32).contains(&pa) {
        Some(pa)
    } else {
        crate::hle::mem::pa_to_va(pa)
    }
}

impl SampleMemory for GuestMem {
    fn read(&self, pa: u32, len: usize) -> Option<Vec<u8>> {
        if len == 0 {
            return Some(Vec::new());
        }
        if len > 64 << 20 {
            return None;
        }
        let va = pa_to_va(pa)?;
        let last = pa_to_va(pa.checked_add(len as u32 - 1)?)?;
        if last != va + (len as u32 - 1) || !crate::hle::mem::is_mapped(va) || !crate::hle::mem::is_mapped(last) {
            return None;
        }
        let mut v = vec![0u8; len];
        yw1_rt::read_bytes(va, &mut v);
        Some(v)
    }
}

// ------------------------------------------------------------ núcleo (fontes + mixers)

/// Região que o DSP lê: a de `frame_counter` maior, tratando a volta de 0xFFFF (`CurrentRegionIndex`).
pub fn current_region(fc0: u16, fc1: u16) -> usize {
    if fc0 == 0xffff && fc1 != 0xfffe {
        return 1;
    }
    if fc1 == 0xffff && fc0 != 0xfffe {
        return 0;
    }
    if fc0 > fc1 {
        0
    } else {
        1
    }
}

#[derive(Clone)]
pub struct Core {
    pub sources: Vec<Source>,
    mixers: mixer::Mixers,
}

impl Default for Core {
    fn default() -> Self {
        Core { sources: (0..NUM_SOURCES).map(Source::new).collect(), mixers: mixer::Mixers::default() }
    }
}

impl Core {
    /// Um quadro de áudio completo (`GenerateCurrentFrame`). Devolve a mixagem final.
    pub fn generate_frame(&mut self, ram: &mut dyn DspRam, mem: &dyn SampleMemory) -> Frame {
        let mut fc = [0u8; 4];
        ram.read(REGION0 + FRAME_COUNTER, &mut fc[..2]);
        ram.read(REGION1 + FRAME_COUNTER, &mut fc[2..]);
        let ri = current_region(rd16(&fc, 0), rd16(&fc, 2));
        let (rb, wb) = if ri == 0 { (REGION0, REGION1) } else { (REGION1, REGION0) };

        // cópia da região lida; só os campos que o DSP altera (dirty flags) voltam para ela
        let mut snap = vec![0u8; REGION_SIZE];
        ram.read(rb, &mut snap);

        let mut quads: [QuadFrame; 3] = [[[0; 4]; SAMPLES_PER_FRAME]; 3];
        let mut statuses = [0u8; SOURCE_STATUS_SIZE];
        for (i, s) in self.sources.iter_mut().enumerate() {
            let off = SOURCE_CONFIG + i * SOURCE_CONFIG_ONE;
            let had_dirty = rd32(&snap, off + src::DIRTY) != 0;
            let mut coeffs = [0i16; 16];
            for (k, c) in coeffs.iter_mut().enumerate() {
                *c = rd16(&snap, ADPCM_COEFFS + i * 32 + 2 * k) as i16;
            }
            let st = s.tick(&mut snap[off..off + SOURCE_CONFIG_ONE], &coeffs, mem);
            if had_dirty {
                ram.write(rb + off + src::DIRTY, &snap[off + src::DIRTY..off + src::DIRTY + 4]);
                let b = off + src::BUFFERS_DIRTY;
                ram.write(rb + b, &snap[b..b + 2]);
            }
            st.write(&mut statuses[i * SOURCE_STATUS_ONE..(i + 1) * SOURCE_STATUS_ONE]);
            for (m, q) in quads.iter_mut().enumerate() {
                s.mix_into(q, m);
            }
        }
        ram.write(wb + SOURCE_STATUS, &statuses);

        let had_dirty = rd32(&snap, DSP_CONFIG + cfg::DIRTY) != 0;
        let (a, b) = snap.split_at_mut(INTERMEDIATE);
        let config = &mut a[DSP_CONFIG..DSP_CONFIG + DSP_CONFIG_SIZE];
        let read_inter = &b[..INTERMEDIATE_SIZE];
        let mut write_inter = vec![0u8; INTERMEDIATE_SIZE];
        let wrote = self.mixers.tick(config, read_inter, &mut write_inter, &quads);
        if had_dirty {
            ram.write(rb + DSP_CONFIG + cfg::DIRTY, &config[cfg::DIRTY..cfg::DIRTY + 4]);
            for m in 0..2 {
                let o = cfg::DELAY + m * cfg::DELAY_SIZE + cfg::delay::DIRTY;
                ram.write(rb + DSP_CONFIG + o, &config[o..o + 2]);
            }
        }
        for (m, w) in wrote.iter().enumerate() {
            if *w {
                let o = m * INTERMEDIATE_SIZE / 2;
                ram.write(wb + INTERMEDIATE + o, &write_inter[o..o + INTERMEDIATE_SIZE / 2]);
            }
        }
        // DspStatus: unknown = 0, dropped_frames = 0
        ram.write(wb + DSP_STATUS, &[0; 4]);
        let out = *self.mixers.output();
        let mut fin = [0u8; FINAL_SAMPLES_SIZE];
        for (i, s) in out.iter().enumerate() {
            fin[4 * i..4 * i + 2].copy_from_slice(&s[0].to_le_bytes());
            fin[4 * i + 2..4 * i + 4].copy_from_slice(&s[1].to_le_bytes());
        }
        ram.write(wb + FINAL_SAMPLES, &fin);
        out
    }
}

// ------------------------------------------------------------ serviço

#[derive(Clone, Copy, PartialEq, Debug)]
enum Power {
    Off,
    On,
    Sleeping,
}

#[derive(Default)]
struct Stats {
    frames: u64,
    max_playing: usize,
    playing_mask: u32,
    peak: i32,
    last_print: Option<std::time::Instant>,
}

struct Dsp {
    sem: (u32, usize),
    power: Power,
    pipes: [Vec<u8>; NUM_PIPES],
    sleep_pending: bool,
    sleep_reply_frames: u32,
    int_zero: Option<usize>,
    int_one: Option<usize>,
    int_pipe: [Option<usize>; NUM_PIPES],
    core: Core,
    backup: Option<(Vec<u8>, Core)>,
    stats: Stats,
}

static DSP: Mutex<Option<Dsp>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Dsp) -> R) -> R {
    let mut g = DSP.lock().unwrap();
    f(g.get_or_insert_with(|| Dsp {
        sem: new_event(Reset::OneShot, "dsp-sem"),
        power: Power::Off,
        pipes: Default::default(),
        sleep_pending: false,
        sleep_reply_frames: 0,
        int_zero: None,
        int_one: None,
        int_pipe: [None; NUM_PIPES],
        core: Core::default(),
        backup: None,
        stats: Stats::default(),
    }))
}

impl Dsp {
    /// Enfileira os endereços das estruturas no pipe de áudio; devolve o evento a sinalizar.
    fn write_struct_addresses(&mut self) -> Option<usize> {
        let p = &mut self.pipes[PIPE_AUDIO];
        p.extend_from_slice(&(STRUCT_ADDRESSES.len() as u16).to_le_bytes());
        for a in STRUCT_ADDRESSES {
            p.extend_from_slice(&a.to_le_bytes());
        }
        self.int_pipe[PIPE_AUDIO]
    }

    fn reset_pipes(&mut self) {
        for p in &mut self.pipes {
            p.clear();
        }
    }

    fn finish_pending_sleep(&mut self, ev: &mut Vec<usize>) {
        if !self.sleep_pending {
            return;
        }
        self.sleep_pending = false;
        let mut ram = vec![0u8; DSP_RAM_SIZE];
        GuestDspRam.read(0, &mut ram);
        self.backup = Some((ram, self.core.clone()));
        ev.extend(self.write_struct_addresses());
        self.power = Power::Sleeping;
    }

    /// Escrita no pipe de áudio (`PipeWrite(DspPipe::Audio)`). Devolve os eventos a sinalizar.
    fn audio_pipe_write(&mut self, cmd: u8) -> Vec<usize> {
        let mut ev = Vec::new();
        self.finish_pending_sleep(&mut ev);
        self.sleep_reply_frames = 0;
        match cmd {
            0 => {
                eprintln!("[dsp] inicialização do DSP (HLE de áudio)");
                // como o Citra: estado limpo a cada inicialização (RAM do DSP zerada)
                GuestDspRam.write(0, &vec![0u8; DSP_RAM_SIZE]);
                self.core = Core::default();
                self.reset_pipes();
                ev.extend(self.write_struct_addresses());
                self.power = Power::On;
            }
            1 => {
                eprintln!("[dsp] desligamento do DSP");
                self.power = Power::Off;
            }
            2 => {
                eprintln!("[dsp] DSP acordado");
                if let Some((ram, core)) = self.backup.take() {
                    GuestDspRam.write(0, &ram);
                    self.core = core;
                }
                self.reset_pipes();
                ev.extend(self.write_struct_addresses());
                self.power = Power::On;
            }
            3 => {
                eprintln!("[dsp] DSP dormindo");
                self.sleep_pending = true;
            }
            c => {
                eprintln!("[dsp] transição de estado desconhecida {c}");
                self.power = Power::Off;
            }
        }
        ev
    }

    fn events_registered(&self) -> usize {
        self.int_pipe.iter().flatten().count() + self.int_zero.is_some() as usize + self.int_one.is_some() as usize
    }
}

fn signal_all(ev: Vec<usize>) {
    for e in ev {
        super::signal(e);
    }
}

pub fn request(ipc: &Ipc) {
    match ipc.cmd() {
        // RecvData(registrador 0): 0 = ligado; 1 = desligado/dormindo (ou dormindo em andamento)
        0x0001 => {
            let v = with(|d| {
                if d.sleep_pending || d.sleep_reply_frames > 0 || d.power != Power::On {
                    1
                } else {
                    0
                }
            });
            ipc.ok(2, 0);
            ipc.set(2, v);
        }
        // RecvDataIsReady(registrador 0)
        0x0002 => {
            let v = with(|d| !d.sleep_pending && d.sleep_reply_frames == 0);
            ipc.ok(2, 0);
            ipc.set(2, v as u32);
        }
        // SetSemaphore / ClearSemaphore (nada no HLE)
        0x0007 | 0x0009 => ipc.ok(1, 0),
        // GetSemaphore → u16 do semáforo na palavra 2 (3dbrew "DSP Services"); o HLE não tem semáforo → 0
        0x0008 => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        // ConvertProcessAddressFromDspDram(endereço)
        0x000c => {
            let a = ipc.get(1);
            ipc.ok(2, 0);
            ipc.set(2, (a << 1).wrapping_add(DSP_RAM + 0x4_0000));
        }
        // WriteProcessPipe(canal, tamanho, desc. estático, buffer)
        0x000d => {
            let (ch, size, ptr) = (ipc.get(1) as usize, ipc.get(2) as usize, ipc.get(4));
            let size = size.min((ipc.get(3) >> 14) as usize).min(0x1000);
            let mut buf = vec![0u8; size];
            yw1_rt::read_bytes(ptr, &mut buf);
            ipc.ok(1, 0);
            match ch {
                PIPE_AUDIO => {
                    if buf.len() != 4 {
                        eprintln!("[dsp] pipe de áudio: escrita de {} bytes (esperado 4), ignorada", buf.len());
                    } else {
                        // bytes 2-3 são lixo da pilha do jogo; o serviço real os zera (RE do Citra)
                        let ev = with(|d| d.audio_pipe_write(buf[0]));
                        signal_all(ev);
                    }
                }
                PIPE_BINARY => eprintln!("[dsp] pipe binário (decodificador AAC) não implementado: {buf:02x?}"),
                _ => eprintln!("[dsp] escrita no pipe {ch} não tratada"),
            }
        }
        // ReadPipe(canal, peer, tamanho) / ReadPipeIfPossible(canal, peer, tamanho) → buffer estático 0
        0x000e | 0x0010 => {
            let ch = ipc.get(1) as usize;
            let size = (ipc.get(3) & 0xffff) as usize;
            let data = with(|d| match d.pipes.get_mut(ch) {
                Some(p) if p.len() >= size => p.drain(..size).collect::<Vec<u8>>(),
                _ => Vec::new(),
            });
            if ipc.cmd() == 0x000e && data.len() < size {
                eprintln!("[dsp] ReadPipe({ch}) de {size} bytes sem dados (o hardware travaria)");
            }
            let out = ipc.static_buf(0);
            yw1_rt::write_bytes(out, &data);
            let desc = ((data.len() as u32) << 14) | 2;
            if ipc.cmd() == 0x000e {
                ipc.ok(1, 2);
                ipc.set(2, desc);
                ipc.set(3, out);
            } else {
                ipc.ok(2, 2);
                ipc.set(2, data.len() as u32);
                ipc.set(3, desc);
                ipc.set(4, out);
            }
        }
        // GetPipeReadableSize(canal, peer)
        0x000f => {
            let ch = ipc.get(1) as usize;
            let n = with(|d| d.pipes.get(ch).map_or(0, |p| p.len()));
            ipc.ok(2, 0);
            ipc.set(2, n as u32 & 0xffff);
        }
        // LoadComponent(tamanho, máscaras, desc, buffer) → carregado (o HLE não precisa do firmware)
        0x0011 => {
            let (size, ptr) = (ipc.get(1), ipc.get(5));
            ipc.ok(2, 2);
            ipc.set(2, 1);
            ipc.set(3, (size << 4) | 0xa);
            ipc.set(4, ptr);
        }
        // UnloadComponent / FlushDataCache / InvalidateDataCache / SetSemaphoreMask / ForceHeadphoneOut
        0x0012 | 0x0013 | 0x0014 | 0x0017 | 0x0020 => ipc.ok(1, 0),
        // RegisterInterruptEvents(interrupção, canal, desc, evento): evento 0 = remover
        0x0015 => {
            let (int, ch, ev) = (ipc.get(1), ipc.get(2) as usize, ipc.get(4));
            let id = if ev == 0 { None } else { K.lock().obj_of(ev) };
            let res = with(|d| {
                if int > 2 || ch >= NUM_PIPES {
                    eprintln!("[dsp] RegisterInterruptEvents inválido: interrupção {int}, canal {ch}");
                    return 0;
                }
                if id.is_some() && d.events_registered() >= MAX_EVENTS {
                    return RESULT_OUT_OF_EVENTS;
                }
                let slot = match int {
                    0 => &mut d.int_zero,
                    1 => &mut d.int_one,
                    _ => &mut d.int_pipe[ch],
                };
                *slot = id;
                0
            });
            ipc.reply(1, 0, res);
        }
        // GetSemaphoreEventHandle
        0x0016 => {
            let h = with(|d| d.sem.0);
            ipc.ok(1, 2);
            ipc.set(2, 0);
            ipc.set(3, h);
        }
        // GetHeadphoneStatus: sem fone
        0x001f => {
            ipc.ok(2, 0);
            ipc.set(2, 0);
        }
        _ => unknown("dsp::DSP", ipc),
    }
}

fn stats_enabled() -> bool {
    static S: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *S.get_or_init(|| std::env::var("YW_DSP_STATS").map_or(true, |v| v != "0"))
}

/// Um quadro de áudio passou (thread `dsp` do host, a 32728/160 Hz).
pub fn audio_frame() {
    let mut ev = Vec::new();
    let frame = {
        let mut g = DSP.lock().unwrap();
        let Some(d) = g.as_mut() else { return };
        if d.sleep_pending {
            d.finish_pending_sleep(&mut ev);
            d.sleep_reply_frames = 2;
        } else if d.sleep_reply_frames > 0 {
            d.sleep_reply_frames -= 1;
            if d.sleep_reply_frames == 0 {
                ev.extend(d.int_zero);
            }
        }
        if d.power == Power::On {
            let f = d.core.generate_frame(&mut GuestDspRam, &GuestMem);
            ev.extend(d.int_pipe[PIPE_AUDIO]);
            let st = &mut d.stats;
            st.frames += 1;
            let mut n = 0;
            for s in &d.core.sources {
                if s.playing() {
                    n += 1;
                    st.playing_mask |= 1 << s.id;
                }
            }
            st.max_playing = st.max_playing.max(n);
            for s in &f {
                st.peak = st.peak.max((s[0] as i32).abs()).max((s[1] as i32).abs());
            }
            Some((f, n))
        } else {
            None
        }
    };
    {
        let (r, sv, t8w) = crate::hle::kernel::diag_running();
        if std::env::var_os("YW_DIAG").is_some() { eprintln!("[diag] run {:?} svc {:#x} t8wait {}", r, sv, t8w); }
    }
    for e in ev {
        K.signal_external(e);
    }
    if let Some((f, _)) = frame {
        output::push(&f);
        wav::append(&f);
    }
    if stats_enabled() {
        print_stats();
    }
}

/// Gravação opcional da mixagem final em WAV (`YW_DSP_WAV`); o cabeçalho é atualizado a cada ~1 s.
mod wav {
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::Mutex;

    struct Wav {
        f: std::io::BufWriter<std::fs::File>,
        samples: u32,
    }
    static W: Mutex<Option<Option<Wav>>> = Mutex::new(None);

    fn header(rate: u32, data: u32) -> [u8; 44] {
        let mut h = [0u8; 44];
        h[0..4].copy_from_slice(b"RIFF");
        h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
        h[8..16].copy_from_slice(b"WAVEfmt ");
        h[16..20].copy_from_slice(&16u32.to_le_bytes());
        h[20..22].copy_from_slice(&1u16.to_le_bytes());
        h[22..24].copy_from_slice(&2u16.to_le_bytes());
        h[24..28].copy_from_slice(&rate.to_le_bytes());
        h[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
        h[32..34].copy_from_slice(&4u16.to_le_bytes());
        h[34..36].copy_from_slice(&16u16.to_le_bytes());
        h[36..40].copy_from_slice(b"data");
        h[40..44].copy_from_slice(&data.to_le_bytes());
        h
    }

    pub fn append(frame: &[[i16; 2]]) {
        let mut g = W.lock().unwrap();
        let w = g.get_or_insert_with(|| {
            let path = std::env::var_os("YW_DSP_WAV")?;
            let mut f = std::io::BufWriter::new(std::fs::File::create(&path).ok()?);
            f.write_all(&header(32728, 0)).ok()?;
            eprintln!("[dsp] gravando a mixagem em {}", std::path::Path::new(&path).display());
            Some(Wav { f, samples: 0 })
        });
        let Some(w) = w else { return };
        for s in frame {
            let _ = w.f.write_all(&s[0].to_le_bytes());
            let _ = w.f.write_all(&s[1].to_le_bytes());
        }
        w.samples += frame.len() as u32;
        if w.samples % (160 * 205) == 0 {
            let data = w.samples * 4;
            let _ = w.f.flush();
            let file = w.f.get_mut();
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.write_all(&header(32728, data));
            let _ = file.seek(SeekFrom::End(0));
        }
    }
}

fn print_stats() {
    let now = std::time::Instant::now();
    let mut g = DSP.lock().unwrap();
    let Some(d) = g.as_mut() else { return };
    let st = &mut d.stats;
    let Some(last) = st.last_print else {
        st.last_print = Some(now);
        return;
    };
    if now.duration_since(last).as_secs_f64() < 1.0 {
        return;
    }
    let names = ["PCM8", "PCM16", "ADPCM", "?"];
    let ids: Vec<String> = (0..NUM_SOURCES)
        .filter(|i| st.playing_mask & (1 << i) != 0)
        .map(|i| format!("{i}:{}", names[(d.core.sources[i].state.format & 3) as usize]))
        .collect();
    let db = if st.peak > 0 { 20.0 * (st.peak as f64 / 32768.0).log10() } else { f64::NEG_INFINITY };
    let out = match output::stats() {
        Some((ms, under, drop)) => format!("fila {ms:.0} ms · faltas {under} · descartes {drop}"),
        None => "sem saída".into(),
    };
    let current: usize = d.core.sources.iter().filter(|s| s.playing()).count();
    eprintln!(
        "[dsp] {:?} · quadros {} · fontes tocando {} (máx {} no último s, ids {}) · pico {} ({:.1} dBFS) · {}",
        d.power, st.frames, current, st.max_playing, ids.join(" "), st.peak, db, out
    );
    st.last_print = Some(now);
    st.max_playing = 0;
    st.playing_mask = 0;
    st.peak = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct VecRam(Vec<u8>);
    impl DspRam for VecRam {
        fn read(&self, off: usize, out: &mut [u8]) {
            out.copy_from_slice(&self.0[off..off + out.len()]);
        }
        fn write(&mut self, off: usize, data: &[u8]) {
            self.0[off..off + data.len()].copy_from_slice(data);
        }
    }

    /// FCRAM falsa a partir do endereço físico 0x20000000.
    struct VecMem(Vec<u8>);
    impl SampleMemory for VecMem {
        fn read(&self, pa: u32, len: usize) -> Option<Vec<u8>> {
            let o = pa.checked_sub(0x2000_0000)? as usize;
            self.0.get(o..o + len).map(|s| s.to_vec())
        }
    }

    #[test]
    fn region_choice_and_wraparound() {
        assert_eq!(current_region(5, 4), 0);
        assert_eq!(current_region(4, 5), 1);
        assert_eq!(current_region(0xffff, 0), 1); // a região 1 deu a volta
        assert_eq!(current_region(0, 0xffff), 0);
        assert_eq!(current_region(0xffff, 0xfffe), 0);
    }

    /// Uma fonte PCM16 mono constante (1000) com ganho 1 no mixer 0 e volume mestre 1: depois da rampa de ganho
    /// e do atraso de 2 amostras do interpolador, a saída final é 1000 nos dois canais; ao acabar o buffer a fonte
    /// desliga e o status informa o último buffer.
    #[test]
    fn pcm16_source_end_to_end() {
        let mut ram = VecRam(vec![0; DSP_RAM_SIZE]);
        let n = 400u32;
        let mem = VecMem((0..n).flat_map(|_| 1000i16.to_le_bytes()).collect());
        // região 0 é a mais nova
        wr16(&mut ram.0, REGION0 + FRAME_COUNTER, 3);
        wr16(&mut ram.0, REGION1 + FRAME_COUNTER, 2);
        let c = REGION0 + SOURCE_CONFIG; // fonte 0
        let r = &mut ram.0;
        let dirty = src::ENABLE_DIRTY | src::GAIN0_DIRTY | src::RATE_DIRTY | src::INTERP_DIRTY | src::FORMAT_DIRTY
            | src::MONO_STEREO_DIRTY | src::EMBEDDED_DIRTY;
        wr32(r, c + src::DIRTY, dirty);
        r[c + src::ENABLE] = 1;
        wr32(r, c + src::GAIN, 1f32.to_bits()); // mixer 0, frente esquerda
        wr32(r, c + src::GAIN + 4, 1f32.to_bits()); // mixer 0, frente direita
        wr32(r, c + src::RATE, 1f32.to_bits());
        r[c + src::INTERP] = 1;
        wr16(r, c + src::FLAGS1, (source::FMT_PCM16 << 2) | 1);
        wr32d(r, c + src::PHYS_ADDR, 0x2000_0000);
        wr32d(r, c + src::LENGTH, n);
        wr16(r, c + src::BUFFER_ID, 7);
        let d = REGION0 + DSP_CONFIG;
        wr32(r, d + cfg::DIRTY, cfg::MASTER_VOLUME_DIRTY);
        wr32(r, d + cfg::MASTER_VOLUME, 1f32.to_bits());

        let mut core = Core::default();
        // 1º quadro: o buffer é tirado da fila e o quadro sai mudo (como no Citra: `DequeueBuffer` + return)
        let f1 = core.generate_frame(&mut ram, &mem);
        assert!(f1.iter().all(|s| *s == [0, 0]));
        // dirty flags zeradas na região lida
        assert_eq!(rd32(&ram.0, c + src::DIRTY), 0);
        assert_eq!(rd32(&ram.0, d + cfg::DIRTY), 0);
        // status na região 1
        let s = REGION1 + SOURCE_STATUS;
        assert_eq!(ram.0[s + status::IS_ENABLED], 1);
        assert_eq!(rd16(&ram.0, s + status::CURRENT_BUFFER_ID), 7);
        assert_eq!(rd32d(&ram.0, s + status::BUFFER_POSITION), 0);
        assert_eq!(ram.0[s + status::BUFFER_ID_DIRTY], 0, "buffer embutido não marca current_buffer_id_dirty");
        // 2º quadro (mesma região: o jogo não avançou o contador): atraso de 2 amostras do interpolador
        let f2 = core.generate_frame(&mut ram, &mem);
        assert_eq!(&f2[..3], &[[0, 0], [0, 0], [1000, 1000]]);
        assert!(f2[2..].iter().all(|s| *s == [1000, 1000]));
        assert_eq!(rd32d(&ram.0, s + status::BUFFER_POSITION), 159);
        // amostras finais escritas na região 1 (PCM16 estéreo)
        assert_eq!(rd16(&ram.0, REGION1 + FINAL_SAMPLES + 159 * 4) as i16, 1000);
        assert_eq!(rd16(&ram.0, REGION1 + FINAL_SAMPLES + 159 * 4 + 2) as i16, 1000);
        let f3 = core.generate_frame(&mut ram, &mem);
        assert!(f3.iter().all(|s| *s == [1000, 1000]));
        assert_eq!(rd32d(&ram.0, s + status::BUFFER_POSITION), 319);
        // 4º quadro: as 81 amostras restantes; depois a fila está vazia
        let f4 = core.generate_frame(&mut ram, &mem);
        assert!(f4[..80].iter().all(|s| *s == [1000, 1000]));
        assert!(f4[80..].iter().all(|s| *s == [0, 0]));
        assert_eq!(rd32d(&ram.0, s + status::BUFFER_POSITION), 400);
        assert_eq!(ram.0[s + status::IS_ENABLED], 1);
        // 5º quadro: a fonte desliga e informa o último buffer
        core.generate_frame(&mut ram, &mem);
        assert_eq!(ram.0[s + status::IS_ENABLED], 0);
        assert_eq!(rd16(&ram.0, s + status::LAST_BUFFER_ID), 7);
        assert_eq!(rd16(&ram.0, s + status::CURRENT_BUFFER_ID), 0);
        assert_eq!(ram.0[s + status::BUFFER_ID_DIRTY], 1);
    }

    /// Rampa de ganho: ao mudar o ganho, o quadro vai linearmente do ganho antigo ao novo.
    #[test]
    fn gain_ramp() {
        let mut ram = VecRam(vec![0; DSP_RAM_SIZE]);
        let mem = VecMem((0..2000).flat_map(|_| 1000i16.to_le_bytes()).collect());
        wr16(&mut ram.0, REGION0 + FRAME_COUNTER, 1);
        let c = REGION0 + SOURCE_CONFIG;
        let r = &mut ram.0;
        wr32(r, c + src::DIRTY, src::ENABLE_DIRTY | src::FORMAT_DIRTY | src::EMBEDDED_DIRTY | src::RATE_DIRTY);
        r[c + src::ENABLE] = 1;
        wr32(r, c + src::RATE, 1f32.to_bits());
        wr16(r, c + src::FLAGS1, (source::FMT_PCM16 << 2) | 1);
        wr32d(r, c + src::PHYS_ADDR, 0x2000_0000);
        wr32d(r, c + src::LENGTH, 2000);
        wr32(r, REGION0 + DSP_CONFIG + cfg::DIRTY, cfg::MASTER_VOLUME_DIRTY);
        wr32(r, REGION0 + DSP_CONFIG + cfg::MASTER_VOLUME, 1f32.to_bits());
        let mut core = Core::default();
        core.generate_frame(&mut ram, &mem);
        core.generate_frame(&mut ram, &mem); // ganho 0: mudo
        // agora ganho 1 nos canais da frente do mixer 0
        wr32(&mut ram.0, c + src::GAIN, 1f32.to_bits());
        wr32(&mut ram.0, c + src::GAIN + 4, 1f32.to_bits());
        wr32(&mut ram.0, c + src::DIRTY, src::GAIN0_DIRTY);
        let f = core.generate_frame(&mut ram, &mem);
        assert_eq!(f[0], [0, 0]);
        // 159 · (1/159) em f32 = 0,99999994 → 999 depois do truncamento (mesma conta do Citra)
        assert_eq!(f[159], [999, 999]);
        let mid = 1000.0 * 80.0 / 159.0;
        assert!((f[80][0] as f32 - mid).abs() <= 1.0, "{:?}", f[80]);
        let f = core.generate_frame(&mut ram, &mem);
        assert!(f.iter().all(|s| *s == [1000, 1000]));
    }

    /// Fila de buffers: sai o de menor id; `current_buffer_id_dirty` só para buffers da fila.
    #[test]
    fn buffer_queue_order() {
        let mut ram = VecRam(vec![0; DSP_RAM_SIZE]);
        let mem = VecMem(vec![0; 0x1000]);
        wr16(&mut ram.0, REGION1 + FRAME_COUNTER, 1);
        let c = REGION1 + SOURCE_CONFIG + 5 * SOURCE_CONFIG_ONE; // fonte 5, região 1
        let r = &mut ram.0;
        wr32(r, c + src::DIRTY, src::ENABLE_DIRTY | src::FORMAT_DIRTY | src::BUFFER_QUEUE_DIRTY);
        r[c + src::ENABLE] = 1;
        wr16(r, c + src::FLAGS1, source::FMT_PCM16 << 2 | 1);
        wr16(r, c + src::BUFFERS_DIRTY, 0b11);
        for (i, id) in [(0usize, 9u16), (1, 4)] {
            let b = c + src::BUFFERS + i * src::BUFFER_SIZE;
            wr32d(r, b + src::buf::PHYS_ADDR, 0x2000_0000);
            wr32d(r, b + src::buf::LENGTH, 1000);
            wr16(r, b + src::buf::BUFFER_ID, id);
        }
        let mut core = Core::default();
        core.generate_frame(&mut ram, &mem);
        let s = REGION0 + SOURCE_STATUS + 5 * SOURCE_STATUS_ONE;
        assert_eq!(rd16(&ram.0, s + status::CURRENT_BUFFER_ID), 4);
        assert_eq!(ram.0[s + status::BUFFER_ID_DIRTY], 1);
        assert_eq!(rd16(&ram.0, c + src::BUFFERS_DIRTY), 0);
    }
}
