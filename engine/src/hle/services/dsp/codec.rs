//! Decodificadores (PCM8, PCM16, DSP-ADPCM), reamostragem e filtros por fonte — mesma aritmética do HLE do
//! Citra/Azahar (`audio_core/codec.cpp`, `interpolate.cpp`, `hle/filter.cpp`).

use std::collections::VecDeque;

pub type Stereo = [i16; 2];
pub type Buffer = VecDeque<Stereo>;

/// Histórico do decodificador ADPCM (y[n-1], y[n-2]).
#[derive(Clone, Copy, Default, Debug)]
pub struct AdpcmState {
    pub yn1: i16,
    pub yn2: i16,
}

/// Bytes que `count` amostras ocupam na memória.
pub fn bytes_needed(format: u16, channels: usize, count: usize) -> usize {
    match format {
        0 => count * channels,                 // PCM8
        1 => count * channels * 2,             // PCM16
        _ => count.div_ceil(14) * 8,           // ADPCM: quadros de 8 bytes com 14 amostras
    }
}

/// DSP-ADPCM ("GC-ADPCM"): quadros de 8 bytes = cabeçalho (preditor 3 bits | escala 4 bits) + 14 nibbles.
/// Filtro em ponto fixo de 11 bits: `y = (x·2^11 + 0x400 + c1·y1 + c2·y2) >> 11`, saturado em 16 bits.
/// Como no Citra, a saída tem tamanho par (a última amostra de um total ímpar também é decodificada).
pub fn decode_adpcm(data: &[u8], count: usize, coeffs: &[i16; 16], state: &mut AdpcmState) -> Buffer {
    const SIGNED: [i32; 16] = [0, 1, 2, 3, 4, 5, 6, 7, -8, -7, -6, -5, -4, -3, -2, -1];
    let size = if count % 2 == 0 { count } else { count + 1 };
    let mut out: Buffer = VecDeque::from(vec![[0i16; 2]; size]);
    let (mut yn1, mut yn2) = (state.yn1 as i32, state.yn2 as i32);
    let frames = count.div_ceil(14);
    for f in 0..frames {
        let Some(&header) = data.get(f * 8) else { break };
        let scale = 1i32 << (header & 0xf);
        let idx = ((header >> 4) & 7) as usize;
        let (c1, c2) = (coeffs[idx * 2] as i32, coeffs[idx * 2 + 1] as i32);
        let mut dec = |nib: u8| -> i16 {
            let xn = SIGNED[nib as usize] * scale;
            let v = ((xn << 11) + 0x400 + c1 * yn1 + c2 * yn2) >> 11;
            let v = v.clamp(-32768, 32767);
            yn2 = yn1;
            yn1 = v;
            v as i16
        };
        let mut o = f * 14;
        let mut d = f * 8 + 1;
        let mut i = 0;
        while i < 14 && o < count {
            let byte = data.get(d).copied().unwrap_or(0);
            let s1 = dec(byte >> 4);
            out[o] = [s1, s1];
            o += 1;
            let s2 = dec(byte & 0xf);
            out[o] = [s2, s2];
            o += 1;
            d += 1;
            i += 2;
        }
    }
    state.yn1 = yn1 as i16;
    state.yn2 = yn2 as i16;
    out
}

/// PCM8 com sinal: amostra << 8.
pub fn decode_pcm8(channels: usize, data: &[u8], count: usize) -> Buffer {
    let s = |b: u8| ((b as u16) << 8) as i16;
    (0..count)
        .map(|i| if channels == 2 { [s(data[2 * i]), s(data[2 * i + 1])] } else { [s(data[i]); 2] })
        .collect()
}

pub fn decode_pcm16(channels: usize, data: &[u8], count: usize) -> Buffer {
    let s = |i: usize| i16::from_le_bytes([data[2 * i], data[2 * i + 1]]);
    (0..count)
        .map(|i| if channels == 2 { [s(2 * i), s(2 * i + 1)] } else { [s(i); 2] })
        .collect()
}

// ------------------------------------------------------------ reamostragem

/// Estado do interpolador: duas amostras de histórico e a posição fracionária (24 bits fracionários).
#[derive(Clone, Copy, Default, Debug)]
pub struct InterpState {
    pub xn1: Stereo,
    pub xn2: Stereo,
    pub fpos: u64,
}

const SCALE: u64 = 1 << 24;
const MASK: u64 = SCALE - 1;

/// Percorre a entrada em passos de `rate`, consumindo-a; há um atraso de duas amostras (o histórico).
fn step_over(
    st: &mut InterpState,
    input: &mut Buffer,
    rate: f32,
    out: &mut [Stereo; 160],
    oi: &mut usize,
    f: impl Fn(u64, Stereo, Stereo) -> Stereo,
) {
    if input.is_empty() {
        return;
    }
    input.push_front(st.xn1);
    input.push_front(st.xn2);
    let step = (rate * SCALE as f32) as u64;
    let mut fpos = st.fpos;
    let mut ii = 0usize;
    while *oi < out.len() {
        ii = (fpos / SCALE) as usize;
        if ii + 2 >= input.len() {
            ii = input.len() - 2;
            break;
        }
        out[*oi] = f(fpos & MASK, input[ii], input[ii + 1]);
        *oi += 1;
        fpos += step;
    }
    st.xn2 = input[ii];
    st.xn1 = input[ii + 1];
    st.fpos = fpos - ii as u64 * SCALE;
    input.drain(..ii + 2);
}

/// Sem interpolação (sample-and-hold).
pub fn interp_none(st: &mut InterpState, input: &mut Buffer, rate: f32, out: &mut [Stereo; 160], oi: &mut usize) {
    step_over(st, input, rate, out, oi, |_, x0, _| x0);
}

/// Linear com diferença saturada (verificado por fuzzing no Citra); divisão com arredondamento para baixo.
pub fn interp_linear(st: &mut InterpState, input: &mut Buffer, rate: f32, out: &mut [Stereo; 160], oi: &mut usize) {
    step_over(st, input, rate, out, oi, |fr, x0, x1| {
        let mut r = [0i16; 2];
        for c in 0..2 {
            let d = (x1[c] as i64 - x0[c] as i64).clamp(-32768, 32767);
            r[c] = (x0[c] as i64 + ((fr as i64 * d) >> 24)) as i16;
        }
        r
    });
}

// ------------------------------------------------------------ filtros por fonte

/// Filtro de 1ª ordem `H(z) = b0 / (1 - a1 z^-1)`, coeficientes com 15 bits fracionários (guardados em s32).
#[derive(Clone, Copy, Debug)]
pub struct SimpleFilter {
    pub b0: i32,
    pub a1: i32,
    y1: Stereo,
}

impl Default for SimpleFilter {
    /// Passagem direta (b0 = 1,0), como o `Reset` do Citra.
    fn default() -> Self {
        SimpleFilter { b0: 1 << 15, a1: 0, y1: [0; 2] }
    }
}

impl SimpleFilter {
    fn process(&mut self, x: Stereo) -> Stereo {
        let mut y = [0i16; 2];
        for c in 0..2 {
            let t = self.b0.wrapping_mul(x[c] as i32).wrapping_add(self.a1.wrapping_mul(self.y1[c] as i32)) >> 15;
            y[c] = t.clamp(-32768, 32767) as i16;
        }
        self.y1 = y;
        y
    }
}

/// Biquad normalizado, coeficientes com 14 bits fracionários (realimentação já negada pela Nintendo).
#[derive(Clone, Copy, Debug)]
pub struct BiquadFilter {
    pub a1: i32,
    pub a2: i32,
    pub b0: i32,
    pub b1: i32,
    pub b2: i32,
    x1: Stereo,
    x2: Stereo,
    y1: Stereo,
    y2: Stereo,
}

impl Default for BiquadFilter {
    fn default() -> Self {
        BiquadFilter { a1: 0, a2: 0, b0: 1 << 14, b1: 0, b2: 0, x1: [0; 2], x2: [0; 2], y1: [0; 2], y2: [0; 2] }
    }
}

impl BiquadFilter {
    fn process(&mut self, x: Stereo) -> Stereo {
        let mut y = [0i16; 2];
        for c in 0..2 {
            let t = self
                .b0
                .wrapping_mul(x[c] as i32)
                .wrapping_add(self.b1.wrapping_mul(self.x1[c] as i32))
                .wrapping_add(self.b2.wrapping_mul(self.x2[c] as i32))
                .wrapping_add(self.a1.wrapping_mul(self.y1[c] as i32))
                .wrapping_add(self.a2.wrapping_mul(self.y2[c] as i32))
                >> 14;
            y[c] = t.clamp(-32768, 32767) as i16;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Os dois filtros de uma fonte (`SourceFilters`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Filters {
    simple_on: bool,
    biquad_on: bool,
    simple: SimpleFilter,
    biquad: BiquadFilter,
}

impl Filters {
    pub fn enable(&mut self, simple: bool, biquad: bool) {
        self.simple_on = simple;
        self.biquad_on = biquad;
        if !simple {
            self.simple = SimpleFilter::default();
        }
        if !biquad {
            self.biquad = BiquadFilter::default();
        }
    }
    pub fn configure_simple(&mut self, b0: i16, a1: i16) {
        self.simple.b0 = b0 as i32;
        self.simple.a1 = a1 as i32;
    }
    pub fn configure_biquad(&mut self, a2: i16, a1: i16, b2: i16, b1: i16, b0: i16) {
        let q = &mut self.biquad;
        (q.a2, q.a1, q.b2, q.b1, q.b0) = (a2 as i32, a1 as i32, b2 as i32, b1 as i32, b0 as i32);
    }
    pub fn process(&mut self, frame: &mut [Stereo; 160]) {
        if self.simple_on {
            for s in frame.iter_mut() {
                *s = self.simple.process(*s);
            }
        }
        if self.biquad_on {
            for s in frame.iter_mut() {
                *s = self.biquad.process(*s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mono(b: &Buffer) -> Vec<i16> {
        b.iter().map(|s| s[0]).collect()
    }

    /// Valores calculados à mão com a fórmula do DSP: `(nib·escala·2048 + 1024 + c1·y1 + c2·y2) >> 11`.
    #[test]
    fn adpcm_known_values() {
        let mut coeffs = [0i16; 16];
        coeffs[2] = 0x800; // preditor 1: c1 = 1,0
        // quadro 1: preditor 0, escala 1, nibbles 1, 7, -8 (0x8), -1 (0xF)
        // quadro 2: preditor 1, escala 1, nibbles 1 (rampa: y = y1 + 1, com o arredondamento de +0,5)
        let data = [0x00, 0x17, 0x8f, 0, 0, 0, 0, 0, 0x10, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11];
        let mut st = AdpcmState::default();
        let out = decode_adpcm(&data, 28, &coeffs, &mut st);
        let v = mono(&out);
        assert_eq!(&v[..4], &[1, 7, -8, -1]);
        assert_eq!(&v[4..14], &[0; 10]);
        // quadro 2 parte do histórico y1 = 0 (amostra 13): 1, 2, 3, …, 14
        assert_eq!(&v[14..28], &(1..=14).collect::<Vec<i16>>()[..]);
        assert_eq!((st.yn1, st.yn2), (14, 13));
    }

    #[test]
    fn adpcm_scale_history_and_clamp() {
        let mut coeffs = [0i16; 16];
        coeffs[0] = 0x800;
        // escala 2^4: nibble -8 → x = -128; (−128·2048 + 1024) >> 11 = −127,5 → −128 (arredonda para baixo)
        let mut st = AdpcmState::default();
        assert_eq!(mono(&decode_adpcm(&[0x04, 0x80, 0, 0, 0, 0, 0, 0], 2, &[0; 16], &mut st)), vec![-128, 0]);
        // histórico vindo do buffer (adpcm_yn): y1 = 100 → (2048 + 1024 + 2048·100) >> 11 = 101
        let mut st = AdpcmState { yn1: 100, yn2: 0 };
        assert_eq!(decode_adpcm(&[0x00, 0x10, 0, 0, 0, 0, 0, 0], 1, &coeffs, &mut st)[0], [101, 101]);
        // saturação: escala 2^11, nibble 7, y1 = 32000 → 14336 + 32000 → 32767
        let mut st = AdpcmState { yn1: 32000, yn2: 0 };
        assert_eq!(decode_adpcm(&[0x0b, 0x70, 0, 0, 0, 0, 0, 0], 1, &coeffs, &mut st)[0], [32767, 32767]);
        assert_eq!(st.yn1, 32767);
    }

    #[test]
    fn adpcm_odd_count_is_padded() {
        let mut st = AdpcmState::default();
        let out = decode_adpcm(&[0x00, 0x12, 0x34, 0, 0, 0, 0, 0], 3, &[0; 16], &mut st);
        assert_eq!(out.len(), 4);
        assert_eq!(mono(&out), vec![1, 2, 3, 4]);
    }

    #[test]
    fn pcm_decoders() {
        assert_eq!(decode_pcm8(1, &[0x01, 0xff], 2), Buffer::from(vec![[256, 256], [-256, -256]]));
        assert_eq!(decode_pcm16(2, &[1, 0, 0xff, 0xff], 1), Buffer::from(vec![[1, -1]]));
        assert_eq!(bytes_needed(2, 1, 15), 16);
    }

    /// Taxa 1,0: a saída é a entrada com o atraso de duas amostras do histórico.
    #[test]
    fn linear_rate_one_delays_two_samples() {
        let mut input: Buffer = (1..=200).map(|i| [i as i16, -(i as i16)]).collect();
        let mut st = InterpState::default();
        let mut out = [[0i16; 2]; 160];
        let mut oi = 0;
        interp_linear(&mut st, &mut input, 1.0, &mut out, &mut oi);
        assert_eq!(oi, 160);
        assert_eq!(out[0], [0, 0]);
        assert_eq!(out[1], [0, 0]);
        assert_eq!(out[2], [1, -1]);
        assert_eq!(out[159], [158, -158]);
        assert_eq!(input.len(), 41);
        assert_eq!(st.fpos, SCALE);
    }

    /// Taxa 0,5: amostras intermediárias são a média (arredondada para baixo).
    #[test]
    fn linear_half_rate() {
        let mut input: Buffer = [[0, 0], [10, -10], [20, -20], [30, -30]].into_iter().collect();
        let mut st = InterpState::default();
        let mut out = [[0i16; 2]; 160];
        let mut oi = 0;
        interp_linear(&mut st, &mut input, 0.5, &mut out, &mut oi);
        // entrada efetiva: 0 0 | 0 10 20 30
        assert_eq!(&out[..8], &[[0, 0], [0, 0], [0, 0], [0, 0], [0, 0], [5, -5], [10, -10], [15, -15]]);
        assert!(input.is_empty());
    }
}
